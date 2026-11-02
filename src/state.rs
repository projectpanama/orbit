pub mod client;
pub mod frame;
pub mod input;

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    sync::Arc,
    time::Instant,
};

use anyhow::Context;
use orbit_config::Config;
use orbit_core::{
    ids::{IdAllocator, OutputId, WindowId},
    workspaces::Workspaces,
};
use smithay::{
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{
        Seat, SeatState,
        keyboard::KeyboardHandle,
        pointer::{CursorImageStatus, PointerHandle},
    },
    output::Output,
    reexports::{
        calloop::{EventLoop, LoopHandle, LoopSignal},
        wayland_server::{Display, DisplayHandle, protocol::wl_surface::WlSurface},
    },
    utils::{Clock, Logical, Monotonic, Point},
    wayland::{
        dmabuf::DmabufState, session_lock::SessionLockManagerState,
        shell::wlr_layer::WlrLayerShellState, socket::ListeningSocketSource,
    },
};

use crate::{
    backend::Backend,
    outputs::OutputState,
    protocols::output_management::OutputManagementState,
    render::SpaceRefresh,
    shell::lock::SessionLock,
    state::{client::ClientState, frame::FrameSchedule, input::InputState},
    window::{
        self,
        decorations::{self, DecorationHit, DecorationStyle, FontLoader},
        with_window_state,
    },
};

/// What the pointer is over.
pub enum Hit {
    /// A client surface and its location in global coordinates.
    Surface(WlSurface, Point<f64, Logical>),
    /// A window decoration and the hit location.
    Decoration(DecorationHit),
}

pub struct Orbit {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Orbit>,
    pub loop_signal: LoopSignal,
    pub backend: Option<Backend>,
    pub frames: FrameSchedule,
    pub space: Space<Window>,
    pub space_refresh: SpaceRefresh,
    pub layer_shell_state: WlrLayerShellState,
    pub output_ids: IdAllocator,
    pub outputs: HashMap<Output, OutputState>,
    pub focused_output: Option<OutputId>,
    pub output_management: OutputManagementState,
    pub window_ids: IdAllocator,
    pub windows: HashMap<WindowId, Window>,
    pub window_surfaces: HashMap<WlSurface, WindowId>,
    pub workspaces: Workspaces<WindowId>,
    pub hide_stamp: u64,
    pub popups: PopupManager,
    pub dmabuf_state: DmabufState,
    pub clock: Clock<Monotonic>,
    pub session_lock: SessionLock,
    pub session_lock_state: SessionLockManagerState,
    pub seat: Seat<Self>,
    pub seat_state: SeatState<Orbit>,
    pub input: InputState,
    pub pointer: PointerHandle<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub config: Config,
    pub decoration_style: Arc<DecorationStyle>,
    pub font_loader: FontLoader,
}

impl Orbit {
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
        config: Config,
    ) -> anyhow::Result<Self> {
        let start_time = Instant::now();
        let display_handle = display.handle();

        let layer_shell_state = WlrLayerShellState::new_with_filter::<Self, _>(
            &display_handle,
            crate::protocols::privileged,
        );

        let output_management =
            OutputManagementState::new(&display_handle, config.desktop_integration);

        let session_lock_state =
            SessionLockManagerState::new::<Self, _>(&display_handle, crate::protocols::privileged);

        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&display_handle, "seat0");

        let input = InputState {
            workspace_swipe: None,
            suppressed_keys: HashSet::new(),
            last_titlebar_click: None,
            decoration_hover: None,
            cursor_status: CursorImageStatus::default_named(),
            cursor_theme: None,
        };

        let pointer = seat.add_pointer();

        let keyboard_config = &config.input.keyboard;
        let keyboard = seat
            .add_keyboard(
                crate::input::config::xkb_config(keyboard_config),
                keyboard_config.repeat_delay,
                keyboard_config.repeat_rate,
            )
            .or_else(|err| {
                tracing::warn!(?err, "invalid keyboard layout, using the default");
                seat.add_keyboard(
                    Default::default(),
                    keyboard_config.repeat_delay,
                    keyboard_config.repeat_rate,
                )
            })
            .context("failed to initialize the keyboard")?;

        let decoration_style = Arc::new(DecorationStyle::new(config.decorations.clone(), None));

        let mut font_loader = FontLoader::new(&event_loop.handle())?;
        font_loader.request(&config.decorations);

        Ok(Self {
            start_time,
            socket_name: OsString::new(),
            display_handle,
            loop_handle: event_loop.handle(),
            loop_signal: event_loop.get_signal(),
            backend: None,
            frames: FrameSchedule::default(),
            space: Space::default(),
            space_refresh: Default::default(),
            layer_shell_state,
            output_ids: IdAllocator::default(),
            outputs: HashMap::new(),
            focused_output: None,
            output_management,
            window_ids: IdAllocator::default(),
            windows: HashMap::new(),
            window_surfaces: HashMap::new(),
            workspaces: Workspaces::default(),
            hide_stamp: 0,
            popups: PopupManager::default(),
            dmabuf_state: DmabufState::new(),
            clock: Clock::new(),
            session_lock: SessionLock::default(),
            session_lock_state,
            seat,
            seat_state,
            input,
            pointer,
            keyboard,
            config,
            decoration_style,
            font_loader,
        })
    }

    pub fn listen(&mut self) -> anyhow::Result<()> {
        let listening_socket =
            ListeningSocketSource::new_auto().context("failed to create the Wayland socket")?;
        self.socket_name = listening_socket.socket_name().to_os_string();
        self.loop_handle
            .insert_source(listening_socket, |client_stream, _, state| {
                if let Err(err) = state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!(?err, "failed to insert client");
                }
            })
            .context("failed to listen on the Wayland socket")?;
        Ok(())
    }

    pub fn after_dispatch(&mut self) {
        if let Err(err) = self.display_handle.flush_clients() {
            tracing::debug!(%err, "failed to flush clients");
        }
    }

    pub fn hit_test(&self, pos: Point<f64, Logical>) -> Option<(Window, Hit)> {
        for window in self.space.elements().rev() {
            let Some(location) = self.space.element_location(window) else {
                continue;
            };
            let render_loc = location - window.geometry().loc;
            if let Some((surface, surface_loc)) =
                window.surface_under(pos - render_loc.to_f64(), WindowSurfaceType::ALL)
            {
                let global = (surface_loc + render_loc).to_f64();
                return Some((window.clone(), Hit::Surface(surface, global)));
            }
            if window::uses_server_side_decorations(window)
                && let Some(frame) = self.frame_geometry(window)
                && let Some(hit) = with_window_state(window, |state| {
                    decorations::hit_test(&state.style.config, frame, pos)
                })
            {
                return Some((window.clone(), Hit::Decoration(hit)));
            }
        }
        None
    }
}

// Routes every Wayland protocol Orbit implements to its handler trait.
smithay::delegate_dispatch2!(Orbit);
