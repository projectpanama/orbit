use std::collections::HashMap;

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols::ext::workspace::v1::server::{
            ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1, GroupCapabilities},
            ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1, WorkspaceCapabilities},
            ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
        },
        wayland_server::{
            Client, DataInit, DisplayHandle, New, Resource,
            backend::{ClientId, ObjectId},
            protocol::wl_output::WlOutput,
        },
    },
    wayland::{Dispatch2, GlobalDispatch2},
};

use super::privileged;
use orbit_core::{
    ids::{OutputId, WorkspaceId},
    workspaces::Workspaces,
};

use crate::{state::Orbit, workspaces::WorkspaceTarget};

const VERSION: u32 = 1;

/// One ordered protocol update for a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Announces an output group.
    GroupAdded(OutputId),
    /// Announces a workspace; position, state, and group membership follow.
    WorkspaceAdded(WorkspaceId),
    /// Updates a workspace's position and coordinates.
    Position(WorkspaceId, usize),
    /// Updates whether a workspace is active.
    Active(WorkspaceId, bool),
    /// Removes a workspace from an output group.
    Leave(OutputId, WorkspaceId),
    /// Adds a workspace to an output group.
    Enter(OutputId, WorkspaceId),
    /// Removes a workspace object.
    WorkspaceRemoved(WorkspaceId),
    /// Removes an output group.
    GroupRemoved(OutputId),
}

/// Workspace state last sent to one client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    /// Stable workspace identifier.
    pub id: WorkspaceId,
    /// Zero-based position on the output.
    pub index: usize,
    /// Whether the workspace is currently visible.
    pub active: bool,
}

/// Workspace state for one output group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSnapshot {
    /// Stable output identifier.
    pub output: OutputId,
    /// Workspaces belonging to the output, in order.
    pub workspaces: Vec<WorkspaceSnapshot>,
}

/// Complete workspace state last sent to a client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Output groups in model order.
    pub groups: Vec<GroupSnapshot>,
}

impl Snapshot {
    /// Builds a snapshot from the workspace model.
    pub fn of<W: Copy + Eq + std::hash::Hash>(model: &Workspaces<W>) -> Self {
        let groups = model
            .output_ids()
            .map(|output| {
                let active = model.active(output);
                GroupSnapshot {
                    output,
                    workspaces: model
                        .workspace_ids(output)
                        .enumerate()
                        .map(|(index, id)| WorkspaceSnapshot {
                            id,
                            index,
                            active: Some(id) == active,
                        })
                        .collect(),
                }
            })
            .collect();
        Self { groups }
    }

    /// Iterates over workspaces paired with their output group.
    fn workspaces(&self) -> impl Iterator<Item = (OutputId, WorkspaceSnapshot)> + '_ {
        self.groups
            .iter()
            .flat_map(|group| group.workspaces.iter().map(|ws| (group.output, *ws)))
    }

    /// Returns whether a snapshot contains an output group.
    fn has_group(&self, output: OutputId) -> bool {
        self.groups.iter().any(|group| group.output == output)
    }
}

/// One client's bound manager and its protocol objects.
struct Manager {
    resource: ExtWorkspaceManagerV1,
    /// Snapshot most recently sent to this client.
    sent: Snapshot,
    /// Group resources indexed by output.
    groups: HashMap<OutputId, ExtWorkspaceGroupHandleV1>,
    /// Workspace resources indexed by workspace ID.
    workspaces: HashMap<WorkspaceId, ExtWorkspaceHandleV1>,
    /// Activation requests waiting for the next manager commit.
    pending: Vec<WorkspaceId>,
}

