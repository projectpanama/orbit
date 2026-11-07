//! A small Wayland client for the tests: it opens windows from
//! single-pixel buffers (no shared memory needed) and records what Orbit
//! tells it.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use smithay::reexports::wayland_server::Client;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::WaylandError,
    delegate_noop,
    protocol::{
        wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
        wl_registry::WlRegistry, wl_surface::WlSurface,
    },
};
use wayland_protocols::{
    wp::{
        single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
        viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
    },
    xdg::shell::client::{
        xdg_popup::{self, XdgPopup},
        xdg_positioner::XdgPositioner,
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::{self, XdgToplevel},
        xdg_wm_base::{self, XdgWmBase},
    },
};

pub struct TestClient {
    pub conn: Connection,
    queue: EventQueue<ClientData>,
    pub data: ClientData,
    /// The client as Orbit sees it.
    pub server: Client,
}

/// A window's objects, and the last configure it got.
pub struct TestWindow {
    pub surface: WlSurface,
    pub xdg_surface: XdgSurface,
    pub toplevel: XdgToplevel,
    _viewport: WpViewport,
}

pub struct TestPopup {
    pub surface: WlSurface,
    _popup: XdgPopup,
    _xdg_surface: XdgSurface,
}

#[derive(Default)]
pub struct ClientData {
    pub globals: Vec<(u32, String, u32)>,
    compositor: Option<WlCompositor>,
    wm_base: Option<XdgWmBase>,
    viewporter: Option<WpViewporter>,
    pixels: Option<WpSinglePixelBufferManagerV1>,
    /// The latest unacknowledged configure serial, by xdg_surface.
    pending_serial: HashMap<u32, u32>,
    /// The size from the latest toplevel configure, by xdg_toplevel.
    pub configured_size: HashMap<u32, (i32, i32)>,
    /// Toplevels Orbit asked to close.
    pub closed: Vec<u32>,
    pub popups_done: Vec<u32>,
    /// What the desktop-integration protocols said (`protocols.rs`).
    pub desktop: super::protocols::Desktop,
    /// What the session lock, idle and input method protocols said
    /// (`session.rs`).
    pub session: super::session::Session,
    /// What the capture protocols said (`capture.rs`).
    pub capture: super::capture::Captures,
    /// What the overview protocol said (`overview.rs`).
    pub overview: super::overview::Overviews,
    /// What wlr-output-management said (`output_management.rs`).
    pub outputs: super::output_management::Heads,
}

impl TestClient {
    pub fn new(conn: Connection, server: Client) -> Self {
        let queue = conn.new_event_queue();
        conn.display().get_registry(&queue.handle(), ());
        Self {
            conn,
            queue,
            data: ClientData::default(),
            server,
        }
    }

    pub fn qh(&self) -> QueueHandle<ClientData> {
        self.queue.handle()
    }

    /// Binds the globals every window needs, once the registry arrived.
    pub fn bind_globals(&mut self) {
        let qh = self.qh();
        let registry = self.registry();
        self.data.compositor = Some(self.bind(&registry, "wl_compositor", 6, &qh));
        self.data.wm_base = Some(self.bind(&registry, "xdg_wm_base", 6, &qh));
        self.data.viewporter = Some(self.bind(&registry, "wp_viewporter", 1, &qh));
        self.data.pixels = Some(self.bind(&registry, "wp_single_pixel_buffer_manager_v1", 1, &qh));
    }

    pub fn registry(&self) -> WlRegistry {
        // Re-requesting is cheap and avoids keeping the first one around.
        self.conn.display().get_registry(&self.qh(), ())
    }

    /// Binds global `interface` at up to `version`.
    pub fn bind<I>(
        &self,
        registry: &WlRegistry,
        interface: &str,
        version: u32,
        qh: &QueueHandle<ClientData>,
    ) -> I
    where
        I: Proxy + 'static,
        ClientData: Dispatch<I, ()>,
    {
        let &(name, _, advertised) = self
            .data
            .globals
            .iter()
            .find(|(_, i, _)| i == interface)
            .unwrap_or_else(|| panic!("Orbit doesn't advertise {interface}"));
        registry.bind::<I, _, _>(name, version.min(advertised), qh, ())
    }

    /// Creates a toplevel and makes its initial commit. It maps once
    /// [`TestClient::attach`] is called after the first configure.
    pub fn create_window(&mut self) -> TestWindow {
        self.create_window_with(|_| {})
    }

    /// Like [`create_window`](Self::create_window), with `setup` run on the
    /// toplevel before its initial commit (a parent, size limits).
    pub fn create_window_with(&mut self, setup: impl FnOnce(&XdgToplevel)) -> TestWindow {
        let qh = self.qh();
        let surface = self.compositor().create_surface(&qh, ());
        let xdg_surface = self.wm_base().get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        let viewport = self.viewporter().get_viewport(&surface, &qh, ());
        setup(&toplevel);
        surface.commit();
        TestWindow {
            surface,
            xdg_surface,
            toplevel,
            _viewport: viewport,
        }
    }

