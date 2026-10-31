mod globals;

use std::{ffi::OsString, sync::Arc};

use anyhow::Context;
use smithay::{
    delegate_dispatch2,
    reexports::{
        calloop::{EventLoop, LoopHandle},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
        },
    },
    wayland::{
        compositor::CompositorClientState, dmabuf::DmabufState, security_context::SecurityContext,
        socket::ListeningSocketSource,
    },
};

use crate::backend::Backend;

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

pub struct Orbit {
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Orbit>,
    pub backend: Option<Backend>,
    pub dmabuf_state: DmabufState,
}

impl Orbit {
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
    ) -> anyhow::Result<Self> {
        let display_handle = display.handle();

        Ok(Self {
            socket_name: OsString::new(),
            display_handle,
            loop_handle: event_loop.handle(),
            backend: None,
            dmabuf_state: DmabufState::new(),
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
delegate_dispatch2!(Orbit);
