//! The desktop-integration protocols Orbit implements itself
//! (ext-workspace-v1 and `orbit_toplevel_manager_v1`) and the toplevel
//! list, as a shell would use them.

use std::collections::HashMap;

use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, backend::ObjectId, delegate_noop,
    event_created_child, protocol::wl_registry::WlRegistry,
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    },
    workspace::v1::client::{
        ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
        ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
        ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
    },
};

use self::generated::{
    orbit_toplevel_manager_v1::OrbitToplevelManagerV1,
    orbit_toplevel_state_v1::{self, OrbitToplevelStateV1},
};
use super::{Fixture, TestClient, client::ClientData, map_window};
use crate::workspaces::WorkspaceTarget;

use orbit_protocols::toplevel_management::client as generated;

/// What a shell knows from the desktop protocols.
#[derive(Default)]
pub struct Desktop {
    /// Every event received on these protocols, to check idle silence.
    pub events: usize,
    pub workspaces: HashMap<ObjectId, WorkspaceInfo>,
    pub toplevels: Vec<ExtForeignToplevelHandleV1>,
    pub manager_finished: bool,
    /// The latest `orbit_toplevel_state_v1.state` of each state object
    /// (sorted), and how many it got.
    pub toplevel_states: HashMap<ObjectId, (Vec<u32>, usize)>,
}

#[derive(Default, Debug, Clone)]
pub struct WorkspaceInfo {
    pub name: String,
    pub active: bool,
    pub removed: bool,
}

struct Shell {
    workspaces: ExtWorkspaceManagerV1,
    _toplevels: ExtForeignToplevelListV1,
    manager: OrbitToplevelManagerV1,
}

fn bind_shell(fixture: &mut Fixture, client: &mut TestClient) -> Shell {
    let qh = client.qh();
    let registry: WlRegistry = client.conn.display().get_registry(&qh, ());
    let shell = Shell {
        workspaces: client.bind(&registry, "ext_workspace_manager_v1", 1, &qh),
        _toplevels: client.bind(&registry, "ext_foreign_toplevel_list_v1", 1, &qh),
        manager: client.bind(&registry, "orbit_toplevel_manager_v1", 2, &qh),
    };
    fixture.roundtrip(client);
    shell
}

/// The shown workspace's handle on each output, in output order.
fn handle_named<'a>(client: &'a TestClient, name: &str, active: bool) -> Option<&'a ObjectId> {
    client
        .data
        .desktop
        .workspaces
        .iter()
        .find(|(_, ws)| !ws.removed && ws.name == name && ws.active == active)
        .map(|(id, _)| id)
}

#[test]
fn a_shell_switches_one_output_without_touching_the_other() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    let mut apps = fixture.client();
    // A window on each output, so each has a second (empty) workspace.
    fixture.state.set_focused_output(&a);
    let _on_a = map_window(&mut fixture, &mut apps);
    fixture.state.set_focused_output(&b);
    let _on_b = map_window(&mut fixture, &mut apps);

    let mut shell_client = fixture.client();
    let shell = bind_shell(&mut fixture, &mut shell_client);
    let live = shell_client
        .data
        .desktop
        .workspaces
        .values()
        .filter(|ws| !ws.removed)
        .count();
    assert_eq!(live, 4);
    assert_eq!(shell_client.data.desktop.toplevels.len(), 2);

    // Activate B's workspace 2: B's id gives the output.
    let b_id = fixture.state.output_id(&b).expect("B");
    let a_id = fixture.state.output_id(&a).expect("A");
    let a_before = fixture.state.workspaces.active(a_id);
    let target = fixture
        .state
        .workspaces
        .workspace_ids(b_id)
        .nth(1)
        .expect("B has two workspaces");
    // Find the client's handle for it: the inactive "2" whose output is
    // B. Both outputs have an inactive "2", so try B's by switching and
    // checking the model.
    let candidates: Vec<ObjectId> = shell_client
        .data
        .desktop
        .workspaces
        .iter()
        .filter(|(_, ws)| !ws.removed && ws.name == "2")
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(candidates.len(), 2);
    let mut switched = false;
    for id in candidates {
        let handle = ExtWorkspaceHandleV1::from_id(&shell_client.conn, id).expect("live handle");
        handle.activate();
        shell.workspaces.commit();
        fixture.roundtrip(&mut shell_client);
        if fixture.state.workspaces.active(b_id) == Some(target) {
            switched = true;
            break;
        }
        // That was A's: put A back and try the other.
        fixture
            .state
            .switch_workspace(&a, WorkspaceTarget::Index(0));
        fixture.roundtrip(&mut shell_client);
    }
    assert!(switched);
    assert_eq!(fixture.state.workspaces.active(a_id), a_before);
    assert!(handle_named(&shell_client, "2", true).is_some());
}

