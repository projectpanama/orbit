//! Window bookkeeping that is keyed by `WindowId`: minimizing and finding
//! windows by surface.

use std::time::Duration;

use smithay::desktop::Window;

use super::{Fixture, map_window};
use crate::{window::is_minimized, workspaces::WorkspaceTarget};

/// Maps a window and returns Orbit's side of it (a new window is focused).
fn open(fixture: &mut Fixture, client: &mut super::TestClient) -> Window {
    let _window = map_window(fixture, client);
    fixture
        .state
        .focused_window()
        .expect("a new window is focused")
}

fn shown(fixture: &Fixture, window: &Window) -> bool {
    fixture.state.space.elements().any(|w| w == window)
}

#[test]
fn minimized_windows_come_back_last_first() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut client = fixture.client();
    let first = open(&mut fixture, &mut client);
    let second = open(&mut fixture, &mut client);
    fixture.state.minimize(&first);
    fixture.state.minimize(&second);
    assert!(is_minimized(&first) && is_minimized(&second));
    assert!(!shown(&fixture, &first) && !shown(&fixture, &second));

    fixture.state.restore_minimized();
    assert!(shown(&fixture, &second) && !is_minimized(&second));
    assert!(!shown(&fixture, &first) && is_minimized(&first));
    fixture.state.restore_minimized();
    assert!(shown(&fixture, &first) && !is_minimized(&first));
}

#[test]
fn a_minimized_window_stays_hidden_across_workspace_switches() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    let mut client = fixture.client();
    let kept = open(&mut fixture, &mut client);
    let minimized = open(&mut fixture, &mut client);
    fixture.state.minimize(&minimized);

    fixture
        .state
        .switch_workspace(&a, WorkspaceTarget::Index(1));
    fixture.advance(Duration::from_millis(400));
    fixture
        .state
        .switch_workspace(&a, WorkspaceTarget::Index(0));
    fixture.advance(Duration::from_millis(400));
    assert!(shown(&fixture, &kept));
    assert!(!shown(&fixture, &minimized) && is_minimized(&minimized));
    // Not offered focus while minimized.
    assert_eq!(fixture.state.focused_window().as_ref(), Some(&kept));
}

#[test]
fn a_surface_finds_its_window_until_the_window_closes() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut client = fixture.client();
    let test_window = map_window(&mut fixture, &mut client);
    let window = fixture.state.focused_window().expect("focused");
    let surface = window.toplevel().expect("a toplevel").wl_surface().clone();
    assert_eq!(fixture.state.window_for_surface(&surface), Some(window));

    // Only the toplevel role goes: the surface itself outlives it.
    test_window.toplevel.destroy();
    fixture.roundtrip(&mut client);
    assert_eq!(fixture.state.window_for_surface(&surface), None);
    assert!(fixture.state.window_surfaces.is_empty());
}

/// `Space::refresh` (which tells surfaces what outputs they are on) only
/// runs when something changed; a window moved by Orbit, without any
/// commit, must still count.
#[test]
fn the_space_follows_a_window_moved_without_a_commit() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    fixture.state.set_focused_output(&a);
    let mut client = fixture.client();
    let window = open(&mut fixture, &mut client);
    fixture.advance(Duration::from_millis(400));
    let outputs = |fixture: &Fixture| fixture.state.space.outputs_for_element(&window);
    assert_eq!(outputs(&fixture), vec![a.clone()]);

    let b_geo = fixture
        .state
        .space
        .output_geometry(&b)
        .expect("B is mapped");
    let to = b_geo.loc + smithay::utils::Point::from((100, 100));
    fixture.state.space.map_element(window.clone(), to, false);
    fixture.state.queue_redraw_all();
    fixture.advance(Duration::from_millis(50));
    assert_eq!(outputs(&fixture), vec![b]);
}

/// A window committing a new size that reaches output B is on both.
#[test]
fn the_space_follows_a_window_growing_by_a_commit() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    fixture.state.set_focused_output(&a);
    let mut client = fixture.client();
    let test_window = map_window(&mut fixture, &mut client);
    let window = fixture.state.focused_window().expect("focused");
    fixture.advance(Duration::from_millis(400));

    client.attach(&test_window, (3000, 480));
    fixture.roundtrip(&mut client);
    fixture.advance(Duration::from_millis(50));
    let mut outputs = fixture.state.space.outputs_for_element(&window);
    outputs.sort_by_key(|o| o.name());
    assert_eq!(outputs, vec![a, b]);
}

/// Minimizing or closing the focused window focuses the one its
/// workspace would focus next (the newest usable one), and restoring a
/// minimized window focuses it.
#[test]
fn focus_moves_on_after_minimize_close_and_restore() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut client = fixture.client();
    let _a = open(&mut fixture, &mut client);
    let b = open(&mut fixture, &mut client);
    let c_window = map_window(&mut fixture, &mut client);
    let c = fixture
        .state
        .focused_window()
        .expect("a new window is focused");
    let focused = |fixture: &Fixture| fixture.state.focused_window();

    fixture.state.minimize(&c);
    assert_eq!(focused(&fixture).as_ref(), Some(&b));
    fixture.state.restore_minimized();
    assert_eq!(focused(&fixture).as_ref(), Some(&c));

    c_window.toplevel.destroy();
    fixture.roundtrip(&mut client);
    assert_eq!(focused(&fixture).as_ref(), Some(&b));
}
