//! Session protocols as a lock screen, an idle daemon and an input method
//! use them: ext-session-lock-v1, ext-idle-notify-v1, idle-inhibit and
//! text-input-v3 with input-method-v2.

use std::time::Duration;

use smithay::{backend::input::InputTime, reexports::wayland_server::Resource as _};
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface},
};
use wayland_protocols::{
    ext::{
        idle_notify::v1::client::{
            ext_idle_notification_v1::{self, ExtIdleNotificationV1},
            ext_idle_notifier_v1::ExtIdleNotifierV1,
        },
        session_lock::v1::client::{
            ext_session_lock_manager_v1::ExtSessionLockManagerV1,
            ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
            ext_session_lock_v1::{self, ExtSessionLockV1},
        },
    },
    wp::{
        idle_inhibit::zv1::client::{
            zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
            zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
        },
        text_input::zv3::client::{
            zwp_text_input_manager_v3::ZwpTextInputManagerV3, zwp_text_input_v3::ZwpTextInputV3,
        },
        viewporter::client::wp_viewport::WpViewport,
    },
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};

use super::{Fixture, TestClient, client::ClientData, map_window};

/// What the session protocols told a client.
#[derive(Default)]
pub struct Session {
    pub locked: bool,
    pub finished: bool,
    /// Latest lock surface configure: (serial, width, height).
    pub lock_configure: Option<(u32, u32, u32)>,
    pub idled: usize,
    pub resumed: usize,
    /// Input method: activations and deactivations seen at `done`.
    pub im_active: Option<bool>,
    pub im_pending_active: Option<bool>,
}

/// A lock screen client's objects.
struct Locker {
    lock: ExtSessionLockV1,
    surface: WlSurface,
    lock_surface: ExtSessionLockSurfaceV1,
    viewport: WpViewport,
}

pub(super) fn bind_all_outputs(client: &mut TestClient) -> Vec<WlOutput> {
    let qh = client.qh();
    let registry = client.registry();
    client
        .data
        .globals
        .iter()
        .filter(|(_, i, _)| i == "wl_output")
        .map(|&(name, _, v)| registry.bind::<WlOutput, _, _>(name, v.min(4), &qh, ()))
        .collect()
}

/// Asks for the lock and puts a lock surface on the (only) output.
fn lock(fixture: &mut Fixture, client: &mut TestClient) -> Locker {
    let qh = client.qh();
    let manager: ExtSessionLockManagerV1 =
        client.bind(&client.registry(), "ext_session_lock_manager_v1", 1, &qh);
    let outputs = bind_all_outputs(client);
    fixture.roundtrip(client);
    let lock = manager.lock(&qh, ());
    let surface = client.compositor().create_surface(&qh, ());
    let viewport = client.viewporter().get_viewport(&surface, &qh, ());
    let lock_surface = lock.get_lock_surface(&surface, &outputs[0], &qh, ());
    fixture.roundtrip(client);
    Locker {
        lock,
        surface,
        lock_surface,
        viewport,
    }
}

/// Acknowledges the configure and draws the lock surface at its size.
fn draw_lock(fixture: &mut Fixture, client: &mut TestClient, locker: &Locker) {
    let qh = client.qh();
    let (serial, w, h) = client
        .data
        .session
        .lock_configure
        .expect("lock surface configured");
    locker.lock_surface.ack_configure(serial);
    let buffer = client
        .pixels()
        .create_u32_rgba_buffer(0, 0, 0, u32::MAX, &qh, ());
    locker.viewport.set_destination(w as i32, h as i32);
    locker.surface.attach(Some(&buffer), 0, 0);
    locker.surface.commit();
    fixture.roundtrip(client);
}

/// Lets the outputs draw a few frames, then delivers what Orbit sent.
fn frames(fixture: &mut Fixture, client: &mut TestClient) {
    fixture.advance(Duration::from_millis(60));
    fixture.roundtrip(client);
}

fn focus_client(
    fixture: &Fixture,
) -> Option<smithay::reexports::wayland_server::backend::ClientId> {
    let focus = fixture.state.keyboard.current_focus()?;
    focus.client().map(|c| c.id())
}