    /// Acknowledges the latest configure and commits a `size` buffer (the
    /// configured size if Orbit chose one).
    pub fn attach(&mut self, window: &TestWindow, size: (i32, i32)) {
        let qh = self.qh();
        let size = self
            .data
            .configured_size
            .get(&window.toplevel.id().protocol_id())
            .copied()
            .filter(|&(w, h)| w > 0 && h > 0)
            .unwrap_or(size);
        self.ack(&window.xdg_surface);
        let buffer = self
            .pixels()
            .create_u32_rgba_buffer(0, 0, 0, u32::MAX, &qh, ());
        window._viewport.set_destination(size.0, size.1);
        window.surface.attach(Some(&buffer), 0, 0);
        window.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        window.surface.commit();
    }

    fn ack(&mut self, xdg_surface: &XdgSurface) {
        if let Some(serial) = self
            .data
            .pending_serial
            .remove(&xdg_surface.id().protocol_id())
        {
            xdg_surface.ack_configure(serial);
        }
    }

    /// Opens a popup on `parent`, mapped with a small buffer.
    pub fn create_popup(&mut self, parent: &TestWindow) -> TestPopup {
        let qh = self.qh();
        let positioner: XdgPositioner = self.wm_base().create_positioner(&qh, ());
        positioner.set_size(100, 100);
        positioner.set_anchor_rect(0, 0, 10, 10);
        let surface = self.compositor().create_surface(&qh, ());
        let xdg_surface = self.wm_base().get_xdg_surface(&surface, &qh, ());
        let popup = xdg_surface.get_popup(Some(&parent.xdg_surface), &positioner, &qh, ());
        positioner.destroy();
        surface.commit();
        TestPopup {
            surface,
            _popup: popup,
            _xdg_surface: xdg_surface,
        }
    }

    pub fn attach_popup(&mut self, popup: &TestPopup) {
        let qh = self.qh();
        self.ack(&popup._xdg_surface);
        let buffer = self
            .pixels()
            .create_u32_rgba_buffer(0, 0, 0, u32::MAX, &qh, ());
        popup.surface.attach(Some(&buffer), 0, 0);
        popup.surface.commit();
    }

    pub fn compositor(&self) -> &WlCompositor {
        self.data.compositor.as_ref().expect("wl_compositor bound")
    }

    pub fn wm_base(&self) -> &XdgWmBase {
        self.data.wm_base.as_ref().expect("xdg_wm_base bound")
    }

    pub fn viewporter(&self) -> &WpViewporter {
        self.data.viewporter.as_ref().expect("wp_viewporter bound")
    }

    pub fn pixels(&self) -> &WpSinglePixelBufferManagerV1 {
        self.data
            .pixels
            .as_ref()
            .expect("single-pixel buffers bound")
    }

    /// Sends `wl_display.sync`; the flag is set once Orbit answers.
    pub fn sync(&mut self) -> Arc<AtomicBool> {
        let done = Arc::new(AtomicBool::new(false));
        self.conn.display().sync(&self.qh(), done.clone());
        done
    }

    pub fn flush(&mut self) -> Result<(), WaylandError> {
        match self.conn.flush() {
            Err(WaylandError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            other => other,
        }
    }

    /// Reads whatever Orbit sent, without waiting, and handles it.
    pub fn read(&mut self) -> Result<(), WaylandError> {
        if let Some(guard) = self.conn.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(err),
            }
        }
        self.queue
            .dispatch_pending(&mut self.data)
            .map(|_| ())
            .map_err(|err| match err {
                wayland_client::DispatchError::Backend(err) => err,
                err => panic!("client dispatch failed: {err}"),
            })
    }
}

impl Dispatch<WlRegistry, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_registry::Event;
        match event {
            Event::Global {
                name,
                interface,
                version,
            } => {
                if !state.globals.iter().any(|&(n, _, _)| n == name) {
                    state.globals.push((name, interface, version));
                }
            }
            Event::GlobalRemove { name } => state.globals.retain(|&(n, _, _)| n != name),
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, Arc<AtomicBool>> for ClientData {
    fn event(
        _: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        done: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        done.store(true, Ordering::Relaxed);
    }
}

impl Dispatch<XdgWmBase, ()> for ClientData {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for ClientData {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            state
                .pending_serial
                .insert(xdg_surface.id().protocol_id(), serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for ClientData {
    fn event(
        state: &mut Self,
        toplevel: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = toplevel.id().protocol_id();
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state.configured_size.insert(id, (width, height));
            }
            xdg_toplevel::Event::Close => state.closed.push(id),
            _ => {}
        }
    }
}

impl Dispatch<XdgPopup, ()> for ClientData {
    fn event(
        state: &mut Self,
        popup: &XdgPopup,
        event: xdg_popup::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = event {
            state.popups_done.push(popup.id().protocol_id());
        }
    }
}

delegate_noop!(ClientData: ignore WlCompositor);
delegate_noop!(ClientData: ignore WlSurface);
delegate_noop!(ClientData: ignore WlBuffer);
delegate_noop!(ClientData: ignore WpViewporter);
delegate_noop!(ClientData: ignore WpViewport);
delegate_noop!(ClientData: ignore WpSinglePixelBufferManagerV1);
delegate_noop!(ClientData: ignore XdgPositioner);
