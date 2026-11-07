mod frame;
mod globals;
mod input;

pub mod client;

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    process::Command,
    sync::Arc,
    time::Instant,
};

use anyhow::Context;
use orbit_config::{Config, ConfigLoader, ModKey};
use orbit_core::{
    ids::{IdAllocator, OutputId, WindowId},
    workspaces::Workspaces,
};
use smithay::{
    backend::session::libseat::LibSeatSession,
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{
        Seat, SeatState,
        keyboard::KeyboardHandle,
        pointer::{CursorImageStatus, PointerHandle},
    },
    output::Output,
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken,
            generic::Generic,
        },
        wayland_server::{Display, DisplayHandle, protocol::wl_surface::WlSurface},
    },
    utils::{Clock, Logical, Monotonic, Point},
    wayland::{
        compositor::CompositorState,
        dmabuf::DmabufState,
        idle_notify::IdleNotifierState,
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        session_lock::SessionLockManagerState,
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState},
        shm::ShmState,
        socket::ListeningSocketSource,
        xdg_activation::XdgActivationState,
    },
};

use crate::{
    backend::Backend,
    outputs::OutputState,
    protocols::{DesktopProtocols, output_management::OutputManagementState},
    render::{Overlays, SpaceRefresh},
    shell::{capture::CaptureState, lock::SessionLock},
    state::{client::ClientState, frame::FrameSchedule, globals::Globals, input::InputState},
    window::{
        self,
        decorations::{self, DecorationHit, DecorationStyle, FontLoader},
        with_window_state,
    },
};

/// The stack size for threads that wait for spawned clients to exit.
const REAPER_STACK_SIZE: usize = 64 * 1024;

/// What the pointer is over.
pub enum Hit {
    /// A client surface and its location in global coordinates.
    Surface(WlSurface, Point<f64, Logical>),
    /// A window decoration and the hit location.
    Decoration(DecorationHit),
}

/// Reloading the config when its files change.
#[derive(Default)]
pub struct ConfigReload {
    /// Loads the config again from the files it came from.
    pub loader: Option<ConfigLoader>,
    /// A reload waiting for the files to settle.
    pub timer: Option<RegistrationToken>,
}

pub struct Orbit {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Orbit>,
    pub loop_signal: LoopSignal,
    pub backend: Option<Backend>,
    pub frames: FrameSchedule,
    pub session: Option<LibSeatSession>,
    pub space: Space<Window>,
    pub space_refresh: SpaceRefresh,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_activation_state: XdgActivationState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub layer_shell_state: WlrLayerShellState,
    pub desktop: DesktopProtocols,
    pub output_ids: IdAllocator,
    pub outputs: HashMap<Output, OutputState>,
    pub focused_output: Option<OutputId>,
    pub output_management: OutputManagementState,
    pub window_ids: IdAllocator,
    pub windows: HashMap<WindowId, Window>,
    pub window_surfaces: HashMap<WlSurface, WindowId>,
    pub workspaces: Workspaces<WindowId>,
    pub globals: Globals,
    pub hide_stamp: u64,
    pub popups: PopupManager,
    pub overlays: Overlays,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub capture: CaptureState,
    pub clock: Clock<Monotonic>,
    pub session_lock: SessionLock,
    pub session_lock_state: SessionLockManagerState,
    pub idle_notifier: IdleNotifierState<Self>,
    pub idle_inhibitors: HashSet<WlSurface>,
    pub seat: Seat<Self>,
    pub seat_state: SeatState<Orbit>,
    pub input: InputState,
    pub pointer: PointerHandle<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub config: Config,
    pub decoration_style: Arc<DecorationStyle>,
    pub font_loader: FontLoader,
    pub config_reload: ConfigReload,
}