impl Manager {
    /// Applies changes and updates the client's protocol objects.
    ///
    /// `outputs` resolves output IDs for `output_enter` events.
    fn send(
        &mut self,
        changes: &[Change],
        dh: &DisplayHandle,
        outputs: &HashMap<OutputId, Output>,
    ) {
        let Ok(client) = dh.get_client(self.resource.id()) else {
            return;
        };
        let manager = self.resource.id();
        for &change in changes {
            match change {
                Change::GroupAdded(output) => {
                    let Ok(group) = client.create_resource::<ExtWorkspaceGroupHandleV1, _, Orbit>(
                        dh,
                        self.resource.version(),
                        GroupData,
                    ) else {
                        continue;
                    };
                    self.resource.workspace_group(&group);
                    group.capabilities(GroupCapabilities::empty());
                    if let Some(output) = outputs.get(&output) {
                        for wl_output in output.client_outputs(&client) {
                            group.output_enter(&wl_output);
                        }
                    }
                    self.groups.insert(output, group);
                }
                Change::WorkspaceAdded(id) => {
                    let Ok(workspace) = client.create_resource::<ExtWorkspaceHandleV1, _, Orbit>(
                        dh,
                        self.resource.version(),
                        WorkspaceData {
                            id,
                            manager: manager.clone(),
                        },
                    ) else {
                        continue;
                    };
                    self.resource.workspace(&workspace);
                    workspace.capabilities(WorkspaceCapabilities::Activate);
                    self.workspaces.insert(id, workspace);
                }
                Change::Position(id, index) => {
                    if let Some(workspace) = self.workspaces.get(&id) {
                        workspace.name((index + 1).to_string());
                        let coordinate = u32::try_from(index).unwrap_or(u32::MAX);
                        workspace.coordinates(coordinate.to_ne_bytes().to_vec());
                    }
                }
                Change::Active(id, active) => {
                    if let Some(workspace) = self.workspaces.get(&id) {
                        workspace.state(if active {
                            ext_workspace_handle_v1::State::Active
                        } else {
                            ext_workspace_handle_v1::State::empty()
                        });
                    }
                }
                Change::Leave(output, id) => {
                    if let (Some(group), Some(workspace)) =
                        (self.groups.get(&output), self.workspaces.get(&id))
                    {
                        group.workspace_leave(workspace);
                    }
                }
                Change::Enter(output, id) => {
                    if let (Some(group), Some(workspace)) =
                        (self.groups.get(&output), self.workspaces.get(&id))
                    {
                        group.workspace_enter(workspace);
                    }
                }
                Change::WorkspaceRemoved(id) => {
                    if let Some(workspace) = self.workspaces.remove(&id) {
                        workspace.removed();
                    }
                    self.pending.retain(|&pending| pending != id);
                }
                Change::GroupRemoved(output) => {
                    if let Some(group) = self.groups.remove(&output) {
                        group.removed();
                    }
                }
            }
        }
    }
}

/// State for the ext-workspace global and all bound clients.
pub struct ExtWorkspaceState {
    /// Bound client managers.
    managers: Vec<Manager>,
    /// Workspace-model generation last synchronized to clients.
    synced: Option<u64>,
}

impl ExtWorkspaceState {
    /// Creates the state and optionally advertises the global.
    pub fn new(dh: &DisplayHandle, enabled: bool) -> Self {
        dh.create_global::<Orbit, ExtWorkspaceManagerV1, _>(VERSION, ManagerGlobalData { enabled });
        Self {
            managers: Vec::new(),
            synced: None,
        }
    }

    /// Finds a bound manager by protocol object ID.
    fn manager_mut(&mut self, id: &ObjectId) -> Option<&mut Manager> {
        self.managers.iter_mut().find(|m| m.resource.id() == *id)
    }
}

impl Orbit {
    /// Synchronizes changed workspace state with all bound clients.
    pub fn refresh_ext_workspaces(&mut self) {
        let generation = self.workspaces.generation();
        if self.desktop.workspaces.managers.is_empty()
            || self.desktop.workspaces.synced == Some(generation)
        {
            return;
        }
        self.desktop.workspaces.synced = Some(generation);
        let snapshot = Snapshot::of(&self.workspaces);
        let outputs = self.outputs_by_id();
        for manager in &mut self.desktop.workspaces.managers {
            let changes = diff(&manager.sent, &snapshot);
            if changes.is_empty() {
                continue;
            }
            manager.send(&changes, &self.display_handle, &outputs);
            manager.resource.done();
            manager.sent = snapshot.clone();
        }
    }

