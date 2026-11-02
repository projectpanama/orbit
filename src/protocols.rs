pub mod output_management;

use smithay::reexports::wayland_server::Client;

use crate::state::client::ClientState;

/// Returns whether `client` is trusted to access privileged, session-wide
/// globals that can expose or control other clients or the session.
///
/// Clients associated with a Wayland security context are treated as
/// sandboxed (e.g. Flatpak apps) and are not granted these globals.
/// Clients without Orbit's [`ClientState`] are treated as unprivileged.
pub fn privileged(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|data| data.security_context.is_none())
}
