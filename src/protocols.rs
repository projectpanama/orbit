pub mod ext_workspace;
pub mod foreign_toplevel;
pub mod output_management;
pub mod overview;
pub mod security_context;
pub mod toplevel_management;

use std::collections::HashMap;

use orbit_core::ids::WindowId;
use smithay::{
    reexports::wayland_server::{Client, DisplayHandle},
    wayland::{
        foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListState},
        security_context::SecurityContextState,
    },
};

use self::{
    ext_workspace::ExtWorkspaceState, overview::OverviewProtocolState,
    toplevel_management::ToplevelManagerState,
};
use crate::state::{Orbit, client::ClientState};

pub struct DesktopProtocols {
    /// Whether the desktop-integration protocols are enabled at all. When
    /// disabled, Orbit doesn't create the globals and doesn't send any events.
    pub enabled: bool,
    /// The state of the foreign toplevel protocol.
    pub foreign_toplevels: ForeignToplevelListState,
    /// The handle for each window's toplevel object, if any.
    pub toplevel_handles: HashMap<WindowId, ForeignToplevelHandle>,
    /// Whether the toplevel list needs to be refreshed.
    pub toplevels_dirty: bool,
    /// The state of the ext-workspace protocol.
    pub workspaces: ExtWorkspaceState,
    /// The state of the toplevel-management protocol.
    pub toplevel_manager: ToplevelManagerState,
    /// The state of the overview protocol.
    pub overview: OverviewProtocolState,
    /// The state of the security context protocol.
    pub security_context: SecurityContextState,
}

impl DesktopProtocols {
    pub fn new(display_handle: &DisplayHandle, enabled: bool) -> Self {
        let visible = move |client: &Client| enabled && privileged(client);
        Self {
            enabled,
            foreign_toplevels: ForeignToplevelListState::new_with_filter::<Orbit>(
                display_handle,
                visible,
            ),
            toplevel_handles: HashMap::new(),
            toplevels_dirty: true,
            workspaces: ExtWorkspaceState::new(display_handle, enabled),
            toplevel_manager: ToplevelManagerState::new(display_handle, enabled),
            overview: OverviewProtocolState::new(display_handle, enabled),
            // A sandboxed client can't make itself another sandbox.
            security_context: SecurityContextState::new::<Orbit, _>(display_handle, privileged),
        }
    }
}

impl Orbit {
    /// Tells desktop-integration clients what changed since the last call.
    /// Runs after every dispatch; it sends nothing when nothing changed.
    pub fn refresh_desktop_protocols(&mut self) {
        if !self.desktop.enabled {
            return;
        }
        self.refresh_foreign_toplevels();
        self.refresh_toplevel_states();
        self.refresh_ext_workspaces();
    }
}

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
