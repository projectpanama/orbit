//! Client isolation: what one client does must not affect Orbit or other
//! clients, and a client that goes away must leave nothing behind.

use smithay::{reexports::wayland_server::Resource, utils::Serial};
use wayland_protocols::xdg::shell::client::xdg_positioner::XdgPositioner;

use super::{Fixture, map_window};
use crate::{
    state::Orbit,
    window::{Layout, Side},
};

/// Runs Orbit until it has handled a client's disconnect: under load the
/// hang-up can arrive a turn or two after the client closed its socket.
fn dispatch_disconnect(fixture: &mut Fixture) {
    for _ in 0..20 {
        fixture.dispatch();
        if fixture.state.windows.is_empty() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Checks Orbit holds nothing for windows any more.
fn assert_no_windows(state: &Orbit) {
    assert!(state.windows.is_empty(), "windows: {}", state.windows.len());
    assert_eq!(state.space.elements().count(), 0);
    assert!(state.window_surfaces.is_empty());
    assert!(state.desktop.toplevel_handles.is_empty());
    assert!(state.input.last_titlebar_click.is_none());
    assert!(state.input.decoration_hover.is_none());
    assert!(state.focused_window().is_none());
    for output in state.workspaces.output_ids() {
        // Only the trailing empty workspace is left.
        assert_eq!(state.workspaces.workspace_ids(output).count(), 1);
        assert!(state.workspaces.active_windows(output).is_empty());
    }
}

#[test]
fn windows_go_away_with_their_client() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut client = fixture.client();
    let first = map_window(&mut fixture, &mut client);
    let _second = map_window(&mut fixture, &mut client);
    let popup = client.create_popup(&first);
    fixture.roundtrip(&mut client);
    client.attach_popup(&popup);
    fixture.roundtrip(&mut client);
    assert_eq!(fixture.state.windows.len(), 2);
    assert_eq!(fixture.state.desktop.toplevel_handles.len(), 2);

    // Minimize one, and leave a title-bar click and hover on the other
    // behind.
    let windows: Vec<_> = fixture.state.windows.values().cloned().collect();
    fixture.state.minimize(&windows[0]);
    let clicked = crate::workspaces::window_id(&windows[1]).expect("registered");
    fixture.state.input.last_titlebar_click = Some((clicked, std::time::Instant::now()));
    fixture.state.input.decoration_hover = Some((clicked, orbit_config::Button::Close));

    // The client vanishes without destroying anything.
    drop(client);
    dispatch_disconnect(&mut fixture);
    assert_no_windows(&fixture.state);
}

#[test]
fn a_protocol_error_only_disconnects_that_client() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut good = fixture.client();
    let _kept = map_window(&mut fixture, &mut good);
    let focused = fixture.state.focused_window();
    assert!(focused.is_some());

    let mut bad = fixture.client();
    let _window = map_window(&mut fixture, &mut bad);
    // An invalid positioner size is a protocol error.
    let qh = bad.qh();
    let positioner: XdgPositioner = bad.wm_base().create_positioner(&qh, ());
    positioner.set_size(-1, -1);
    assert!(fixture.try_roundtrip(&mut bad).is_err());
    assert!(bad.conn.protocol_error().is_some());
    fixture.dispatch();
    assert!(!fixture.is_connected(&bad.server));

    // The other client and its window are untouched, and focus is back on
    // it.
    fixture.roundtrip(&mut good);
    assert!(fixture.is_connected(&good.server));
    assert_eq!(fixture.state.windows.len(), 1);
    assert_eq!(fixture.state.focused_window(), focused);
}

#[test]
fn extreme_size_hints_dont_crash() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut client = fixture.client();
    let window = map_window(&mut fixture, &mut client);

    // A minimum bigger than any output and a maximum below it.
    window.toplevel.set_min_size(i32::MAX, i32::MAX);
    window.toplevel.set_max_size(1, 1);
    window.surface.commit();
    fixture.roundtrip(&mut client);

    let server_window = fixture
        .state
        .windows
        .values()
        .next()
        .cloned()
        .expect("window");
    for layout in [
        Layout::Maximized,
        Layout::Tiled(Side::Left),
        Layout::Floating,
    ] {
        fixture.state.set_layout(&server_window, layout);
        fixture.roundtrip(&mut client);
    }
    fixture.state.fullscreen(&server_window);
    fixture.roundtrip(&mut client);
    fixture.state.unfullscreen(&server_window);
    fixture.roundtrip(&mut client);

    // A buffer far bigger than the output, then the keyboard bindings that
    // build on the window's size and position.
    client.attach(&window, (i32::MAX, i32::MAX));
    fixture.roundtrip(&mut client);
    for _ in 0..3 {
        fixture.state.resize_by(&server_window, 1, 1);
        fixture.state.move_by(&server_window, 1, 1);
        fixture.state.move_by(&server_window, -1, -1);
        fixture.roundtrip(&mut client);
    }
    fixture
        .state
        .focus_window(Some(&server_window), Serial::from(0));
    fixture.state.queue_redraw_all();
    fixture.roundtrip(&mut client);
    assert_eq!(fixture.state.windows.len(), 1);
}

#[test]
fn many_clients_come_and_go_without_leaking() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let mut last_id = None;
    for round in 0..200 {
        let mut client = fixture.client();
        let first = map_window(&mut fixture, &mut client);
        let _second = map_window(&mut fixture, &mut client);
        if round % 2 == 0 {
            let popup = client.create_popup(&first);
            fixture.roundtrip(&mut client);
            client.attach_popup(&popup);
            fixture.roundtrip(&mut client);
        }
        // IDs keep counting up; none is ever handed out twice.
        let newest = fixture.state.windows.keys().max().copied();
        assert!(newest > last_id);
        last_id = newest;
        drop(client);
        dispatch_disconnect(&mut fixture);
        assert_no_windows(&fixture.state);
    }
}

#[test]
fn a_client_that_stops_reading_doesnt_stall_orbit() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut good = fixture.client();
    let _kept = map_window(&mut fixture, &mut good);
    let mut stalled = fixture.client();
    let _window = map_window(&mut fixture, &mut stalled);
    let toplevel = fixture
        .state
        .windows
        .values()
        .filter_map(|window| window.toplevel().cloned())
        .find(|toplevel| toplevel.wl_surface().client().as_ref() == Some(&stalled.server))
        .expect("stalled client's toplevel");

    // Far more than fits in the socket buffer, while the client reads
    // none of it. wayland-backend keeps at most a few KiB queued per
    // client on top of the socket's own buffer, and disconnects a client
    // whose queue is full, so Orbit's memory doesn't grow and it never
    // waits for the client.
    for _ in 0..20_000 {
        toplevel.send_configure();
        fixture.dispatch();
    }
    assert!(fixture.try_roundtrip(&mut stalled).is_err());
    fixture.dispatch();
    assert_eq!(fixture.state.windows.len(), 1);

    // The other client is served as usual.
    fixture.roundtrip(&mut good);
    assert!(fixture.is_connected(&good.server));
}
