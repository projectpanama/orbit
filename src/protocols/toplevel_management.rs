use orbit_protocols::toplevel_management::server as generated;
use smithay::{
    desktop::Window,
    reexports::wayland_server::{Client, DataInit, DisplayHandle, New, Resource, Weak},
    wayland::{Dispatch2, GlobalDispatch2, foreign_toplevel_list::ForeignToplevelHandle},
};

use crate::{
    state::Orbit,
    window::{self, Layout},
};

use self::generated::{
    orbit_toplevel_manager_v1::{self, OrbitToplevelManagerV1},
    orbit_toplevel_state_v1::{self, OrbitToplevelStateV1},
};
use super::ext_workspace::WorkspaceData;

const VERSION: u32 = 1;

/// A state resource and the last state bitset sent to its client.
struct TrackedState {
    /// Weak reference to the client's state resource.
    resource: Weak<OrbitToplevelStateV1>,
    /// Foreign toplevel represented by the state resource.
    toplevel: ForeignToplevelHandle,
    /// State bits most recently sent to the client.
    sent: u32,
}

/// Owns the toplevel-manager global and tracked state resources.
pub struct ToplevelManagerState {
    /// State objects currently being followed.
    states: Vec<TrackedState>,
    /// Indicates that tracked windows may have changed state.
    dirty: bool,
}

impl ToplevelManagerState {
    /// Creates the manager global when enabled and initializes empty tracking.
    pub fn new(display_handle: &DisplayHandle, enabled: bool) -> Self {
        display_handle.create_global::<Orbit, OrbitToplevelManagerV1, _>(
            VERSION,
            ManagerGlobalData { enabled },
        );
        Self {
            states: Vec::new(),
            dirty: false,
        }
    }
}

impl Orbit {
    /// Marks tracked toplevel state as needing a refresh.
    pub fn toplevel_states_changed(&mut self) {
        self.desktop.toplevel_manager.dirty = true;
    }

    /// Sends changed state and removes destroyed or closed tracked objects.
    ///
    /// This is called after dispatch, but does work only after
    /// [`Orbit::toplevel_states_changed`] marks the collection dirty.
    pub fn refresh_toplevel_states(&mut self) {
        if !std::mem::take(&mut self.desktop.toplevel_manager.dirty)
            || self.desktop.toplevel_manager.states.is_empty()
        {
            return;
        }
        let focused = self.focused_window();
        let mut states = std::mem::take(&mut self.desktop.toplevel_manager.states);
        states.retain_mut(|tracked| {
            let Ok(resource) = tracked.resource.upgrade() else {
                return false;
            };
            let Some(window) = self.window_for_foreign_handle(&tracked.toplevel) else {
                return false;
            };
            let bits = state_bits(&window, focused.as_ref());
            if bits != tracked.sent {
                tracked.sent = bits;
                send_state(&resource, bits);
            }
            true
        });
        self.desktop.toplevel_manager.states = states;
    }
}

/// Data controlling visibility of the manager global.
pub struct ManagerGlobalData {
    /// Whether the global is enabled for privileged clients.
    enabled: bool,
}

impl GlobalDispatch2<OrbitToplevelManagerV1, Orbit> for ManagerGlobalData {
    fn bind(
        &self,
        _state: &mut Orbit,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<OrbitToplevelManagerV1>,
        data_init: &mut DataInit<'_, Orbit>,
    ) {
        data_init.init(resource, ManagerData);
    }

    fn can_view(&self, client: &Client) -> bool {
        self.enabled && super::privileged(client)
    }
}

/// Per-resource data for a bound manager object.
pub struct ManagerData;