#[test]
fn locking_hides_the_desktop_and_unlocking_restores_focus() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut app = fixture.client();
    let _window = map_window(&mut fixture, &mut app);
    let focused = fixture.state.focused_window();
    assert!(focused.is_some());

    let mut screen = fixture.client();
    let locker = lock(&mut fixture, &mut screen);
    assert!(fixture.state.session_lock.is_active());
    // Sized to the output.
    let (_, w, h) = screen.data.session.lock_configure.expect("configured");
    assert_eq!((w, h), (1920, 1080));
    draw_lock(&mut fixture, &mut screen, &locker);
    frames(&mut fixture, &mut screen);
    assert!(
        screen.data.session.locked,
        "locked after a frame on every output"
    );

    // The keyboard is the lock screen's; the window can't get it back.
    assert_eq!(focus_client(&fixture), Some(screen.server.id()));
    if let Some(window) = fixture.state.windows.values().next().cloned() {
        fixture.state.activate_window(&window);
    }
    assert_eq!(focus_client(&fixture), Some(screen.server.id()));
    // Nor the pointer: over the window it lands on the lock surface.
    fixture
        .state
        .pointer_moved_to((100.0, 100.0).into(), InputTime::now());
    let pointer_client = fixture
        .state
        .pointer
        .current_focus()
        .and_then(|s| s.client().map(|c| c.id()));
    assert_eq!(pointer_client, Some(screen.server.id()));

    locker.lock.unlock_and_destroy();
    fixture.roundtrip(&mut screen);
    assert!(!fixture.state.session_lock.is_active());
    assert_eq!(fixture.state.focused_window(), focused);
}

#[test]
fn a_crashed_locker_leaves_the_session_locked_until_another_locks() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut screen = fixture.client();
    let locker = lock(&mut fixture, &mut screen);
    draw_lock(&mut fixture, &mut screen, &locker);
    frames(&mut fixture, &mut screen);
    assert!(screen.data.session.locked);

    // A second client can't take a live lock.
    let mut rival = fixture.client();
    let _rival = lock(&mut fixture, &mut rival);
    assert!(rival.data.session.finished);

    // The locker dies: still locked.
    drop(locker);
    drop(screen);
    fixture.dispatch();
    frames(&mut fixture, &mut rival);
    assert!(fixture.state.session_lock.is_active());
    assert!(
        fixture
            .state
            .session_lock
            .surface_on(&fixture.output("A"))
            .is_none()
    );

    // A new locker takes over and can unlock.
    let mut again = fixture.client();
    let locker = lock(&mut fixture, &mut again);
    draw_lock(&mut fixture, &mut again, &locker);
    frames(&mut fixture, &mut again);
    assert!(again.data.session.locked);
    assert!(!again.data.session.finished);
    locker.lock.unlock_and_destroy();
    fixture.roundtrip(&mut again);
    assert!(!fixture.state.session_lock.is_active());
}

#[test]
fn idle_notifications_and_inhibitors() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut client = fixture.client();
    let window = map_window(&mut fixture, &mut client);
    let qh = client.qh();
    let registry = client.registry();
    let seat: WlSeat = client.bind(&registry, "wl_seat", 1, &qh);
    let notifier: ExtIdleNotifierV1 = client.bind(&registry, "ext_idle_notifier_v1", 1, &qh);
    let _notification = notifier.get_idle_notification(50, &seat, &qh, ());
    fixture.roundtrip(&mut client);

    fixture.advance(Duration::from_millis(120));
    fixture.roundtrip(&mut client);
    assert_eq!(client.data.session.idled, 1);

    // Input resumes it.
    fixture.state.note_activity();
    fixture.roundtrip(&mut client);
    assert_eq!(client.data.session.resumed, 1);

    // A shown window inhibiting keeps it from idling again.
    let inhibitors: ZwpIdleInhibitManagerV1 =
        client.bind(&registry, "zwp_idle_inhibit_manager_v1", 1, &qh);
    let inhibitor = inhibitors.create_inhibitor(&window.surface, &qh, ());
    fixture.roundtrip(&mut client);
    fixture.advance(Duration::from_millis(120));
    fixture.roundtrip(&mut client);
    assert_eq!(client.data.session.idled, 1);

    // Minimized, it no longer counts.
    let w = fixture
        .state
        .windows
        .values()
        .next()
        .cloned()
        .expect("window");
    fixture.state.minimize(&w);
    fixture.dispatch();
    fixture.advance(Duration::from_millis(120));
    fixture.roundtrip(&mut client);
    assert_eq!(client.data.session.idled, 2);
    inhibitor.destroy();
    fixture.roundtrip(&mut client);
}