#[test]
fn stale_handles_are_ignored() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut apps = fixture.client();
    let window = map_window(&mut fixture, &mut apps);
    let mut shell_client = fixture.client();
    let shell = bind_shell(&mut fixture, &mut shell_client);
    let toplevel = shell_client.data.desktop.toplevels[0].clone();
    let workspace_id = handle_named(&shell_client, "2", false)
        .cloned()
        .expect("the empty workspace");
    let workspace =
        ExtWorkspaceHandleV1::from_id(&shell_client.conn, workspace_id).expect("live handle");

    // The window closes and its workspace goes away.
    window.toplevel.destroy();
    window.xdg_surface.destroy();
    window.surface.destroy();
    fixture.roundtrip(&mut apps);
    fixture.roundtrip(&mut shell_client);
    assert!(fixture.state.windows.is_empty());

    // Acting on them now does nothing, and isn't an error.
    let seat = shell_client.bind(
        &shell_client
            .conn
            .display()
            .get_registry(&shell_client.qh(), ()),
        "wl_seat",
        1,
        &shell_client.qh(),
    );
    shell.manager.activate(&toplevel, &seat);
    shell.manager.move_to_workspace(&toplevel, &workspace);
    workspace.activate();
    shell.workspaces.commit();
    fixture.roundtrip(&mut shell_client);
    assert!(fixture.is_connected(&shell_client.server));
}

#[test]
fn requests_after_stop_are_harmless() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut shell_client = fixture.client();
    let shell = bind_shell(&mut fixture, &mut shell_client);
    let workspace_id = handle_named(&shell_client, "1", true)
        .cloned()
        .expect("the shown workspace");

    shell.workspaces.stop();
    fixture.roundtrip(&mut shell_client);
    assert!(shell_client.data.desktop.manager_finished);

    // The workspace handle outlives its manager; activating it is ignored.
    let workspace =
        ExtWorkspaceHandleV1::from_id(&shell_client.conn, workspace_id).expect("live handle");
    workspace.activate();
    workspace.destroy();
    fixture.roundtrip(&mut shell_client);
    assert!(fixture.is_connected(&shell_client.server));

    // Later changes reach nobody, and don't fail.
    let mut apps = fixture.client();
    let _window = map_window(&mut fixture, &mut apps);
    fixture.roundtrip(&mut shell_client);
    assert!(fixture.is_connected(&shell_client.server));
}

#[test]
fn an_idle_desktop_sends_shells_nothing() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut apps = fixture.client();
    let _window = map_window(&mut fixture, &mut apps);
    let mut shell_client = fixture.client();
    let _shell = bind_shell(&mut fixture, &mut shell_client);
    let before = shell_client.data.desktop.events;
    assert!(before > 0);

    for _ in 0..10 {
        fixture.dispatch();
        fixture.roundtrip(&mut apps);
        fixture.roundtrip(&mut shell_client);
    }
    assert_eq!(shell_client.data.desktop.events, before);

    // A switch is sent, once.
    let a = fixture.output("A");
    fixture
        .state
        .switch_workspace(&a, WorkspaceTarget::Index(1));
    fixture.roundtrip(&mut shell_client);
    let after_switch = shell_client.data.desktop.events;
    assert!(after_switch > before);
    fixture.roundtrip(&mut shell_client);
    assert_eq!(shell_client.data.desktop.events, after_switch);
}

/// The latest state the shell was told for `object`, and how many state
/// events it got.
fn toplevel_state(client: &TestClient, object: &OrbitToplevelStateV1) -> (Vec<u32>, usize) {
    client
        .data
        .desktop
        .toplevel_states
        .get(&object.id())
        .cloned()
        .unwrap_or_default()
}

const ACTIVATED: u32 = orbit_toplevel_state_v1::State::Activated as u32;
const MINIMIZED: u32 = orbit_toplevel_state_v1::State::Minimized as u32;
const MAXIMIZED: u32 = orbit_toplevel_state_v1::State::Maximized as u32;

