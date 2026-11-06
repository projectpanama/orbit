use orbit_core::ids::WindowId;
use orbit_protocols::overview::server as generated;
use smithay::{
    output::{Output, WeakOutput},
    reexports::wayland_server::{Client, DataInit, DisplayHandle, New, Resource, Weak},
    utils::{Logical, Rectangle},
    wayland::{Dispatch2, GlobalDispatch2},
};

use self::generated::{
    orbit_overview_manager_v1::{self, OrbitOverviewManagerV1},
    orbit_overview_v1::{self, OrbitOverviewV1},
};
use crate::state::Orbit;

const VERSION: u32 = 1;

/// The layout of windows sent to clients is a vector of (window id, rectangle)s.
type Layout = Vec<(WindowId, Rectangle<i32, Logical>)>;

/// State tracked for one client overview resource.
struct Tracked {
    /// Weak reference to the client resource.
    resource: Weak<OrbitOverviewV1>,
    /// Output represented by the overview resource.
    output: WeakOutput,
    /// Most recently sent overview transition progress.
    progress: Option<f64>,
    /// Most recently sent window layout.
    layout: Option<Layout>,
}

/// Runtime state for the overview protocol.
pub struct OverviewProtocolState {
    /// Overview resources that still need updates.
    tracked: Vec<Tracked>,
}

impl OverviewProtocolState {
    /// Creates the overview protocol global and its runtime state.
    ///
    /// The global is advertised only when `enabled` is true and the client
    /// passes the privilege check during binding.
    pub fn new(dh: &DisplayHandle, enabled: bool) -> Self {
        dh.create_global::<Orbit, OrbitOverviewManagerV1, _>(
            VERSION,
            ManagerGlobalData { enabled },
        );
        Self {
            tracked: Vec::new(),
        }
    }
}

/// Global data used while handling overview manager bindings.
pub struct ManagerGlobalData {
    /// Whether the overview protocol is enabled.
    enabled: bool,
}

/// Per-resource data for an overview manager object.
pub struct ManagerData;

/// Per-resource data for an overview object.
pub struct OverviewData {
    /// Output represented by this overview object.
    output: WeakOutput,
}

impl GlobalDispatch2<OrbitOverviewManagerV1, Orbit> for ManagerGlobalData {
    fn bind(
        &self,
        _state: &mut Orbit,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<OrbitOverviewManagerV1>,
        data_init: &mut DataInit<'_, Orbit>,
    ) {
        data_init.init(resource, ManagerData);
    }

    fn can_view(&self, client: &Client) -> bool {
        self.enabled && super::privileged(client)
    }
}

impl Dispatch2<OrbitOverviewManagerV1, Orbit> for ManagerData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        _resource: &OrbitOverviewManagerV1,
        request: orbit_overview_manager_v1::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Orbit>,
    ) {
        match request {
            orbit_overview_manager_v1::Request::GetOverview { id, output } => {
                let output = Output::from_resource(&output)
                    .map_or_else(WeakOutput::default, |o| o.downgrade());
                let resource = data_init.init(
                    id,
                    OverviewData {
                        output: output.clone(),
                    },
                );
                state.desktop.overview.tracked.push(Tracked {
                    resource: resource.downgrade(),
                    output: output.clone(),
                    progress: None,
                    layout: None,
                });
                // Send the current state immediately when the output exists.
                if let Some(output) = output.upgrade() {
                    state.refresh_overview_protocol(&output);
                } else {
                    resource.progress(0.0);
                }
            }
            orbit_overview_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch2<OrbitOverviewV1, Orbit> for OverviewData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        resource: &OrbitOverviewV1,
        request: orbit_overview_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Orbit>,
    ) {
        let Some(output) = self
            .output
            .upgrade()
            .filter(|o| state.outputs.contains_key(o))
        else {
            return;
        };
        match request {
            orbit_overview_v1::Request::Open => state.open_overview(&output),
            orbit_overview_v1::Request::Close => state.close_overview(&output),
            orbit_overview_v1::Request::Destroy => {
                let id = resource.id();
                state
                    .desktop
                    .overview
                    .tracked
                    .retain(|t| t.resource.id() != id);
            }
            _ => {}
        }
    }
}

impl Orbit {
    /// Sends the current overview state to clients tracking `output`.
    ///
    /// Progress is sent when it changes. The window layout is sent only when
    /// it changes and is followed by the protocol done event. Resources whose
    /// clients or outputs no longer exist are removed from tracking.
    pub fn refresh_overview_protocol(&mut self, output: &Output) {
        if self.desktop.overview.tracked.is_empty() {
            return;
        }
        let progress = self.overview_progress(output);
        let mut tracked = std::mem::take(&mut self.desktop.overview.tracked);
        // Build the layout once and copy it only for clients that need it.
        let layout = self.overview(output).map(|o| o.layout());
        tracked.retain_mut(|t| {
            let Ok(resource) = t.resource.upgrade() else {
                return false;
            };
            if t.output.upgrade().as_ref() != Some(output) {
                return true;
            }
            if t.progress != Some(progress) {
                t.progress = Some(progress);
                resource.progress(progress);
            }
            if let Some(layout) = layout
                && t.layout.as_deref() != Some(layout)
            {
                t.layout = Some(layout.to_vec());
                let client = resource.client();
                for (id, rect) in layout {
                    let handle = self
                        .desktop
                        .toplevel_handles
                        .get(id)
                        .zip(client.as_ref())
                        .and_then(|(h, c)| h.resources_for_client(c).into_iter().next());
                    if let Some(handle) = handle {
                        resource.window(&handle, rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
                    }
                }
                resource.done();
            } else if layout.is_none() {
                // Clear the cached layout so the next opening sends it again.
                t.layout = None;
            }
            true
        });
        self.desktop.overview.tracked = tracked;
    }
}
