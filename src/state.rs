use std::{
    collections::HashMap,
    ffi::OsString,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use orbit_core::{
    ids::{IdAllocator, WindowId},
    workspaces::Workspaces,
};
use smithay::{
    desktop::{Space, Window},
    output::Output,
    reexports::{
        calloop::{EventLoop, LoopHandle, LoopSignal},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
        },
    },
    utils::{Clock, Monotonic},
    wayland::{
        compositor::CompositorClientState, dmabuf::DmabufState, security_context::SecurityContext,
        socket::ListeningSocketSource,
    },
};

use crate::{backend::Backend, outputs::OutputState, render::SpaceRefresh};

/// Data associated with a wayland client that connects to Orbit.
#[derive(Default)]
pub struct ClientState {
    /// The compositor state for this client.
    pub compositor_state: CompositorClientState,
    /// Optional sandbox identity metadata for this client.
    pub security_context: Option<SecurityContext>,
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        tracing::debug!(?client_id, "client connected");
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::debug!(?client_id, ?reason, "client disconnected");
    }
}

#[derive(Debug, Default)]
pub struct FrameSchedule {
    pub renders_scheduled: bool,
    pub cleanup_scheduled: bool,
    pub queued_outputs: Vec<Output>,
    pub frame_time: Duration,
}

pub struct Orbit {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Orbit>,
    pub loop_signal: LoopSignal,
    pub backend: Option<Backend>,
    pub outputs: HashMap<Output, OutputState>,
    pub frames: FrameSchedule,
    pub space: Space<Window>,
    pub space_refresh: SpaceRefresh,
    pub output_ids: IdAllocator,
    pub windows: HashMap<WindowId, Window>,
    pub workspaces: Workspaces<WindowId>,
    pub dmabuf_state: DmabufState,
    pub clock: Clock<Monotonic>,
}

impl Orbit {
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
    ) -> anyhow::Result<Self> {
        let start_time = Instant::now();
        let display_handle = display.handle();

        Ok(Self {
            start_time,
            socket_name: OsString::new(),
            display_handle,
            loop_handle: event_loop.handle(),
            loop_signal: event_loop.get_signal(),
            backend: None,
            outputs: HashMap::new(),
            frames: FrameSchedule::default(),
            space: Space::default(),
            space_refresh: Default::default(),
            output_ids: IdAllocator::default(),
            windows: HashMap::new(),
            workspaces: Workspaces::default(),
            dmabuf_state: DmabufState::new(),
            clock: Clock::new(),
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

    pub fn after_dispatch(&mut self) {}
}

// Routes every Wayland protocol Orbit implements to its handler trait.
smithay::delegate_dispatch2!(Orbit);