    /// Announces an output group to a client that bound its `wl_output`.
    pub fn ext_workspace_output_bound(&mut self, output: &Output, wl_output: &WlOutput) {
        let Some(id) = self.output_id(output) else {
            return;
        };
        let client = wl_output.client();
        for manager in &self.desktop.workspaces.managers {
            if manager.resource.client() != client {
                continue;
            }
            if let Some(group) = manager.groups.get(&id) {
                group.output_enter(wl_output);
                manager.resource.done();
            }
        }
    }

    /// Builds a lookup from stable output IDs to Smithay outputs.
    fn outputs_by_id(&self) -> HashMap<OutputId, Output> {
        self.outputs
            .iter()
            .map(|(output, state)| (state.id, output.clone()))
            .collect()
    }

    /// Applies queued workspace activations in request order.
    fn commit_ext_workspace(&mut self, manager: &ObjectId) {
        let Some(manager) = self.desktop.workspaces.manager_mut(manager) else {
            return;
        };
        let pending = std::mem::take(&mut manager.pending);
        for workspace in pending {
            let Some((output, index)) = self.workspaces.find_workspace(workspace) else {
                continue;
            };
            if let Some(output) = self.output_by_id(output) {
                self.switch_workspace(&output, WorkspaceTarget::Index(index));
            }
        }
    }
}

/// Data associated with the ext-workspace global.
pub struct ManagerGlobalData {
    /// Whether clients may bind the global.
    enabled: bool,
}

impl GlobalDispatch2<ExtWorkspaceManagerV1, Orbit> for ManagerGlobalData {
    fn bind(
        &self,
        state: &mut Orbit,
        handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        data_init: &mut DataInit<'_, Orbit>,
    ) {
        let resource = data_init.init(resource, ManagerData);
        let mut manager = Manager {
            resource,
            sent: Snapshot::default(),
            groups: HashMap::new(),
            workspaces: HashMap::new(),
            pending: Vec::new(),
        };
        let snapshot = Snapshot::of(&state.workspaces);
        let changes = diff(&manager.sent, &snapshot);
        manager.send(&changes, handle, &state.outputs_by_id());
        manager.resource.done();
        manager.sent = snapshot;
        state.desktop.workspaces.managers.push(manager);
    }

    fn can_view(&self, client: &Client) -> bool {
        self.enabled && privileged(client)
    }
}

/// Data associated with a bound manager resource.
pub struct ManagerData;

impl Dispatch2<ExtWorkspaceManagerV1, Orbit> for ManagerData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        resource: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Orbit>,
    ) {
        match request {
            ext_workspace_manager_v1::Request::Commit => {
                state.commit_ext_workspace(&resource.id());
            }
            ext_workspace_manager_v1::Request::Stop => {
                resource.finished();
                state
                    .desktop
                    .workspaces
                    .managers
                    .retain(|m| m.resource != *resource);
            }
            _ => {}
        }
    }

    fn destroyed(&self, state: &mut Orbit, _client: ClientId, resource: &ExtWorkspaceManagerV1) {
        state
            .desktop
            .workspaces
            .managers
            .retain(|m| m.resource != *resource);
    }
}

/// Data associated with a workspace-group resource.
pub struct GroupData;

impl Dispatch2<ExtWorkspaceGroupHandleV1, Orbit> for GroupData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        resource: &ExtWorkspaceGroupHandleV1,
        request: ext_workspace_group_handle_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Orbit>,
    ) {
        match request {
            // Not offered so ignored, as the protocol asks.
            ext_workspace_group_handle_v1::Request::CreateWorkspace { .. } => {}
            ext_workspace_group_handle_v1::Request::Destroy => {
                for manager in &mut state.desktop.workspaces.managers {
                    manager.groups.retain(|_, group| group != resource);
                }
            }
            _ => {}
        }
    }
}

