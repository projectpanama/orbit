//! Screen and window capture (ext-image-capture-source-v1 and
//! ext-image-copy-capture-v1) into wl_shm buffers, rendered by the
//! headless backend's CPU renderer.

use std::{collections::HashMap, fs::File, io::Read as _, os::fd::AsFd as _};

use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    backend::ObjectId,
    delegate_noop,
    protocol::{
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    },
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};

use super::{Fixture, TestClient, client::ClientData, map_window, session::bind_all_outputs};

#[derive(Default)]
pub struct Captures {
    pub sessions: HashMap<ObjectId, SessionInfo>,
    /// Per frame: "ready", or "failed:<reason>".
    pub frames: HashMap<ObjectId, String>,
}

#[derive(Default, Debug, Clone)]
pub struct SessionInfo {
    pub size: Option<(u32, u32)>,
    pub formats: Vec<u32>,
    pub done: bool,
    pub stopped: bool,
}

struct Shot {
    file: File,
    buffer: wayland_client::protocol::wl_buffer::WlBuffer,
    size: (i32, i32),
}

fn shm_buffer(client: &mut TestClient, shm: &WlShm, size: (u32, u32)) -> Shot {
    let qh = client.qh();
    let (w, h) = (size.0 as i32, size.1 as i32);
    let len = w * h * 4;
    let path = std::env::temp_dir().join(format!(
        "orbit-capture-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("temp file");
    let _ = std::fs::remove_file(&path);
    file.set_len(len as u64).expect("size");
    let pool: WlShmPool = shm.create_pool(file.as_fd(), len, &qh, ());
    let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &qh, ());
    pool.destroy();
    Shot {
        file,
        buffer,
        size: (w, h),
    }
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

fn pixels(shot: &mut Shot) -> Vec<u8> {
    let mut v = Vec::new();
    use std::io::Seek as _;
    shot.file.rewind().expect("rewind");
    shot.file.read_to_end(&mut v).expect("read");
    v
}

struct Capturer {
    shm: WlShm,
    outputs: ExtOutputImageCaptureSourceManagerV1,
    toplevels: ExtForeignToplevelImageCaptureSourceManagerV1,
    copy: ExtImageCopyCaptureManagerV1,
    _list: ExtForeignToplevelListV1,
}

fn bind(fixture: &mut Fixture, client: &mut TestClient) -> Capturer {
    let qh = client.qh();
    let registry = client.registry();
    let c = Capturer {
        shm: client.bind(&registry, "wl_shm", 1, &qh),
        outputs: client.bind(
            &registry,
            "ext_output_image_capture_source_manager_v1",
            1,
            &qh,
        ),
        toplevels: client.bind(
            &registry,
            "ext_foreign_toplevel_image_capture_source_manager_v1",
            1,
            &qh,
        ),
        copy: client.bind(&registry, "ext_image_copy_capture_manager_v1", 1, &qh),
        _list: client.bind(&registry, "ext_foreign_toplevel_list_v1", 1, &qh),
    };
    fixture.roundtrip(client);
    c
}

fn session_info(client: &TestClient, s: &ExtImageCopyCaptureSessionV1) -> SessionInfo {
    client
        .data
        .capture
        .sessions
        .get(&s.id())
        .cloned()
        .unwrap_or_default()
}

fn capture(
    fixture: &mut Fixture,
    client: &mut TestClient,
    c: &Capturer,
    session: &ExtImageCopyCaptureSessionV1,
) -> (Shot, ExtImageCopyCaptureFrameV1) {
    let info = session_info(client, session);
    let size = info.size.expect("buffer size sent");
    let shot = shm_buffer(client, &c.shm, size);
    let qh = client.qh();
    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&shot.buffer);
    frame.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
    frame.capture();
    fixture.roundtrip(client);
    fixture.roundtrip(client);
    (shot, frame)
}

#[test]
fn captures_an_output_into_shm() {
    // Without the open fade, so the window is fully drawn at once (the
    // headless backend never draws the frame that would start it).
    let mut config = orbit_config::Config::default();
    config.animations.window_open = orbit_core::animation::Timing::OFF;
    let mut fixture = Fixture::with_config(config);
    fixture.add_output("A");
    let mut apps = fixture.client();
    let _window = map_window(&mut fixture, &mut apps);

    let mut client = fixture.client();
    let c = bind(&mut fixture, &mut client);
    let outputs = bind_all_outputs(&mut client);
    fixture.roundtrip(&mut client);
    let qh = client.qh();
    let source: ExtImageCaptureSourceV1 = c.outputs.create_source(&outputs[0], &qh, ());
    let session = c.copy.create_session(&source, Options::empty(), &qh, ());
    fixture.roundtrip(&mut client);

    let info = session_info(&client, &session);
    assert_eq!(info.size, Some((1920, 1080)), "the output's size in pixels");
    assert!(info.formats.contains(&(wl_shm::Format::Argb8888 as u32)));
    assert!(info.formats.contains(&(wl_shm::Format::Xrgb8888 as u32)));
    assert!(info.done);

    let (mut shot, frame) = capture(&mut fixture, &mut client, &c, &session);
    assert_eq!(
        client
            .data
            .capture
            .frames
            .get(&frame.id())
            .map(String::as_str),
        Some("ready")
    );
    // The (opaque black) window was rendered; the empty background is
    // transparent.
    let px = pixels(&mut shot);
    let geo = fixture
        .state
        .space
        .elements()
        .next()
        .and_then(|w| fixture.state.space.element_geometry(w))
        .expect("window placed");
    let at = |x: i32, y: i32| {
        let i = ((y * shot.size.0 + x) * 4) as usize;
        px[i + 3]
    };
    let inside = (geo.loc.x + geo.size.w / 2, geo.loc.y + geo.size.h / 2);
    assert_eq!(at(inside.0, inside.1), 255, "window pixels are opaque");
    let corner = if geo.loc.x > 0 {
        (0, 1079)
    } else {
        (1919, 1079)
    };
    assert_eq!(at(corner.0, corner.1), 0, "background is transparent");
}

#[test]
fn captures_a_window_and_stops_when_it_closes() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut apps = fixture.client();
    let window = map_window(&mut fixture, &mut apps);

    let mut client = fixture.client();
    let c = bind(&mut fixture, &mut client);
    fixture.roundtrip(&mut client);
    let handle = client
        .data
        .desktop
        .toplevels
        .first()
        .cloned()
        .expect("window listed");
    let qh = client.qh();
    let source = c.toplevels.create_source(&handle, &qh, ());
    let session = c.copy.create_session(&source, Options::empty(), &qh, ());
    fixture.roundtrip(&mut client);
    let info = session_info(&client, &session);
    let geo = fixture
        .state
        .space
        .elements()
        .next()
        .map(|w| w.geometry())
        .expect("window");
    // Minimized, it can still be captured (from its last buffer).
    let mapped = fixture
        .state
        .windows
        .values()
        .next()
        .cloned()
        .expect("window");
    fixture.state.minimize(&mapped);
    fixture.roundtrip(&mut client);
    assert_eq!(info.size, Some((geo.size.w as u32, geo.size.h as u32)));

    let (mut shot, frame) = capture(&mut fixture, &mut client, &c, &session);
    assert_eq!(
        client
            .data
            .capture
            .frames
            .get(&frame.id())
            .map(String::as_str),
        Some("ready")
    );
    let px = pixels(&mut shot);
    assert_eq!(px[3], 255, "the window's own pixels");

    // The window goes away: the session stops.
    window.toplevel.destroy();
    window.surface.destroy();
    fixture.roundtrip(&mut apps);
    fixture.roundtrip(&mut client);
    fixture.roundtrip(&mut client);
    assert!(session_info(&client, &session).stopped);
    assert_eq!(fixture.state.capture.session_count(), 0);
}