#[test]
fn a_shell_follows_and_changes_toplevel_state() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut apps = fixture.client();
    let _first = map_window(&mut fixture, &mut apps);
    let second = map_window(&mut fixture, &mut apps);
    let mut shell_client = fixture.client();
    let shell = bind_shell(&mut fixture, &mut shell_client);
    let qh = shell_client.qh();
    let toplevels = shell_client.data.desktop.toplevels.clone();
    assert_eq!(toplevels.len(), 2);
    let states: Vec<OrbitToplevelStateV1> = toplevels
        .iter()
        .map(|t| shell.manager.get_state(t, &qh, ()))
        .collect();
    fixture.roundtrip(&mut shell_client);
    // Sent on creation: the newest window has focus.
    assert_eq!(toplevel_state(&shell_client, &states[0]), (vec![], 1));
    assert_eq!(
        toplevel_state(&shell_client, &states[1]),
        (vec![ACTIVATED], 1)
    );

    // Focus moves: both hear about it.
    let seat = shell_client.bind(
        &shell_client.conn.display().get_registry(&qh, ()),
        "wl_seat",
        1,
        &qh,
    );
    shell.manager.activate(&toplevels[0], &seat);
    fixture.roundtrip(&mut shell_client);
    assert_eq!(toplevel_state(&shell_client, &states[0]).0, vec![ACTIVATED]);
    assert_eq!(toplevel_state(&shell_client, &states[1]).0, vec![]);

    shell.manager.set_maximized(&toplevels[0]);
    fixture.roundtrip(&mut shell_client);
    assert_eq!(
        toplevel_state(&shell_client, &states[0]).0,
        vec![ACTIVATED, MAXIMIZED]
    );
    shell.manager.unset_maximized(&toplevels[0]);
    fixture.roundtrip(&mut shell_client);
    assert_eq!(toplevel_state(&shell_client, &states[0]).0, vec![ACTIVATED]);

    // Minimizing hands focus to the other window; restoring doesn't take
    // it back.
    shell.manager.set_minimized(&toplevels[0]);
    fixture.roundtrip(&mut shell_client);
    assert_eq!(toplevel_state(&shell_client, &states[0]).0, vec![MINIMIZED]);
    assert_eq!(toplevel_state(&shell_client, &states[1]).0, vec![ACTIVATED]);
    shell.manager.unset_minimized(&toplevels[0]);
    fixture.roundtrip(&mut shell_client);
    assert_eq!(toplevel_state(&shell_client, &states[0]).0, vec![]);
    assert_eq!(toplevel_state(&shell_client, &states[1]).0, vec![ACTIVATED]);

    // Nothing changes: nothing is sent.
    let counts = (
        toplevel_state(&shell_client, &states[0]).1,
        toplevel_state(&shell_client, &states[1]).1,
    );
    for _ in 0..5 {
        fixture.dispatch();
        fixture.roundtrip(&mut apps);
        fixture.roundtrip(&mut shell_client);
    }
    assert_eq!(
        counts,
        (
            toplevel_state(&shell_client, &states[0]).1,
            toplevel_state(&shell_client, &states[1]).1,
        )
    );

    // Close asks the client; once it goes, its state object falls silent.
    shell.manager.close(&toplevels[1]);
    fixture.roundtrip(&mut shell_client);
    fixture.roundtrip(&mut apps);
    assert_eq!(apps.data.closed, vec![second.toplevel.id().protocol_id()]);
    second.toplevel.destroy();
    second.xdg_surface.destroy();
    second.surface.destroy();
    fixture.roundtrip(&mut apps);
    fixture.roundtrip(&mut shell_client);
    let silent = toplevel_state(&shell_client, &states[1]);
    shell.manager.activate(&toplevels[0], &seat);
    fixture.roundtrip(&mut shell_client);
    assert_eq!(toplevel_state(&shell_client, &states[1]), silent);
    assert_eq!(toplevel_state(&shell_client, &states[0]).0, vec![ACTIVATED]);

    // A state object for a closed toplevel never says anything.
    let late = shell.manager.get_state(&toplevels[1], &qh, ());
    fixture.roundtrip(&mut shell_client);
    assert_eq!(toplevel_state(&shell_client, &late), (vec![], 0));
    late.destroy();
    states[0].destroy();
    fixture.roundtrip(&mut shell_client);
    assert!(fixture.is_connected(&shell_client.server));
    assert!(fixture.state.desktop.toplevel_manager_is_idle());
}

impl Dispatch<OrbitToplevelStateV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        object: &OrbitToplevelStateV1,
        event: orbit_toplevel_state_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.desktop.events += 1;
        let orbit_toplevel_state_v1::Event::State { state: bytes } = event else {
            return;
        };
        let mut values: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| u32::from_ne_bytes(b))
            .collect();
        values.sort_unstable();
        let entry = state
            .desktop
            .toplevel_states
            .entry(object.id())
            .or_default();
        entry.0 = values;
        entry.1 += 1;
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.desktop.events += 1;
        match event {
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                state
                    .desktop
                    .workspaces
                    .insert(workspace.id(), WorkspaceInfo::default());
            }
            ext_workspace_manager_v1::Event::Finished => state.desktop.manager_finished = true,
            _ => {}
        }
    }

    event_created_child!(ClientData, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &ExtWorkspaceGroupHandleV1,
        _: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.desktop.events += 1;
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        workspace: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.desktop.events += 1;
        let info = state.desktop.workspaces.entry(workspace.id()).or_default();
        match event {
            ext_workspace_handle_v1::Event::Name { name } => info.name = name,
            ext_workspace_handle_v1::Event::State { state } => {
                info.active = state
                    .into_result()
                    .is_ok_and(|s| s.contains(ext_workspace_handle_v1::State::Active));
            }
            ext_workspace_handle_v1::Event::Removed => info.removed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.desktop.events += 1;
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.desktop.toplevels.push(toplevel);
        }
    }

    event_created_child!(ClientData, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        toplevel: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.desktop.events += 1;
        if let ext_foreign_toplevel_handle_v1::Event::Closed = event {
            state.desktop.toplevels.retain(|t| t != toplevel);
        }
    }
}

delegate_noop!(ClientData: ignore OrbitToplevelManagerV1);
delegate_noop!(ClientData: ignore wayland_client::protocol::wl_seat::WlSeat);
