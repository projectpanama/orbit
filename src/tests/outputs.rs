//! Outputs coming and going, and per-output workspaces staying
//! independent, with real clients' windows on them.

use std::time::Duration;

use smithay::{desktop::Window, output::Output};

use super::{Fixture, TestClient, map_window};
use orbit_core::ids::WorkspaceId;

use crate::{
    input::gestures::SwipeSource,
    state::Orbit,
    workspaces::{WorkspaceTarget, window_id},
};

/// Opens a window on `output`'s shown workspace and returns Orbit's side
/// of it.
fn window_on(fixture: &mut Fixture, client: &mut TestClient, output: &Output) -> Window {
    fixture.state.set_focused_output(output);
    let before: Vec<_> = fixture.state.windows.keys().copied().collect();
    let _window = map_window(fixture, client);
    let (_, window) = fixture
        .state
        .windows
        .iter()
        .find(|(id, _)| !before.contains(id))
        .expect("a new window");
    window.clone()
}

fn active(state: &Orbit, output: &Output) -> WorkspaceId {
    let id = state.output_id(output).expect("output is connected");
    state
        .workspaces
        .active(id)
        .expect("an output always shows one")
}

fn shown(state: &Orbit, window: &Window) -> bool {
    state.space.element_location(window).is_some()
}

#[test]
fn switching_on_one_output_never_changes_another() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    let mut client = fixture.client();

    // Output A shows A1; output B shows B2 (B1 has a window too).
    let on_a1 = window_on(&mut fixture, &mut client, &a);
    let on_b1 = window_on(&mut fixture, &mut client, &b);
    fixture
        .state
        .switch_workspace(&b, WorkspaceTarget::Index(1));
    let on_b2 = window_on(&mut fixture, &mut client, &b);
    let b_active = active(&fixture.state, &b);

    // Output A moves to A2 and back.
    fixture
        .state
        .switch_workspace(&a, WorkspaceTarget::Index(1));
    fixture.dispatch();
    assert_ne!(active(&fixture.state, &a), active(&fixture.state, &b));
    assert_eq!(active(&fixture.state, &b), b_active);
    assert!(!shown(&fixture.state, &on_a1));
    assert!(!shown(&fixture.state, &on_b1));
    assert!(shown(&fixture.state, &on_b2));

    fixture
        .state
        .switch_workspace(&a, WorkspaceTarget::Index(0));
    fixture.dispatch();
    assert_eq!(active(&fixture.state, &b), b_active);
    assert!(shown(&fixture.state, &on_a1));
    assert!(shown(&fixture.state, &on_b2));
}

#[test]
fn unplugged_windows_wait_on_another_output_and_come_back() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    let mut client = fixture.client();
    let on_a = window_on(&mut fixture, &mut client, &a);
    let _on_b1 = window_on(&mut fixture, &mut client, &b);
    fixture
        .state
        .switch_workspace(&b, WorkspaceTarget::Index(1));
    let on_b2 = window_on(&mut fixture, &mut client, &b);
    let a_active = active(&fixture.state, &a);

    fixture.remove_output(&b);
    fixture.roundtrip(&mut client);
    // B's windows are kept on A, hidden; A still shows what it showed.
    assert_eq!(fixture.state.windows.len(), 3);
    assert_eq!(active(&fixture.state, &a), a_active);
    assert!(shown(&fixture.state, &on_a));
    assert!(!shown(&fixture.state, &on_b2));
    let id = window_id(&on_b2).expect("window id");
    assert_eq!(
        fixture.state.workspaces.output_of(id),
        fixture.state.output_id(&a)
    );

    // Plugged back in, B shows the workspace it showed before.
    let b = fixture.add_output("B");
    fixture.roundtrip(&mut client);
    assert!(shown(&fixture.state, &on_b2));
    assert_eq!(
        fixture.state.workspaces.output_of(id),
        fixture.state.output_id(&b)
    );
    assert_eq!(active(&fixture.state, &a), a_active);
}

#[test]
fn windows_survive_having_no_output_at_all() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    let mut client = fixture.client();
    let window = window_on(&mut fixture, &mut client, &a);

    fixture.remove_output(&a);
    fixture.roundtrip(&mut client);
    assert_eq!(fixture.state.windows.len(), 1);
    assert_eq!(fixture.state.space.elements().count(), 0);

    // A window opened with no output is kept too.
    let _late = map_window(&mut fixture, &mut client);
    assert_eq!(fixture.state.windows.len(), 2);

    // Any monitor that appears takes them in.
    let _c = fixture.add_output("C");
    fixture.roundtrip(&mut client);
    assert!(shown(&fixture.state, &window));
}

#[test]
fn unplugging_during_a_slide_or_swipe() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    let mut client = fixture.client();
    let _on_a = window_on(&mut fixture, &mut client, &a);
    let _on_b = window_on(&mut fixture, &mut client, &b);

    // An animated switch on B, cut short by unplugging B.
    fixture
        .state
        .switch_workspace(&b, WorkspaceTarget::Index(1));
    fixture.dispatch();
    fixture.remove_output(&b);
    fixture.advance(Duration::from_millis(50));

    // A swipe on A (the pointer is there), with A unplugged mid-swipe.
    let b = fixture.add_output("B");
    assert!(fixture.state.swipe_begin(SwipeSource::Touchpad));
    fixture.state.swipe_update(-200.0);
    fixture.remove_output(&a);
    fixture.state.swipe_update(-200.0);
    fixture.state.swipe_end(false);
    fixture.advance(Duration::from_millis(50));
    assert!(fixture.state.input.workspace_swipe.is_none());
    fixture.roundtrip(&mut client);
    assert_eq!(fixture.state.windows.len(), 2);
    assert!(fixture.state.output_id(&b).is_some());
}

#[test]
fn plugging_in_and_out_repeatedly_stays_bounded() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    let mut client = fixture.client();
    let _on_a = window_on(&mut fixture, &mut client, &a);
    for _ in 0..100 {
        let x = fixture.add_output("X");
        let _on_x = window_on(&mut fixture, &mut client, &x);
        fixture.remove_output(&x);
    }
    fixture.roundtrip(&mut client);
    assert_eq!(fixture.state.outputs.len(), 1);
    assert_eq!(fixture.state.workspaces.output_ids().count(), 1);
    // Every window ended up on A: its windows' workspaces plus the empty
    // one, not one workspace per plug.
    let workspaces = fixture
        .state
        .workspaces
        .workspace_ids(fixture.state.output_id(&a).expect("A is connected"));
    assert!(workspaces.count() <= 102);
    assert_eq!(fixture.state.windows.len(), 101);

    // A client only sees the output that's there.
    let late = fixture.client();
    let outputs = late
        .data
        .globals
        .iter()
        .filter(|(_, interface, _)| interface == "wl_output")
        .count();
    assert_eq!(outputs, 1);
}
