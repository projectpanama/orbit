use crate::ids::{IdAllocator, OutputId, OutputKey, WorkspaceId};

/// Describes windows that should be hidden and shown after a workspace change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visibility<W> {
    /// Windows that are no longer visible.
    pub hide: Vec<W>,
    /// Windows that became visible.
    pub show: Vec<W>,
}

impl<W> Default for Visibility<W> {
    fn default() -> Self {
        Self {
            hide: Vec::new(),
            show: Vec::new(),
        }
    }
}

impl<W> Visibility<W> {
    /// Reports whether no windows need to be hidden or shown.
    pub fn is_empty(&self) -> bool {
        self.hide.is_empty() && self.show.is_empty()
    }
}

/// Internal workspace state, including its windows and remembered output.
#[derive(Debug)]
struct Workspace<W> {
    /// Stable identifier for this workspace.
    id: WorkspaceId,
    /// Windows assigned to the workspace.
    windows: Vec<W>,
    /// Most recently focused window in the workspace.
    last_focused: Option<W>,
    /// Output this workspace was moved away from when that output was
    /// disconnected. Used to restore the workspace when the output returns.
    parent: Option<OutputKey>,
    /// Whether this workspace was active on its remembered output.
    was_active_on_parent: bool,
}

impl<W: Copy + Eq> Workspace<W> {
    /// Creates an empty workspace with a newly allocated identifier.
    fn new(ids: &mut IdAllocator) -> Self {
        Self {
            id: ids.allocate(),
            windows: Vec::new(),
            last_focused: None,
            parent: None,
            was_active_on_parent: false,
        }
    }

    /// Removes a window and clears focus if it referred to that window.
    fn remove(&mut self, window: W) -> bool {
        // Find the index of the window to remove
        let Some(index) = self.windows.iter().position(|&w| w == window) else {
            return false;
        };

        self.windows.remove(index);

        // If the removed window was the last focused window, clear the last focused field
        if self.last_focused == Some(window) {
            self.last_focused = None;
        }

        true
    }
}

/// Workspace state belonging to one output.
#[derive(Debug)]
struct OutputWorkspaces<W> {
    /// Stable identity of the output.
    key: OutputKey,
    /// Runtime identifier of the output.
    output: OutputId,
    /// Ordered workspaces on the output.
    workspaces: Vec<Workspace<W>>,
    /// Index of the currently visible workspace.
    active: usize,
}

impl<W: Copy + Eq> OutputWorkspaces<W> {
    /// Returns the currently active workspace.
    fn active(&self) -> &Workspace<W> {
        &self.workspaces[self.active]
    }

    /// Removes empty workspaces while preserving a trailing empty workspace.
    fn normalize(&mut self, ids: &mut IdAllocator) {
        let active_id = self
            .workspaces
            .get(self.active)
            .map(|workspace| workspace.id);
        let last = self.workspaces.len().saturating_sub(1);

        let mut index = 0;

        // Remove empty workspaces, but keep the last one and the active one.
        self.workspaces.retain(|workspace| {
            let keep =
                !workspace.windows.is_empty() || Some(workspace.id) == active_id || index == last;
            index += 1;
            keep
        });

        // Ensure there is at least one workspace, and that the active workspace is valid.
        if self
            .workspaces
            .last()
            .is_none_or(|workspace| !workspace.windows.is_empty())
        {
            self.workspaces.push(Workspace::new(ids));
        }

        // Update the active index to point to the correct workspace after normalization.
        self.active = active_id
            .and_then(|id| {
                self.workspaces
                    .iter()
                    .position(|workspace| workspace.id == id)
            })
            .unwrap_or(0);

        let len = self.workspaces.len();

        // If the active workspace is not the last one and is empty, remove it.
        if len >= 2 && self.active == len - 2 && self.workspaces[self.active].windows.is_empty() {
            self.workspaces.pop();
        }
    }

    /// Switches to a workspace and reports the resulting visibility changes.
    fn switch(&mut self, index: usize, ids: &mut IdAllocator) -> Visibility<W> {
        let index = index.min(self.workspaces.len() - 1);
        if index == self.active {
            return Visibility::default();
        }
        let visibility = Visibility {
            hide: self.active().windows.clone(),
            show: self.workspaces[index].windows.clone(),
        };
        self.active = index;
        self.normalize(ids);
        visibility
    }
}

/// Stores workspaces, their output assignments, and disconnected workspaces.
#[derive(Debug)]
pub struct Workspaces<W> {
    /// Allocator for workspace identifiers.
    ids: IdAllocator,
    /// Workspace groups currently attached to outputs.
    outputs: Vec<OutputWorkspaces<W>>,
    /// Workspaces waiting for an output to return or be added.
    orphans: Vec<Workspace<W>>,
    /// Counts changes to the workspace state, so observers can
    /// tell nothing changed without having to compare everything.
    generation: u64,
}

