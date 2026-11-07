//! Idle means idle: a change redraws only the outputs it shows on.

use std::time::Duration;

use smithay::backend::input::InputTime;

use super::{Fixture, TestClient, TestWindow, map_window};
use crate::workspaces::WorkspaceTarget;

/// Long enough for every open and switch animation to finish.
const SETTLE: Duration = Duration::from_millis(400);

/// Outputs A and B, and a window in the middle of A.
fn window_on_a() -> (Fixture, TestClient, TestWindow) {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let a = fixture.output("A");
    fixture.state.set_focused_output(&a);
    let mut client = fixture.client();
    let window = map_window(&mut fixture, &mut client);
    fixture.advance(SETTLE);
    (fixture, client, window)
}

/// Frames drawn on A and B while `f` runs and the loop settles.
fn redraws(fixture: &mut Fixture, f: impl FnOnce(&mut Fixture)) -> (u32, u32) {
    let before = (fixture.renders("A"), fixture.renders("B"));
    f(fixture);
    fixture.advance(Duration::from_millis(50));
    (
        fixture.renders("A") - before.0,
        fixture.renders("B") - before.1,
    )
}

#[test]
fn a_commit_on_output_a_does_not_redraw_output_b() {
    let (mut fixture, mut client, window) = window_on_a();
    let (a, b) = redraws(&mut fixture, |fixture| {
        client.attach(&window, (640, 480));
        fixture.roundtrip(&mut client);
    });
    assert!(a > 0, "the window's output is redrawn");
    assert_eq!(b, 0, "the other output stays idle");
}

#[test]
fn a_commit_on_a_hidden_workspace_redraws_nothing() {
    let (mut fixture, mut client, window) = window_on_a();
    let a = fixture.output("A");
    fixture
        .state
        .switch_workspace(&a, WorkspaceTarget::Index(1));
    fixture.advance(SETTLE);

    let redrawn = redraws(&mut fixture, |fixture| {
        client.attach(&window, (640, 480));
        fixture.roundtrip(&mut client);
    });
    assert_eq!(redrawn, (0, 0));
}

#[test]
fn a_window_growing_onto_output_b_redraws_both_and_shrinking_back_too() {
    let (mut fixture, mut client, window) = window_on_a();
    // Wider than A: it now reaches into B.
    let grown = redraws(&mut fixture, |fixture| {
        client.data.configured_size.clear();
        client.attach(&window, (3000, 480));
        fixture.roundtrip(&mut client);
    });
    assert!(grown.0 > 0 && grown.1 > 0, "{grown:?}");
    // And back: B must lose what the window left there.
    let shrunk = redraws(&mut fixture, |fixture| {
        client.data.configured_size.clear();
        client.attach(&window, (640, 480));
        fixture.roundtrip(&mut client);
    });
    assert!(shrunk.0 > 0 && shrunk.1 > 0, "{shrunk:?}");
}

#[test]
fn pointer_motion_redraws_only_the_outputs_under_the_cursor() {
    let (mut fixture, _client, _window) = window_on_a();
    let moved = redraws(&mut fixture, |fixture| {
        fixture
            .state
            .pointer_moved_to((300.0, 300.0).into(), InputTime::now());
    });
    assert_eq!(moved.1, 0, "B is far from the cursor: {moved:?}");

    // Across the edge: both.
    let crossed = redraws(&mut fixture, |fixture| {
        fixture
            .state
            .pointer_moved_to((1930.0, 300.0).into(), InputTime::now());
    });
    assert!(crossed.0 > 0 && crossed.1 > 0, "{crossed:?}");
}
