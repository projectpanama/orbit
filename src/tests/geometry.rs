//! Where windows end up: layouts and their reversals, moving between
//! outputs by keyboard and by dragging, and outputs going away under
//! laid-out windows.
//!
//! The test client draws its own decorations, so a window's frame is its
//! content here.

use std::time::Duration;

use smithay::{
    backend::input::{ButtonState, InputTime},
    desktop::Window,
    input::pointer::{ButtonEvent, GrabStartData},
    output::Output,
    utils::{Logical, Rectangle, SERIAL_COUNTER},
};

use super::{Fixture, TestClient, client::TestWindow, map_window};
use crate::{
    window::{Layout, Side, is_fullscreen},
    workspaces::{WorkspaceTarget, window_id},
};

const BTN_LEFT: u32 = 0x110;

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
    Rectangle::new((x, y).into(), (w, h).into())
}

/// Opens a window on `output` and returns both sides of it.
fn open_on(
    fixture: &mut Fixture,
    client: &mut TestClient,
    output: &Output,
) -> (TestWindow, Window) {
    fixture.state.set_focused_output(output);
    let test_window = map_window(fixture, client);
    let window = fixture
        .state
        .focused_window()
        .expect("a new window is focused");
    (test_window, window)
}

/// Lets the client answer Orbit's latest configure at the size it asked
/// for, as a real client would.
fn settle(fixture: &mut Fixture, client: &mut TestClient, window: &TestWindow) {
    fixture.roundtrip(client);
    client.attach(window, (640, 480));
    fixture.roundtrip(client);
}

fn frame(fixture: &Fixture, window: &Window) -> Rectangle<i32, Logical> {
    fixture
        .state
        .frame_geometry(window)
        .expect("the window is shown")
}

/// Starts dragging `window` with the pointer at `at`, as a click on its
/// title bar would.
fn start_drag(fixture: &mut Fixture, window: &Window, at: (f64, f64)) {
    fixture.state.pointer_moved_to(at.into(), InputTime::now());
    let start = GrabStartData {
        focus: None,
        button: BTN_LEFT,
        location: at.into(),
    };
    fixture
        .state
        .start_move(window.clone(), start, SERIAL_COUNTER.next_serial());
}

fn drag_to(fixture: &mut Fixture, at: (f64, f64)) {
    fixture.state.pointer_moved_to(at.into(), InputTime::now());
}

fn release(fixture: &mut Fixture) {
    let pointer = fixture.state.pointer.clone();
    pointer.button(
        &mut fixture.state,
        &ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::now(),
            button: BTN_LEFT,
            state: ButtonState::Released,
        },
    );
    pointer.frame(&mut fixture.state);
}

#[test]
fn layouts_fill_their_area_and_floating_comes_back() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    let mut client = fixture.client();
    let (test_window, window) = open_on(&mut fixture, &mut client, &a);
    let floating = frame(&fixture, &window);
    let output = rect(0, 0, 1920, 1080);

    let steps: [(Layout, Rectangle<i32, Logical>); 3] = [
        (Layout::Maximized, output),
        (Layout::Tiled(Side::Left), rect(0, 0, 960, 1080)),
        (Layout::Tiled(Side::Right), rect(960, 0, 960, 1080)),
    ];
    for (layout, area) in steps {
        fixture.state.set_layout(&window, layout);
        settle(&mut fixture, &mut client, &test_window);
        assert_eq!(frame(&fixture, &window), area, "{layout:?}");
        assert_eq!(window.geometry().size, area.size, "{layout:?}: drawn size");

        fixture.state.set_layout(&window, Layout::Floating);
        settle(&mut fixture, &mut client, &test_window);
        assert_eq!(frame(&fixture, &window), floating, "back from {layout:?}");
    }

    // Fullscreen covers the output, and leaving it restores the floating
    // geometry.
    fixture.state.fullscreen(&window);
    settle(&mut fixture, &mut client, &test_window);
    assert_eq!(frame(&fixture, &window), output);
    fixture.state.unfullscreen(&window);
    settle(&mut fixture, &mut client, &test_window);
    assert_eq!(frame(&fixture, &window), floating);
}

#[test]
fn fullscreen_over_a_maximized_window_returns_to_maximized_then_floating() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    let mut client = fixture.client();
    let (test_window, window) = open_on(&mut fixture, &mut client, &a);
    let floating = frame(&fixture, &window);

    fixture.state.maximize(&window);
    settle(&mut fixture, &mut client, &test_window);
    fixture.state.fullscreen(&window);
    settle(&mut fixture, &mut client, &test_window);
    assert!(is_fullscreen(&window));

    fixture.state.unfullscreen(&window);
    settle(&mut fixture, &mut client, &test_window);
    assert_eq!(crate::state::Orbit::layout(&window), Layout::Maximized);
    assert_eq!(frame(&fixture, &window), rect(0, 0, 1920, 1080));

    fixture.state.unmaximize(&window);
    settle(&mut fixture, &mut client, &test_window);
    assert_eq!(frame(&fixture, &window), floating);
}