impl Orbit {
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
        config: Config,
    ) -> anyhow::Result<Self> {
        let start_time = Instant::now();
        let display_handle = display.handle();

        let compositor_state = CompositorState::new::<Self>(&display_handle);

        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);

        let xdg_activation_state = XdgActivationState::new::<Self>(&display_handle);

        let data_device_state = DataDeviceState::new::<Self>(&display_handle);

        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);

        let layer_shell_state = WlrLayerShellState::new_with_filter::<Self, _>(
            &display_handle,
            crate::protocols::privileged,
        );

        let desktop = DesktopProtocols::new(&display_handle, config.desktop_integration);

        let output_management =
            OutputManagementState::new(&display_handle, config.desktop_integration);

        let globals = Globals::new(&display_handle);

        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);

        let capture = CaptureState::new(&display_handle, config.desktop_integration);

        let session_lock_state =
            SessionLockManagerState::new::<Self, _>(&display_handle, crate::protocols::privileged);

        let idle_notifier = IdleNotifierState::new(&display_handle, event_loop.handle());

        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&display_handle, "seat0");

        let input = InputState {
            mod_key: config.mod_key.unwrap_or(ModKey::Super),
            workspace_swipe: None,
            overview_swipe: None,
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

        Self::insert_display_source(display, event_loop)?;

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
            session: None,
            space: Space::default(),
            space_refresh: Default::default(),
            compositor_state,
            xdg_shell_state,
            xdg_activation_state,
            data_device_state,
            primary_selection_state,
            layer_shell_state,
            desktop,
            output_ids: IdAllocator::default(),
            outputs: HashMap::new(),
            focused_output: None,
            output_management,
            window_ids: IdAllocator::default(),
            windows: HashMap::new(),
            window_surfaces: HashMap::new(),
            workspaces: Workspaces::default(),
            globals,
            hide_stamp: 0,
            popups: PopupManager::default(),
            overlays: Overlays::default(),
            shm_state,
            dmabuf_state: DmabufState::new(),
            capture,
            clock: Clock::new(),
            session_lock: SessionLock::default(),
            session_lock_state,
            idle_notifier,
            idle_inhibitors: HashSet::new(),
            seat,
            seat_state,
            input,
            pointer,
            keyboard,
            config,
            decoration_style,
            font_loader,
            config_reload: ConfigReload::default(),
        })
    }

    /// Opens an auto-named Wayland socket and accepts clients on it.
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

    /// Runs once per event loop iteration, after every event source and
    /// idle callback (including renders). Sends clients everything queued
    /// for them meanwhile.
    pub fn after_dispatch(&mut self) {
        self.refresh_desktop_protocols();
        self.refresh_output_management();
        self.refresh_idle_inhibit();
        if let Err(err) = self.display_handle.flush_clients() {
            tracing::debug!(%err, "failed to flush clients");
        }
    }

    /// Finds what is under `pos`, walking windows from the top down so a
    /// higher window's surface wins over a lower window's title bar.
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

    /// Spawns the `[startup]` commands from the config.
    pub fn spawn_startup(&self) {
        for command in &self.config.startup {
            self.spawn(command);
        }
    }

    /// Spawns a client connected to Orbit rather than to the host.
    pub fn spawn(&self, command: &[String]) {
        let Some((program, args)) = command.split_first() else {
            return;
        };
        let mut command = Command::new(program);
        command.args(args).env("WAYLAND_DISPLAY", &self.socket_name);
        // Orbit blocks SIGHUP for calloop, reset the inherited mask so clients
        // start with no signals blocked.
        // SAFETY: only async-signal-safe libc calls between fork and exec.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut command, || {
                let mut set: libc::sigset_t = std::mem::zeroed();
                if libc::sigemptyset(&mut set) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        if self.session.is_some() {
            // Orbit is the session, not a window inside someone else's. A
            // desktop started with Orbit (its session script) names itself.
            command.env("XDG_SESSION_TYPE", "wayland");
            if std::env::var_os("XDG_CURRENT_DESKTOP").is_none_or(|v| v.is_empty()) {
                command.env("XDG_CURRENT_DESKTOP", "Orbit");
            }
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!(%program, %err, "failed to spawn client");
                return;
            }
        };
        // Reap the child when it exits so it doesn't linger as a zombie.
        // The thread only waits, so a small stack does, a long session
        // starts many of these.
        let reaper = std::thread::Builder::new()
            .name("orbit-reaper".into())
            .stack_size(REAPER_STACK_SIZE)
            .spawn(move || child.wait());
        if let Err(err) = reaper {
            tracing::warn!(%program, %err, "failed to start a thread to reap the client");
        }
    }

    /// Dispatches client requests whenever the display's fd is readable.
    fn insert_display_source(
        display: Display<Self>,
        event_loop: &mut EventLoop<'static, Self>,
    ) -> anyhow::Result<()> {
        event_loop
            .handle()
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    let _span = tracy_client::span!("dispatch_clients");
                    // SAFETY: the display is never dropped while the source is registered.
                    let result = unsafe { display.get_mut().dispatch_clients(state) };
                    // A misbehaving client is disconnected by wayland-server
                    // without an error here. What's left is Orbit's own
                    // epoll failing, after which no client could be served.
                    if let Err(err) = result
                        && !matches!(
                            err.kind(),
                            std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                        )
                    {
                        tracing::error!(%err, "failed to dispatch clients; quitting");
                        state.loop_signal.stop();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .context("failed to watch the Wayland display")?;
        Ok(())
    }
}

// Routes every Wayland protocol Orbit implements to its handler trait.
smithay::delegate_dispatch2!(Orbit);