impl<W> Default for Workspaces<W> {
    fn default() -> Self {
        Self {
            outputs: Vec::new(),
            orphans: Vec::new(),
            ids: IdAllocator::default(),
            generation: 0,
        }
    }
}

impl<W: Copy + Eq> Workspaces<W> {
    /// Adds an output and restores workspaces remembered for its key.
    ///
    /// Returns the windows whose visibility changed as a result.
    pub fn add_output(&mut self, output: OutputId, key: OutputKey) -> Visibility<W> {
        self.generation += 1;
        let before = self.visible();
        let mut returning = Vec::new();

        for other in &mut self.outputs {
            let mut index = 0;
            while index < other.workspaces.len() {
                if other.workspaces[index].parent.as_ref() != Some(&key) {
                    index += 1;
                    continue;
                }
                returning.push(other.workspaces.remove(index));
                // Keep showing the same workspace, or a neighbour if this
                // was it. The last workspace never leaves, so one remains.
                if index < other.active {
                    other.active -= 1;
                } else if index == other.active {
                    other.active = index
                        .saturating_sub(1)
                        .min(other.workspaces.len().saturating_sub(1));
                }
            }
            other.normalize(&mut self.ids);
        }

        let mut active = returning
            .iter()
            .position(|workspace| workspace.was_active_on_parent)
            .unwrap_or(0);

        if returning.is_empty() && !self.orphans.is_empty() {
            // Show the windows that had nowhere to be shown.
            active = 0;
        }

        returning.append(&mut self.orphans);

        for workspace in &mut returning {
            if workspace.parent.as_ref() == Some(&key) {
                workspace.parent = None;
                workspace.was_active_on_parent = false;
            }
        }

        let mut new = OutputWorkspaces {
            output,
            key,
            workspaces: returning,
            active,
        };

        new.normalize(&mut self.ids);
        self.outputs.push(new);
        Self::diff(&before, &self.visible())
    }

    /// Removes an output and migrates its non-empty workspaces.
    ///
    /// Workspaces are sent to `target` when available; otherwise they become
    /// orphans and can be restored when a matching output returns.
    pub fn remove_output(&mut self, output: OutputId, target: Option<OutputId>) -> Visibility<W> {
        self.generation += 1;

        let Some(index) = self.position(output) else {
            return Visibility::default();
        };

        let removed = self.outputs.remove(index);

        let visibility = Visibility {
            hide: removed.active().windows.clone(),
            show: Vec::new(),
        };

        let key = removed.key;
        let active = removed.active;

        // Migrate the non-empty workspaces to the target output or make them orphans.
        let migrating: Vec<Workspace<W>> = removed
            .workspaces
            .into_iter()
            .enumerate()
            .filter(|(_, workspace)| !workspace.windows.is_empty())
            .map(|(index, mut workspace)| {
                // A workspace that already came from elsewhere keeps its original home.
                if workspace.parent.is_none() {
                    workspace.parent = Some(key.clone());
                    workspace.was_active_on_parent = index == active;
                }
                workspace
            })
            .collect();

        // If the target output is available, move the workspaces there; otherwise
        // they become orphans and can be restored when a matching output returns.
        match target.and_then(|target| self.position(target)) {
            Some(target) => {
                let target = &mut self.outputs[target];
                let at = target.workspaces.len() - 1;
                if target.active >= at {
                    target.active += migrating.len();
                }
                target.workspaces.splice(at..at, migrating);
                target.normalize(&mut self.ids);
            }
            None => self.orphans.extend(migrating),
        }

        visibility
    }

    /// Switches the active workspace on `output` to the one at `index`.
    pub fn switch_to(&mut self, output: OutputId, index: usize) -> Visibility<W> {
        self.generation += 1;
        let Some(out) = self.position(output) else {
            return Visibility::default();
        };
        self.outputs[out].switch(index, &mut self.ids)
    }

    /// Moves an output's active workspace by `delta` positions.
    pub fn switch_by(&mut self, output: OutputId, delta: isize) -> Visibility<W> {
        self.generation += 1;
        let Some(out) = self.position(output) else {
            return Visibility::default();
        };
        let index = self.outputs[out].active.saturating_add_signed(delta);
        self.outputs[out].switch(index, &mut self.ids)
    }