/// Data associated with a workspace resource.
pub struct WorkspaceData {
    /// Stable workspace identifier.
    id: WorkspaceId,
    /// Manager resource that announced this workspace.
    manager: ObjectId,
}

impl WorkspaceData {
    /// Returns the stable workspace identifier.
    pub fn id(&self) -> WorkspaceId {
        self.id
    }
}

impl Dispatch2<ExtWorkspaceHandleV1, Orbit> for WorkspaceData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Orbit>,
    ) {
        match request {
            ext_workspace_handle_v1::Request::Activate => {
                if let Some(manager) = state.desktop.workspaces.manager_mut(&self.manager) {
                    manager.pending.push(self.id);
                }
            }
            ext_workspace_handle_v1::Request::Destroy => {
                if let Some(manager) = state.desktop.workspaces.manager_mut(&self.manager) {
                    manager.workspaces.retain(|_, ws| ws != resource);
                }
            }
            // Deactivate, assign and remove aren't offered.
            _ => {}
        }
    }
}

/// Computes ordered protocol updates from `old` to `new`.
///
/// Groups are added before their workspaces and removed after workspaces leave
/// them, so every update refers to a valid protocol object.
pub fn diff(old: &Snapshot, new: &Snapshot) -> Vec<Change> {
    let mut changes = Vec::new();
    let before: HashMap<WorkspaceId, (OutputId, WorkspaceSnapshot)> = old
        .workspaces()
        .map(|(output, ws)| (ws.id, (output, ws)))
        .collect();

    for group in &new.groups {
        if !old.has_group(group.output) {
            changes.push(Change::GroupAdded(group.output));
        }
    }

    let mut after = HashMap::new();
    for (output, ws) in new.workspaces() {
        after.insert(ws.id, ());
        match before.get(&ws.id) {
            None => {
                changes.push(Change::WorkspaceAdded(ws.id));
                changes.push(Change::Position(ws.id, ws.index));
                changes.push(Change::Active(ws.id, ws.active));
                changes.push(Change::Enter(output, ws.id));
            }
            Some((old_output, old_ws)) => {
                if old_ws.index != ws.index {
                    changes.push(Change::Position(ws.id, ws.index));
                }
                if old_ws.active != ws.active {
                    changes.push(Change::Active(ws.id, ws.active));
                }
                if *old_output != output {
                    changes.push(Change::Leave(*old_output, ws.id));
                    changes.push(Change::Enter(output, ws.id));
                }
            }
        }
    }

    for (output, ws) in old.workspaces() {
        if !after.contains_key(&ws.id) {
            changes.push(Change::Leave(output, ws.id));
            changes.push(Change::WorkspaceRemoved(ws.id));
        }
    }

    for group in &old.groups {
        if !new.has_group(group.output) {
            changes.push(Change::GroupRemoved(group.output));
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_core::ids::{IdAllocator, OutputKey};

    fn model(outputs: usize) -> (Workspaces<u32>, Vec<OutputId>) {
        let mut ids = IdAllocator::default();
        let mut model = Workspaces::default();
        let outputs = (0..outputs)
            .map(|i| {
                let id = ids.allocate();
                model.add_output(id, OutputKey(format!("OUT-{i}")));
                id
            })
            .collect();
        (model, outputs)
    }

    #[test]
    fn a_new_client_learns_everything() {
        let (mut model, out) = model(2);
        model.add_window(1, Some(out[0]));
        let snapshot = Snapshot::of(&model);
        let changes = diff(&Snapshot::default(), &snapshot);
        let ids: Vec<_> = model.workspace_ids(out[0]).collect();
        assert_eq!(
            &changes[..2],
            &[Change::GroupAdded(out[0]), Change::GroupAdded(out[1])]
        );
        assert_eq!(
            &changes[2..6],
            &[
                Change::WorkspaceAdded(ids[0]),
                Change::Position(ids[0], 0),
                Change::Active(ids[0], true),
                Change::Enter(out[0], ids[0]),
            ]
        );
        // Two on A (one with the window, the trailing empty one), one on B.
        let added = changes
            .iter()
            .filter(|c| matches!(c, Change::WorkspaceAdded(_)))
            .count();
        assert_eq!(added, 3);
        assert!(diff(&snapshot, &snapshot).is_empty());
    }

    #[test]
    fn switching_on_one_output_only_changes_that_output() {
        let (mut model, out) = model(2);
        let (a, b) = (out[0], out[1]);
        model.add_window(1, Some(a));
        model.add_window(10, Some(b));
        model.add_window(11, Some(b));
        model.move_window_to_index(11, 1);
        model.switch_to(b, 1);
        let before = Snapshot::of(&model);
        let b_active = model.active(b).unwrap();

        model.add_window(2, Some(a));
        model.move_window_to_index(2, 1);
        let middle = Snapshot::of(&model);
        model.switch_to(a, 1);
        let after = Snapshot::of(&model);

        let a_ids: Vec<_> = model.workspace_ids(a).collect();
        assert_eq!(
            diff(&middle, &after),
            vec![
                Change::Active(a_ids[0], false),
                Change::Active(a_ids[1], true),
            ]
        );
        // Nothing about B's workspaces either time.
        let b_ids: Vec<_> = model.workspace_ids(b).collect();
        for change in diff(&before, &after) {
            let touches_b = match change {
                Change::Position(id, _)
                | Change::Active(id, _)
                | Change::WorkspaceAdded(id)
                | Change::WorkspaceRemoved(id) => b_ids.contains(&id),
                Change::Enter(output, _)
                | Change::Leave(output, _)
                | Change::GroupAdded(output)
                | Change::GroupRemoved(output) => output == b,
            };
            assert!(!touches_b, "{change:?}");
        }
        assert_eq!(model.active(b), Some(b_active));
    }

    #[test]
    fn removed_workspaces_leave_then_go_and_the_rest_renumber() {
        let (mut model, out) = model(1);
        let a = out[0];
        model.add_window(1, Some(a));
        model.add_window(2, Some(a));
        model.move_window_to_index(2, 1);
        model.add_window(3, Some(a));
        model.move_window_to_index(3, 2);
        model.switch_to(a, 2);
        let before = Snapshot::of(&model);
        let ids: Vec<_> = model.workspace_ids(a).collect();

        // Workspace 2 empties while hidden, so it goes then 3 becomes 2.
        model.remove_window(2);
        let after = Snapshot::of(&model);
        assert_eq!(
            diff(&before, &after),
            vec![
                Change::Position(ids[2], 1),
                Change::Position(ids[3], 2),
                Change::Leave(a, ids[1]),
                Change::WorkspaceRemoved(ids[1]),
            ]
        );
    }

    #[test]
    fn unplugging_moves_workspaces_to_the_other_group() {
        let (mut model, out) = model(2);
        let (a, b) = (out[0], out[1]);
        model.add_window(1, Some(b));
        let moved = model.workspace_of(1).unwrap().0;
        let before = Snapshot::of(&model);

        model.remove_output(b, Some(a));
        let changes = diff(&before, &Snapshot::of(&model));
        let leave = changes.iter().position(|&c| c == Change::Leave(b, moved));
        let enter = changes.iter().position(|&c| c == Change::Enter(a, moved));
        let removed = changes.iter().position(|&c| c == Change::GroupRemoved(b));
        assert!(leave.is_some() && enter.is_some());
        // The group goes last, after its workspaces left it.
        assert_eq!(removed, Some(changes.len() - 1));
    }
}
