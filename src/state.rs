use smithay::{
    reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason},
    wayland::{compositor::CompositorClientState, security_context::SecurityContext},
};

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