    /// Adds a window to the active workspace on `output`.
    ///
    /// If the output is unavailable, the window is placed in an orphan
    /// workspace.
    pub fn add_window(&mut self, window: W, output: Option<OutputId>) {
        self.generation += 1;
        match output.and_then(|output| self.position(output)) {
            Some(out) => {
                let out = &mut self.outputs[out];
                let active = out.active;
                out.workspaces[active].windows.push(window);
                out.normalize(&mut self.ids);
            }
            None => {
                // Not with workspaces waiting for a particular monitor.
                let index = match self.orphans.iter().position(|ws| ws.parent.is_none()) {
                    Some(index) => index,
                    None => {
                        self.orphans.push(Workspace::new(&mut self.ids));
                        self.orphans.len() - 1
                    }
                };
                self.orphans[index].windows.push(window);
            }
        }
    }

    /// Removes a window from whichever workspace contains it.
    pub fn remove_window(&mut self, window: W) {
        self.generation += 1;
        for out in &mut self.outputs {
            if out
                .workspaces
                .iter_mut()
                .any(|workspace| workspace.remove(window))
            {
                out.normalize(&mut self.ids);
                return;
            }
        }
        for workspace in &mut self.orphans {
            workspace.remove(window);
        }
        self.orphans
            .retain(|workspace| !workspace.windows.is_empty());
    }

    /// Moves a window to a workspace index on its current output.
    ///
    /// Returns visibility changes when the move crosses the active workspace.
    pub fn move_window_to_index(&mut self, window: W, index: usize) -> Visibility<W> {
        self.generation += 1;

        let Some((out, from)) = self.locate(window) else {
            return Visibility::default();
        };

        let out = &mut self.outputs[out];
        let to = index.min(out.workspaces.len() - 1);

        if to == from {
            return Visibility::default();
        }

        out.workspaces[from].remove(window);
        out.workspaces[to].windows.push(window);

        let mut visibility = Visibility::default();

        // If the window was on the active workspace, it should be hidden.
        // If it was moved to the active workspace, it should be shown.
        if from == out.active {
            visibility.hide.push(window);
        } else if to == out.active {
            visibility.show.push(window);
        }

        out.normalize(&mut self.ids);
        visibility
    }

    /// Moves a window to a workspace identified by `workspace`.
    ///
    /// Returns hide/show changes for moves between visible workspaces.
    pub fn move_window_to_workspace(&mut self, window: W, workspace: WorkspaceId) -> Visibility<W> {
        self.generation += 1;

        let (Some((from_out, from_ws)), Some((to_out, to_ws))) =
            (self.locate(window), self.locate_workspace(workspace))
        else {
            return Visibility::default();
        };

        if (from_out, from_ws) == (to_out, to_ws) {
            return Visibility::default();
        }

        let was_visible = from_ws == self.outputs[from_out].active;
        let is_visible = to_ws == self.outputs[to_out].active;

        self.outputs[from_out].workspaces[from_ws].remove(window);
        self.outputs[to_out].workspaces[to_ws].windows.push(window);
        self.outputs[from_out].normalize(&mut self.ids);

        if to_out != from_out {
            self.outputs[to_out].normalize(&mut self.ids);
        }

        let mut visibility = Visibility::default();

        if was_visible {
            visibility.hide.push(window);
        }

        if is_visible {
            visibility.show.push(window);
        }

        visibility
    }

    /// Moves a window to the active workspace on another output.
    ///
    /// Returns a show change when the window was previously on an inactive
    /// workspace and becomes visible on the destination output.
    pub fn move_window_to_output(&mut self, window: W, output: OutputId) -> Visibility<W> {
        self.generation += 1;

        let Some(to) = self.position(output) else {
            return Visibility::default();
        };

        let Some((from, ws)) = self.locate(window) else {
            return Visibility::default();
        };

        if from == to && ws == self.outputs[to].active {
            return Visibility::default();
        }

        let was_visible = ws == self.outputs[from].active;

        self.outputs[from].workspaces[ws].remove(window);
        self.outputs[from].normalize(&mut self.ids);

        let target = &mut self.outputs[to];
        let active = target.active;
        target.workspaces[active].windows.push(window);
        target.normalize(&mut self.ids);

        let mut visibility = Visibility::default();
        if !was_visible {
            visibility.show.push(window);
        }
        visibility
    }

    /// Records `window` as the most recently focused window in its workspace.
    pub fn focused(&mut self, window: W) {
        self.generation += 1;
        if let Some((out, ws)) = self.locate(window) {
            self.outputs[out].workspaces[ws].last_focused = Some(window);
        }
    }

