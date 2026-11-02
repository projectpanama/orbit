use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use orbit_config::OutputConfig;
use smithay::{
    reexports::{
        wayland_protocols_wlr::output_management::v1::server::{
            zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
            zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
            zwlr_output_head_v1::{self, AdaptiveSyncState, ZwlrOutputHeadV1},
            zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
            zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
        },
        wayland_server::{Client, DataInit, DisplayHandle, New, Resource, WEnum, Weak},
    },
    utils::{Point, Transform},
    wayland::{Dispatch2, GlobalDispatch2},
};

use crate::{
    outputs::config::{HeadChange, HeadInfo, HeadMode},
    state::Orbit,
};

const VERSION: u32 = 4;

/// Protocol resources and the last-announced state for one client-visible head.
struct SentHead {
    /// The protocol head resource.
    head: ZwlrOutputHeadV1,
    /// Protocol mode resources paired with Orbit's mode descriptions.
    modes: Vec<(HeadMode, ZwlrOutputModeV1)>,
    /// State most recently sent to the client.
    info: HeadInfo,
}

/// Output-management resources associated with one client manager instance.
struct Manager {
    /// Weak reference to the manager resource.
    resource: Weak<ZwlrOutputManagerV1>,
    /// Heads announced through this manager.
    heads: Vec<SentHead>,
}

/// Runtime state for the output-management protocol.
pub struct OutputManagementState {
    /// Manager resources currently bound by privileged clients.
    managers: Vec<Manager>,
    /// Serial incremented whenever the set of output state changes.
    ///
    /// A configuration created with an older serial is stale and is cancelled.
    serial: u32,
    /// Output state last announced to clients.
    last: Vec<HeadInfo>,
    /// Whether output state must be synchronized with clients.
    pub dirty: bool,
    /// Runtime settings changed through output management.
    ///
    /// These settings take precedence over static `[outputs]` entries when
    /// [`Orbit::output_settings`] resolves an output's configuration.
    pub overrides: HashMap<String, OutputConfig>,
}

impl OutputManagementState {
    /// Creates the protocol state and optionally advertises its global.
    ///
    /// When `enabled` is false, the state is still initialized but clients
    /// cannot bind the output-management global.
    pub fn new(display_handle: &DisplayHandle, enabled: bool) -> Self {
        display_handle
            .create_global::<Orbit, ZwlrOutputManagerV1, _>(VERSION, ManagerGlobalData { enabled });

        Self {
            managers: Vec::new(),
            serial: 0,
            last: Vec::new(),
            dirty: true,
            overrides: HashMap::new(),
        }
    }
}

/// Data associated with the output-management global.
pub struct ManagerGlobalData {
    /// Whether clients may bind the global.
    enabled: bool,
}

impl GlobalDispatch2<ZwlrOutputManagerV1, Orbit> for ManagerGlobalData {
    fn bind(
        &self,
        state: &mut Orbit,
        handle: &DisplayHandle,
        client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        data_init: &mut DataInit<'_, Orbit>,
    ) {
        let manager = data_init.init(resource, ManagerData);
        let heads = state.heads();
        let mut entry = Manager {
            resource: manager.downgrade(),
            heads: Vec::new(),
        };
        for info in &heads {
            if let Some(sent) = announce(handle, client, &manager, info) {
                entry.heads.push(sent);
            }
        }
        manager.done(state.output_management.serial);
        state.output_management.managers.push(entry);
    }

    fn can_view(&self, client: &Client) -> bool {
        self.enabled && super::privileged(client)
    }
}

/// Data associated with a bound output-management manager.
pub struct ManagerData;

impl Dispatch2<ZwlrOutputManagerV1, Orbit> for ManagerData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        resource: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Orbit>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                data_init.init(
                    id,
                    ConfigData {
                        serial,
                        heads: Arc::default(),
                        used: Mutex::new(false),
                    },
                );
            }
            zwlr_output_manager_v1::Request::Stop => {
                let id = resource.id();
                state
                    .output_management
                    .managers
                    .retain(|m| m.resource.id() != id);
                resource.finished();
            }
            _ => {}
        }
    }
}

/// Data attached to a protocol head resource.
pub struct HeadData {
    /// Connector name used to identify the output.
    name: String,
}

impl Dispatch2<ZwlrOutputHeadV1, Orbit> for HeadData {
    fn request(
        &self,
        _state: &mut Orbit,
        _client: &Client,
        _resource: &ZwlrOutputHeadV1,
        _request: zwlr_output_head_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Orbit>,
    ) {
    }
}

/// Data associated with a protocol mode resource.
pub struct ModeData {
    /// Output mode represented by the resource.
    mode: HeadMode,
}

impl Dispatch2<ZwlrOutputModeV1, Orbit> for ModeData {
    fn request(
        &self,
        _state: &mut Orbit,
        _client: &Client,
        _resource: &ZwlrOutputModeV1,
        _request: zwlr_output_mode_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Orbit>,
    ) {
    }
}