impl Dispatch2<OrbitToplevelManagerV1, Orbit> for ManagerData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        _resource: &OrbitToplevelManagerV1,
        request: orbit_toplevel_manager_v1::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Orbit>,
    ) {
        use orbit_toplevel_manager_v1::Request;
        // Closed toplevels and removed workspaces are ignored: either can
        // go away while the request is on its way.
        match request {
            Request::Activate { toplevel, seat: _ } => {
                if let Some(window) = state.window_for_foreign_toplevel(&toplevel) {
                    state.activate_window(&window);
                }
            }
            Request::Close { toplevel } => {
                if let Some(window) = state.window_for_foreign_toplevel(&toplevel) {
                    Orbit::close(&window);
                }
            }
            Request::SetMinimized { toplevel } => {
                if let Some(window) = state.window_for_foreign_toplevel(&toplevel)
                    && !window::is_minimized(&window)
                {
                    state.minimize(&window);
                }
            }
            Request::UnsetMinimized { toplevel } => {
                if let Some(window) = state.window_for_foreign_toplevel(&toplevel) {
                    state.unminimize(&window);
                }
            }
            Request::SetMaximized { toplevel } => {
                if let Some(window) = state.window_for_foreign_toplevel(&toplevel) {
                    state.maximize(&window);
                }
            }
            Request::UnsetMaximized { toplevel } => {
                if let Some(window) = state.window_for_foreign_toplevel(&toplevel) {
                    state.unmaximize(&window);
                }
            }
            Request::GetState { id, toplevel } => {
                let resource = data_init.init(id, StateData);
                // A closed toplevel's state object stays silent.
                if let Some(handle) = ForeignToplevelHandle::from_resource(&toplevel)
                    && let Some(window) = state.window_for_foreign_handle(&handle)
                {
                    let focused = state.focused_window();
                    let bits = state_bits(&window, focused.as_ref());
                    send_state(&resource, bits);
                    state.desktop.toplevel_manager.states.push(TrackedState {
                        resource: resource.downgrade(),
                        toplevel: handle,
                        sent: bits,
                    });
                }
            }
            Request::MoveToWorkspace {
                toplevel,
                workspace,
            } => {
                let window = state.window_for_foreign_toplevel(&toplevel);
                let workspace = workspace.data::<WorkspaceData>().map(WorkspaceData::id);
                if let (Some(window), Some(workspace)) = (window, workspace) {
                    state.move_window_to_workspace_id(&window, workspace);
                }
            }
            Request::Destroy => {}
            _ => {}
        }
        state.queue_redraw_all();
    }
}

/// Per-resource data for a state object.
///
/// The associated toplevel and last-sent state are kept in
/// [`ToplevelManagerState`].
pub struct StateData;

impl Dispatch2<OrbitToplevelStateV1, Orbit> for StateData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        _resource: &OrbitToplevelStateV1,
        request: orbit_toplevel_state_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Orbit>,
    ) {
        // Dropped from the tracked list on the next refresh. (Those of a
        // client that disconnects go on a later one.)
        if let orbit_toplevel_state_v1::Request::Destroy = request {
            state.toplevel_states_changed();
        }
    }
}

#[cfg(test)]
impl super::DesktopProtocols {
    /// Reports whether no state objects are currently tracked.
    pub fn toplevel_manager_is_idle(&self) -> bool {
        self.toplevel_manager.states.is_empty()
    }
}

/// Encodes the states that currently apply to `window` as a bitset.
fn state_bits(window: &Window, focused: Option<&Window>) -> u32 {
    use orbit_toplevel_state_v1::State;
    let mut bits = 0;
    // Each protocol enum value occupies the corresponding bit position.
    let mut set = |state: State, on: bool| {
        if on {
            bits |= 1 << state as u32;
        }
    };
    set(State::Activated, focused == Some(window));
    set(State::Minimized, window::is_minimized(window));
    set(State::Maximized, Orbit::layout(window) == Layout::Maximized);
    set(State::Fullscreen, window::is_fullscreen(window));
    bits
}

/// Sends each set state bit to the protocol resource as native-endian bytes.
fn send_state(resource: &OrbitToplevelStateV1, bits: u32) {
    let states: Vec<u8> = (0..32)
        .filter(|n| bits & (1 << n) != 0)
        .flat_map(|n: u32| n.to_ne_bytes())
        .collect();
    resource.state(states);
}