#[test]
fn moving_to_another_output_keeps_the_relative_position() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    // B is smaller, so the position scales.
    let b = fixture.state.add_headless_output("B", (960, 540), 1.0);
    fixture.dispatch();
    let mut client = fixture.client();
    let (test_window, window) = open_on(&mut fixture, &mut client, &a);
    client.attach(&test_window, (200, 100));
    fixture.roundtrip(&mut client);
    fixture
        .state
        .space
        .map_element(window.clone(), (480, 270), false);

    fixture.state.move_window_to_output(&window, &b);
    assert_eq!(fixture.state.window_output(&window).as_ref(), Some(&b));
    assert_eq!(frame(&fixture, &window), rect(1920 + 240, 135, 200, 100));
    assert_eq!(fixture.state.focused_window().as_ref(), Some(&window));

    // Too close to the far corner to fit at the scaled spot: kept whole on
    // the output instead.
    fixture
        .state
        .space
        .map_element(window.clone(), (1920 + 900, 500), false);
    fixture.state.move_window_to_output(&window, &a);
    assert_eq!(frame(&fixture, &window), rect(1720, 980, 200, 100));
}

#[test]
fn a_maximized_window_moved_to_another_output_fills_that_one() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    let b = fixture.state.add_headless_output("B", (1280, 720), 1.0);
    fixture.dispatch();
    let mut client = fixture.client();
    let (test_window, window) = open_on(&mut fixture, &mut client, &a);
    let floating = frame(&fixture, &window);
    fixture.state.maximize(&window);
    settle(&mut fixture, &mut client, &test_window);

    fixture.state.move_window_to_output(&window, &b);
    settle(&mut fixture, &mut client, &test_window);
    assert_eq!(frame(&fixture, &window), rect(1920, 0, 1280, 720));

    // Floating again, it is at the same relative spot on B, scaled.
    fixture.state.unmaximize(&window);
    settle(&mut fixture, &mut client, &test_window);
    let restored = frame(&fixture, &window);
    assert_eq!(restored.size, floating.size);
    assert!(
        rect(1920, 0, 1280, 720).contains_rect(restored),
        "{restored:?}"
    );
}

#[test]
fn dragging_a_maximized_window_keeps_the_pointer_at_the_same_spot_along_it() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    let mut client = fixture.client();
    let (test_window, window) = open_on(&mut fixture, &mut client, &a);
    let floating = frame(&fixture, &window);
    fixture.state.maximize(&window);
    settle(&mut fixture, &mut client, &test_window);

    // Three quarters along the maximized width.
    start_drag(&mut fixture, &window, (1440.0, 10.0));
    assert_eq!(crate::state::Orbit::layout(&window), Layout::Floating);
    let x = fixture
        .state
        .space
        .element_location(&window)
        .expect("shown")
        .x;
    // Three quarters along the floating width too.
    assert_eq!(x, 1440 - floating.size.w * 3 / 4);
    release(&mut fixture);
}

#[test]
fn dragging_a_window_across_outputs_moves_it_there() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    let mut client = fixture.client();
    let (_test_window, window) = open_on(&mut fixture, &mut client, &a);
    fixture
        .state
        .space
        .map_element(window.clone(), (400, 300), false);

    start_drag(&mut fixture, &window, (500.0, 310.0));
    // Across the shared edge, away from every edge B has.
    drag_to(&mut fixture, (1500.0, 400.0));
    drag_to(&mut fixture, (2500.0, 410.0));
    release(&mut fixture);

    assert_eq!(frame(&fixture, &window).loc, (2400, 400).into());
    assert_eq!(fixture.state.window_output(&window).as_ref(), Some(&b));
    let id = window_id(&window).expect("an id");
    assert_eq!(
        fixture.state.workspaces.output_of(id),
        fixture.state.output_id(&b)
    );
    assert_ne!(fixture.state.window_output(&window).as_ref(), Some(&a));
}