    /// Selects a usable focus target from the active workspace on `output`.
    ///
    /// The remembered focused window is preferred, followed by the most
    /// recently added usable window.
    pub fn focus_target(&self, output: OutputId, usable: impl Fn(W) -> bool) -> Option<W> {
        let workspace = self.outputs[self.position(output)?].active();
        workspace
            .last_focused
            .filter(|&w| usable(w))
            .or_else(|| workspace.windows.iter().rev().copied().find(|&w| usable(w)))
    }

    /// Returns the mutation generation of this workspace model.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the active workspace identifier for `output`.
    pub fn active(&self, output: OutputId) -> Option<WorkspaceId> {
        self.position(output)
            .map(|out| self.outputs[out].active().id)
    }

    /// Returns the active workspace index and total workspace count.
    pub fn active_index(&self, output: OutputId) -> Option<(usize, usize)> {
        let out = &self.outputs[self.position(output)?];
        Some((out.active, out.workspaces.len()))
    }

    /// Returns windows on the active workspace for `output`.
    pub fn active_windows(&self, output: OutputId) -> &[W] {
        self.position(output)
            .map_or(&[], |out| &self.outputs[out].active().windows)
    }

    /// Iterates over workspace identifiers belonging to `output`.
    pub fn workspace_ids(&self, output: OutputId) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.position(output).into_iter().flat_map(|out| {
            self.outputs[out]
                .workspaces
                .iter()
                .map(|workspace| workspace.id)
        })
    }

    /// Returns a workspace's index on an output.
    pub fn index_of(&self, output: OutputId, workspace_id: WorkspaceId) -> Option<usize> {
        self.workspace_ids(output).position(|id| id == workspace_id)
    }

    /// Iterates over currently registered output identifiers.
    pub fn output_ids(&self) -> impl Iterator<Item = OutputId> + '_ {
        self.outputs.iter().map(|out| out.output)
    }

    /// Finds the output and index containing a workspace.
    pub fn find_workspace(&self, workspace_id: WorkspaceId) -> Option<(OutputId, usize)> {
        let (out, ws) = self.locate_workspace(workspace_id)?;
        Some((self.outputs[out].output, ws))
    }

    /// Returns windows assigned to a workspace.
    pub fn windows_of(&self, workspace_id: WorkspaceId) -> &[W] {
        self.outputs
            .iter()
            .flat_map(|out| &out.workspaces)
            .find(|workspace| workspace.id == workspace_id)
            .map_or(&[], |workspace| &workspace.windows)
    }

    /// Returns the output containing `window`.
    pub fn output_of(&self, window: W) -> Option<OutputId> {
        self.locate(window).map(|(out, _)| self.outputs[out].output)
    }

    /// Returns the workspace identifier and index containing `window`.
    pub fn workspace_of(&self, window: W) -> Option<(WorkspaceId, usize)> {
        let (out, ws) = self.locate(window)?;
        Some((self.outputs[out].workspaces[ws].id, ws))
    }

    /// Reports whether `window` belongs to an active workspace.
    pub fn is_visible(&self, window: W) -> bool {
        self.locate(window)
            .is_some_and(|(out, ws)| self.outputs[out].active == ws)
    }

    /// Returns visible windows paired with their output identifiers.
    fn visible(&self) -> Vec<(W, OutputId)> {
        self.outputs
            .iter()
            .flat_map(|out| out.active().windows.iter().map(|&w| (w, out.output)))
            .collect()
    }

    /// Computes the hide/show changes between two visible-window lists.
    fn diff(before: &[(W, OutputId)], after: &[(W, OutputId)]) -> Visibility<W> {
        Visibility {
            hide: before
                .iter()
                .filter(|entry| !after.contains(entry))
                .map(|&(w, _)| w)
                .collect(),
            show: after
                .iter()
                .filter(|entry| !before.contains(entry))
                .map(|&(w, _)| w)
                .collect(),
        }
    }

    /// Finds the internal index of an output.
    fn position(&self, output: OutputId) -> Option<usize> {
        self.outputs.iter().position(|out| out.output == output)
    }

    /// Finds internal output and workspace indices by workspace identifier.
    fn locate_workspace(&self, workspace: WorkspaceId) -> Option<(usize, usize)> {
        self.outputs.iter().enumerate().find_map(|(out, outputs)| {
            outputs
                .workspaces
                .iter()
                .position(|ws| ws.id == workspace)
                .map(|ws| (out, ws))
        })
    }

    /// Finds the workspace containing a window.
    fn locate(&self, window: W) -> Option<(usize, usize)> {
        self.outputs.iter().enumerate().find_map(|(out, outputs)| {
            outputs
                .workspaces
                .iter()
                .position(|workspace| workspace.windows.contains(&window))
                .map(|ws| (out, ws))
        })
    }
}