#[test]
fn a_stale_toplevel_source_has_no_constraints() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut apps = fixture.client();
    let window = map_window(&mut fixture, &mut apps);
    let mut client = fixture.client();
    let c = bind(&mut fixture, &mut client);
    let handle = client
        .data
        .desktop
        .toplevels
        .first()
        .cloned()
        .expect("window listed");
    window.toplevel.destroy();
    window.surface.destroy();
    fixture.roundtrip(&mut apps);
    fixture.roundtrip(&mut client);
    let qh = client.qh();
    let source = c.toplevels.create_source(&handle, &qh, ());
    let session = c.copy.create_session(&source, Options::empty(), &qh, ());
    fixture.roundtrip(&mut client);
    fixture.roundtrip(&mut client);
    assert!(session_info(&client, &session).stopped);
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event as E;
        let info = state.capture.sessions.entry(session.id()).or_default();
        match event {
            E::BufferSize { width, height } => info.size = Some((width, height)),
            E::ShmFormat {
                format: WEnum::Value(f),
            } => info.formats.push(f as u32),
            E::Done => info.done = true,
            E::Stopped => info.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event as E;
        match event {
            E::Ready => {
                state.capture.frames.insert(frame.id(), "ready".into());
            }
            E::Failed { reason } => {
                state
                    .capture
                    .frames
                    .insert(frame.id(), format!("failed:{reason:?}"));
            }
            _ => {}
        }
    }
}

delegate_noop!(ClientData: ignore WlShm);
delegate_noop!(ClientData: ignore WlShmPool);
delegate_noop!(ClientData: ignore ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(ClientData: ignore ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(ClientData: ignore ExtImageCaptureSourceV1);
delegate_noop!(ClientData: ignore ExtImageCopyCaptureManagerV1);