/// Shared state for one output configuration transaction.
pub struct ConfigData {
    /// Serial against which the transaction was created.
    serial: u32,
    /// Changes requested for individual heads in this transaction.
    heads: Arc<Mutex<Vec<Arc<Mutex<HeadChange>>>>>,
    /// Whether the transaction has already been tested, applied, or destroyed.
    used: Mutex<bool>,
}

impl ConfigData {
    /// Returns whether this transaction already contains a change for `name`.
    fn configured(&self, name: &str) -> bool {
        self.heads
            .lock()
            .map(|h| h.iter().any(|c| c.lock().is_ok_and(|c| c.name == name)))
            .unwrap_or(false)
    }

    /// Adds a requested head change to this transaction.
    fn add(&self, change: Arc<Mutex<HeadChange>>) {
        if let Ok(mut h) = self.heads.lock() {
            h.push(change);
        }
    }
}

impl Dispatch2<ZwlrOutputConfigurationV1, Orbit> for ConfigData {
    fn request(
        &self,
        state: &mut Orbit,
        _client: &Client,
        resource: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Orbit>,
    ) {
        use zwlr_output_configuration_v1::{Error, Request};
        let used = self.used.lock().is_ok_and(|u| *u);
        match request {
            Request::EnableHead { id, head } => {
                let name = head
                    .data::<HeadData>()
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                let change = Arc::new(Mutex::new(HeadChange::new(&name, true)));
                data_init.init(
                    id,
                    ConfigHeadData {
                        change: change.clone(),
                    },
                );
                if used {
                    resource.post_error(Error::AlreadyUsed, "configuration already applied");
                } else if self.configured(&name) {
                    resource.post_error(Error::AlreadyConfiguredHead, "head configured twice");
                } else {
                    self.add(change);
                }
            }
            Request::DisableHead { head } => {
                let name = head
                    .data::<HeadData>()
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                if used {
                    resource.post_error(Error::AlreadyUsed, "configuration already applied");
                } else if self.configured(&name) {
                    resource.post_error(Error::AlreadyConfiguredHead, "head configured twice");
                } else {
                    self.add(Arc::new(Mutex::new(HeadChange::new(&name, false))));
                }
            }
            Request::Apply | Request::Test => {
                if used {
                    resource.post_error(Error::AlreadyUsed, "configuration already applied");
                    return;
                }
                if let Ok(mut u) = self.used.lock() {
                    *u = true;
                }
                state.refresh_output_management();
                if self.serial != state.output_management.serial {
                    resource.cancelled();
                    return;
                }
                let changes: Vec<HeadChange> = self
                    .heads
                    .lock()
                    .map(|h| {
                        h.iter()
                            .filter_map(|c| c.lock().ok().map(|c| c.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let test = matches!(request, Request::Test);
                match state.apply_heads(&changes, test) {
                    Ok(()) => resource.succeeded(),
                    Err(err) => {
                        tracing::info!("output configuration refused: {err}");
                        resource.failed();
                    }
                }
                if !test {
                    state.output_management.dirty = true;
                    state.refresh_output_management();
                }
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

/// Shared state for one head within an output configuration transaction.
pub struct ConfigHeadData {
    /// Mutable output change represented by this protocol resource.
    change: Arc<Mutex<HeadChange>>,
}

impl Dispatch2<ZwlrOutputConfigurationHeadV1, Orbit> for ConfigHeadData {
    fn request(
        &self,
        _state: &mut Orbit,
        _client: &Client,
        _resource: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Orbit>,
    ) {
        use zwlr_output_configuration_head_v1::Request;
        let Ok(mut c) = self.change.lock() else {
            return;
        };
        match request {
            Request::SetMode { mode } => {
                if let Some(m) = mode.data::<ModeData>() {
                    c.mode = Some((m.mode.size, m.mode.refresh));
                }
            }
            Request::SetCustomMode {
                width,
                height,
                refresh,
            } => c.mode = Some(((width, height), refresh)),
            Request::SetPosition { x, y } => c.position = Some(Point::from((x, y))),
            Request::SetTransform {
                transform: WEnum::Value(t),
            } => c.transform = Some(t.into()),
            Request::SetScale { scale } => c.scale = Some(scale),
            Request::SetAdaptiveSync { .. } => {}
            _ => {}
        }
    }
}

impl Orbit {
    /// Synchronizes output changes with output-management clients.
    ///
    /// This is normally called after dispatching requests. It sends nothing
    /// when the state is not dirty or when the output list is unchanged.
    pub fn refresh_output_management(&mut self) {
        if !std::mem::take(&mut self.output_management.dirty) {
            return;
        }

        let heads = self.heads();
        if heads == self.output_management.last {
            return;
        }

        self.output_management.last = heads.clone();
        self.output_management.serial = self.output_management.serial.wrapping_add(1);

        let serial = self.output_management.serial;
        let display_handle = self.display_handle.clone();

        let mut managers = std::mem::take(&mut self.output_management.managers);
        managers.retain_mut(|m| {
            let Ok(manager) = m.resource.upgrade() else {
                return false;
            };
            let Some(client) = manager.client() else {
                return false;
            };
            m.heads.retain(|s| {
                let keep = heads.iter().any(|h| h.name == s.info.name);
                if !keep {
                    for (_, mode) in &s.modes {
                        mode.finished();
                    }
                    s.head.finished();
                }
                keep
            });
            for info in &heads {
                match m.heads.iter_mut().find(|s| s.info.name == info.name) {
                    Some(sent) => {
                        if sent.info.modes != info.modes {
                            for (_, mode) in sent.modes.drain(..) {
                                mode.finished();
                            }
                            sent.modes =
                                send_modes(&display_handle, &client, &sent.head, &info.modes);
                            // The current mode must be named again.
                            sent.info.current = None;
                        }
                        send_state(sent, info, false);
                    }
                    None => {
                        if let Some(sent) = announce(&display_handle, &client, &manager, info) {
                            m.heads.push(sent);
                        }
                    }
                }
            }
            manager.done(serial);
            true
        });

        self.output_management.managers = managers;
    }
}

/// Converts Smithay's transform into the `wl_output` transform enum.
fn wl_transform(
    t: Transform,
) -> smithay::reexports::wayland_server::protocol::wl_output::Transform {
    use smithay::reexports::wayland_server::protocol::wl_output::Transform as W;
    match t {
        Transform::Normal => W::Normal,
        Transform::_90 => W::_90,
        Transform::_180 => W::_180,
        Transform::_270 => W::_270,
        Transform::Flipped => W::Flipped,
        Transform::Flipped90 => W::Flipped90,
        Transform::Flipped180 => W::Flipped180,
        Transform::Flipped270 => W::Flipped270,
    }
}

/// Creates a protocol head for `info` on `manager` and sends its complete state.
fn announce(
    display_handle: &DisplayHandle,
    client: &Client,
    manager: &ZwlrOutputManagerV1,
    info: &HeadInfo,
) -> Option<SentHead> {
    let head = client
        .create_resource::<ZwlrOutputHeadV1, _, Orbit>(
            display_handle,
            manager.version(),
            HeadData {
                name: info.name.clone(),
            },
        )
        .ok()?;

    manager.head(&head);
    head.name(info.name.clone());
    head.description(info.description.clone());

    if info.size_mm.0 > 0 && info.size_mm.1 > 0 {
        head.physical_size(info.size_mm.0, info.size_mm.1);
    }

    let modes = send_modes(display_handle, client, &head, &info.modes);
    if head.version() >= 2 {
        head.make(info.make.clone());
        head.model(info.model.clone());
        head.serial_number(info.serial.clone());
    }

    let mut sent = SentHead {
        head,
        modes,
        info: info.clone(),
    };

    send_state(&mut sent, info, true);
    Some(sent)
}

/// Creates and announces the mode resources belonging to a protocol head.
fn send_modes(
    display_handle: &DisplayHandle,
    client: &Client,
    head: &ZwlrOutputHeadV1,
    modes: &[HeadMode],
) -> Vec<(HeadMode, ZwlrOutputModeV1)> {
    modes
        .iter()
        .filter_map(|m| {
            let mode = client
                .create_resource::<ZwlrOutputModeV1, _, Orbit>(
                    display_handle,
                    head.version(),
                    ModeData { mode: *m },
                )
                .ok()?;
            head.mode(&mode);
            mode.size(m.size.0, m.size.1);
            mode.refresh(m.refresh);
            if m.preferred {
                mode.preferred();
            }
            Some((*m, mode))
        })
        .collect()
}

/// Sends changes in a head's state, or its complete state when `all` is true.
fn send_state(sent: &mut SentHead, info: &HeadInfo, all: bool) {
    let head = &sent.head;
    let old = &sent.info;

    if all || old.enabled != info.enabled {
        head.enabled(i32::from(info.enabled));
    }

    if info.enabled {
        let current = info.current.and_then(|i| info.modes.get(i));
        let was = old.current.and_then(|i| old.modes.get(i));
        if (all || current != was || !old.enabled)
            && let Some(c) = current
            && let Some((_, mode)) = sent.modes.iter().find(|(m, _)| m == c)
        {
            head.current_mode(mode);
        }
        if all || !old.enabled || old.position != info.position {
            head.position(info.position.x, info.position.y);
        }
        if all || !old.enabled || old.transform != info.transform {
            head.transform(wl_transform(info.transform));
        }
        if all || !old.enabled || old.scale != info.scale {
            head.scale(info.scale);
        }
    }

    if head.version() >= 4 && all {
        head.adaptive_sync(AdaptiveSyncState::Disabled);
    }

    sent.info = info.clone();
}