#[test]
fn text_input_reaches_the_input_method() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut ime = fixture.client();
    let qh = ime.qh();
    let seat: WlSeat = ime.bind(&ime.registry(), "wl_seat", 1, &qh);
    let manager: ZwpInputMethodManagerV2 =
        ime.bind(&ime.registry(), "zwp_input_method_manager_v2", 1, &qh);
    let _im = manager.get_input_method(&seat, &qh, ());
    fixture.roundtrip(&mut ime);

    let mut app = fixture.client();
    let _window = map_window(&mut fixture, &mut app);
    let aqh = app.qh();
    let aseat: WlSeat = app.bind(&app.registry(), "wl_seat", 1, &aqh);
    let text: ZwpTextInputManagerV3 =
        app.bind(&app.registry(), "zwp_text_input_manager_v3", 1, &aqh);
    let input = text.get_text_input(&aseat, &aqh, ());
    fixture.roundtrip(&mut app);
    // Focus arrives after the text input exists.
    let window = fixture
        .state
        .windows
        .values()
        .next()
        .cloned()
        .expect("window");
    fixture.state.activate_window(&window);
    fixture.roundtrip(&mut app);

    input.enable();
    input.commit();
    fixture.roundtrip(&mut app);
    fixture.roundtrip(&mut ime);
    assert_eq!(ime.data.session.im_active, Some(true));

    input.disable();
    input.commit();
    fixture.roundtrip(&mut app);
    fixture.roundtrip(&mut ime);
    assert_eq!(ime.data.session.im_active, Some(false));
}

impl Dispatch<ExtSessionLockV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.session.locked = true,
            ext_session_lock_v1::Event::Finished => state.session.finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            state.session.lock_configure = Some((serial, width, height));
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.session.idled += 1,
            ext_idle_notification_v1::Event::Resumed => state.session.resumed += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.session.im_pending_active = Some(true),
            zwp_input_method_v2::Event::Deactivate => {
                state.session.im_pending_active = Some(false);
            }
            zwp_input_method_v2::Event::Done => {
                if let Some(active) = state.session.im_pending_active.take() {
                    state.session.im_active = Some(active);
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(ClientData: ignore WlOutput);
delegate_noop!(ClientData: ignore ExtSessionLockManagerV1);
delegate_noop!(ClientData: ignore ExtIdleNotifierV1);
delegate_noop!(ClientData: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(ClientData: ignore ZwpIdleInhibitorV1);
delegate_noop!(ClientData: ignore ZwpTextInputManagerV3);
delegate_noop!(ClientData: ignore ZwpTextInputV3);
delegate_noop!(ClientData: ignore ZwpInputMethodManagerV2);

#[test]
fn outputs_plugged_in_while_locked_get_lock_surfaces_too() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut screen = fixture.client();
    let locker = lock(&mut fixture, &mut screen);
    draw_lock(&mut fixture, &mut screen, &locker);
    frames(&mut fixture, &mut screen);
    assert!(screen.data.session.locked);

    // A second monitor: the client sees a new wl_output and covers it.
    fixture.add_output("B");
    fixture.roundtrip(&mut screen);
    let outputs = bind_all_outputs(&mut screen);
    fixture.roundtrip(&mut screen);
    let qh = screen.qh();
    let surface = screen.compositor().create_surface(&qh, ());
    let _lock_surface = locker.lock.get_lock_surface(&surface, &outputs[1], &qh, ());
    fixture.roundtrip(&mut screen);
    let (_, w, h) = screen.data.session.lock_configure.expect("configured");
    assert_eq!((w, h), (1920, 1080));
    assert!(
        fixture
            .state
            .session_lock
            .surface_on(&fixture.output("B"))
            .is_some()
    );

    // Unplugging it leaves the lock in place.
    let b = fixture.output("B");
    fixture.remove_output(&b);
    assert!(fixture.state.session_lock.is_active());
    assert!(fixture.state.session_lock.surface_on(&b).is_none());
}