#[test]
fn dropping_at_an_edge_applies_its_layout_on_that_output() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    let mut client = fixture.client();
    let (test_window, window) = open_on(&mut fixture, &mut client, &a);
    fixture
        .state
        .space
        .map_element(window.clone(), (400, 300), false);

    // The top edge of B maximizes there.
    start_drag(&mut fixture, &window, (500.0, 310.0));
    drag_to(&mut fixture, (2800.0, 2.0));
    assert!(fixture.state.overlays.snap_preview.is_some());
    release(&mut fixture);
    settle(&mut fixture, &mut client, &test_window);
    assert_eq!(crate::state::Orbit::layout(&window), Layout::Maximized);
    assert_eq!(frame(&fixture, &window), rect(1920, 0, 1920, 1080));
    assert_eq!(fixture.state.window_output(&window).as_ref(), Some(&b));
    assert!(fixture.state.overlays.snap_preview.is_none());

    // Dragged back to A's left edge, it tiles there.
    start_drag(&mut fixture, &window, (2800.0, 10.0));
    drag_to(&mut fixture, (2.0, 500.0));
    release(&mut fixture);
    settle(&mut fixture, &mut client, &test_window);
    assert_eq!(
        crate::state::Orbit::layout(&window),
        Layout::Tiled(Side::Left)
    );
    assert_eq!(frame(&fixture, &window), rect(0, 0, 960, 1080));
    assert_eq!(fixture.state.window_output(&window).as_ref(), Some(&a));
}

/// Shows on `output` the workspace `window` is on.
fn show_workspace_of(fixture: &mut Fixture, output: &Output, window: &Window) {
    let id = window_id(window).expect("an id");
    let (workspace, _) = fixture
        .state
        .workspaces
        .workspace_of(id)
        .expect("on a workspace");
    let (_, index) = fixture
        .state
        .workspaces
        .find_workspace(workspace)
        .expect("the workspace exists");
    fixture
        .state
        .switch_workspace(output, WorkspaceTarget::Index(index));
    fixture.advance(Duration::from_secs(1));
}

#[test]
fn laid_out_windows_refit_to_the_output_they_land_on() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let a = fixture.output("A");
    let b = fixture.state.add_headless_output("B", (1280, 720), 1.0);
    fixture.dispatch();
    let mut client = fixture.client();
    let (maximized_test, maximized) = open_on(&mut fixture, &mut client, &b);
    fixture.state.maximize(&maximized);
    settle(&mut fixture, &mut client, &maximized_test);
    let (tiled_test, tiled) = open_on(&mut fixture, &mut client, &b);
    fixture.state.set_layout(&tiled, Layout::Tiled(Side::Right));
    settle(&mut fixture, &mut client, &tiled_test);
    assert_eq!(frame(&fixture, &tiled), rect(1920 + 640, 0, 640, 720));

    fixture.remove_output(&b);
    // B's workspace waits on A, behind the one A shows.
    show_workspace_of(&mut fixture, &a, &maximized);
    settle(&mut fixture, &mut client, &maximized_test);
    settle(&mut fixture, &mut client, &tiled_test);
    assert_eq!(fixture.state.window_output(&maximized).as_ref(), Some(&a));
    assert_eq!(frame(&fixture, &maximized), rect(0, 0, 1920, 1080));
    assert_eq!(frame(&fixture, &tiled), rect(960, 0, 960, 1080));
}

#[test]
fn unplugging_the_focused_output_moves_focus_to_another() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    let mut client = fixture.client();
    let (_, on_a) = open_on(&mut fixture, &mut client, &a);
    let (_, _on_b) = open_on(&mut fixture, &mut client, &b);
    assert_eq!(fixture.state.focused_output().as_ref(), Some(&b));

    fixture.remove_output(&b);
    fixture.roundtrip(&mut client);
    assert_eq!(fixture.state.focused_output().as_ref(), Some(&a));
    // What A shows gets the keyboard; B's window waits out of sight.
    assert_eq!(fixture.state.focused_window().as_ref(), Some(&on_a));
}

#[test]
fn a_drag_ends_cleanly_when_its_output_goes_away() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let (a, b) = (fixture.output("A"), fixture.output("B"));
    let mut client = fixture.client();
    let (_, window) = open_on(&mut fixture, &mut client, &b);
    fixture
        .state
        .space
        .map_element(window.clone(), (2300, 300), false);
    start_drag(&mut fixture, &window, (2400.0, 310.0));
    drag_to(&mut fixture, (2600.0, 400.0));

    fixture.remove_output(&b);
    // The pointer was put back on A; the next motion finds the window
    // gone from view and ends the drag.
    drag_to(&mut fixture, (500.0, 500.0));
    release(&mut fixture);
    fixture.roundtrip(&mut client);
    assert!(!fixture.state.pointer.is_grabbed());
    assert!(fixture.state.overlays.snap_preview.is_none());
    assert_eq!(fixture.state.window_output(&window).as_ref(), Some(&a));

    // Shown again, it is on A.
    show_workspace_of(&mut fixture, &a, &window);
    let frame = frame(&fixture, &window);
    assert!(rect(0, 0, 1920, 1080).contains_rect(frame), "{frame:?}");
}
