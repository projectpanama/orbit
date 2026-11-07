//! `zwlr_output_manager_v1`: listing monitors with their modes and changing
//! them, as a display settings app would.

use std::collections::HashMap;

use smithay::utils::Transform;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, backend::ObjectId, event_created_child,
};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

use super::{Fixture, TestClient, client::ClientData};

#[derive(Default)]
pub struct Head {
    pub proxy: Option<ZwlrOutputHeadV1>,
    pub name: String,
    pub enabled: bool,
    pub current_mode: Option<ObjectId>,
    pub position: (i32, i32),
    pub scale: f64,
    pub modes: Vec<ZwlrOutputModeV1>,
    pub finished: bool,
}

#[derive(Default)]
pub struct Heads {
    pub heads: HashMap<ObjectId, Head>,
    /// Mode id → (size, refresh, preferred).
    pub modes: HashMap<ObjectId, ((i32, i32), i32, bool)>,
    pub serial: Option<u32>,
    pub dones: u32,
    /// Configuration id → outcome.
    pub results: HashMap<ObjectId, &'static str>,
}

impl Heads {
    fn by_name(&self, name: &str) -> Option<&Head> {
        self.heads.values().find(|h| h.name == name && !h.finished)
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                state.outputs.heads.insert(
                    head.id(),
                    Head {
                        proxy: Some(head),
                        ..Head::default()
                    },
                );
            }
            zwlr_output_manager_v1::Event::Done { serial } => {
                state.outputs.serial = Some(serial);
                state.outputs.dones += 1;
            }
            _ => {}
        }
    }

    event_created_child!(ClientData, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        head: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_output_head_v1::Event as E;
        let Some(h) = state.outputs.heads.get_mut(&head.id()) else {
            return;
        };
        match event {
            E::Name { name } => h.name = name,
            E::Enabled { enabled } => h.enabled = enabled != 0,
            E::Mode { mode } => {
                state.outputs.modes.insert(mode.id(), ((0, 0), 0, false));
                h.modes.push(mode);
            }
            E::CurrentMode { mode } => h.current_mode = Some(mode.id()),
            E::Position { x, y } => h.position = (x, y),
            E::Scale { scale } => h.scale = scale,
            E::Finished => h.finished = true,
            _ => {}
        }
    }

    event_created_child!(ClientData, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        mode: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let entry = state.outputs.modes.entry(mode.id()).or_default();
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => entry.0 = (width, height),
            zwlr_output_mode_v1::Event::Refresh { refresh } => entry.1 = refresh,
            zwlr_output_mode_v1::Event::Preferred => entry.2 = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        config: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let outcome = match event {
            zwlr_output_configuration_v1::Event::Succeeded => "succeeded",
            zwlr_output_configuration_v1::Event::Failed => "failed",
            zwlr_output_configuration_v1::Event::Cancelled => "cancelled",
            _ => return,
        };
        state.outputs.results.insert(config.id(), outcome);
    }
}

wayland_client::delegate_noop!(ClientData: ignore ZwlrOutputConfigurationHeadV1);

fn bind(fixture: &mut Fixture, client: &mut TestClient) -> ZwlrOutputManagerV1 {
    let qh = client.qh();
    let registry = client.registry();
    let manager = client.bind(&registry, "zwlr_output_manager_v1", 4, &qh);
    fixture.roundtrip(client);
    fixture.roundtrip(client);
    manager
}

#[test]
fn lists_heads_with_modes() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let mut client = fixture.client();
    let _manager = bind(&mut fixture, &mut client);
    let heads = &client.data.outputs;
    assert!(heads.serial.is_some());
    let a = heads.by_name("A").expect("head A");
    assert!(a.enabled);
    assert!(!a.modes.is_empty());
    let current = a.current_mode.clone().expect("current mode");
    let (size, refresh, preferred) = heads.modes[&current];
    assert!(size.0 > 0 && refresh > 0 && preferred);
    assert!(heads.by_name("B").is_some());
}

#[test]
fn apply_changes_scale_and_position_and_resends() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let mut client = fixture.client();
    let manager = bind(&mut fixture, &mut client);
    let qh = client.qh();
    let serial = client.data.outputs.serial.expect("serial");
    let dones = client.data.outputs.dones;
    let head = client
        .data
        .outputs
        .by_name("B")
        .and_then(|h| h.proxy.clone())
        .expect("B");
    let config = manager.create_configuration(serial, &qh, ());
    let ch: ZwlrOutputConfigurationHeadV1 = config.enable_head(&head, &qh, ());
    ch.set_scale(2.0);
    ch.set_position(4000, 0);
    config.apply();
    fixture.roundtrip(&mut client);
    fixture.roundtrip(&mut client);
    assert_eq!(
        client.data.outputs.results.get(&config.id()),
        Some(&"succeeded")
    );
    let output = fixture.output("B");
    assert_eq!(output.current_scale().fractional_scale(), 2.0);
    assert_eq!(output.current_location().x, 4000);
    // The client heard about it, with a new serial.
    assert!(client.data.outputs.dones > dones);
    let b = client.data.outputs.by_name("B").expect("B");
    assert_eq!(b.scale, 2.0);
    assert_eq!(b.position, (4000, 0));
}

#[test]
fn stale_serial_is_cancelled_and_test_changes_nothing() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut client = fixture.client();
    let manager = bind(&mut fixture, &mut client);
    let qh = client.qh();
    let serial = client.data.outputs.serial.expect("serial");
    let head = client
        .data
        .outputs
        .by_name("A")
        .and_then(|h| h.proxy.clone())
        .expect("A");

    let stale = manager.create_configuration(serial.wrapping_sub(1), &qh, ());
    let ch: ZwlrOutputConfigurationHeadV1 = stale.enable_head(&head, &qh, ());
    ch.set_scale(1.5);
    stale.apply();
    fixture.roundtrip(&mut client);
    assert_eq!(
        client.data.outputs.results.get(&stale.id()),
        Some(&"cancelled")
    );

    let test = manager.create_configuration(serial, &qh, ());
    let ch: ZwlrOutputConfigurationHeadV1 = test.enable_head(&head, &qh, ());
    ch.set_scale(1.5);
    test.test();
    fixture.roundtrip(&mut client);
    assert_eq!(
        client.data.outputs.results.get(&test.id()),
        Some(&"succeeded")
    );
    assert_eq!(fixture.output("A").current_scale().fractional_scale(), 1.0);

    // Turning the last monitor off is refused.
    let off = manager.create_configuration(serial, &qh, ());
    off.disable_head(&head);
    off.apply();
    fixture.roundtrip(&mut client);
    assert_eq!(client.data.outputs.results.get(&off.id()), Some(&"failed"));
}

#[test]
fn disabled_monitors_stay_listed_and_come_back() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let mut client = fixture.client();
    let manager = bind(&mut fixture, &mut client);
    let qh = client.qh();
    let serial = client.data.outputs.serial.expect("serial");
    let head = client
        .data
        .outputs
        .by_name("B")
        .and_then(|h| h.proxy.clone())
        .expect("B");
    let config = manager.create_configuration(serial, &qh, ());
    config.disable_head(&head);
    config.apply();
    fixture.roundtrip(&mut client);
    fixture.roundtrip(&mut client);
    assert_eq!(
        client.data.outputs.results.get(&config.id()),
        Some(&"succeeded")
    );
    assert!(!fixture.state.outputs.keys().any(|o| o.name() == "B"));
    let b = client.data.outputs.by_name("B").expect("B still listed");
    assert!(!b.enabled);

    let serial = client.data.outputs.serial.expect("serial");
    let config = manager.create_configuration(serial, &qh, ());
    let _ch: ZwlrOutputConfigurationHeadV1 = config.enable_head(&head, &qh, ());
    config.apply();
    fixture.roundtrip(&mut client);
    fixture.roundtrip(&mut client);
    assert_eq!(
        client.data.outputs.results.get(&config.id()),
        Some(&"succeeded")
    );
    assert!(fixture.state.outputs.keys().any(|o| o.name() == "B"));
    assert!(client.data.outputs.by_name("B").is_some_and(|h| h.enabled));
}

#[test]
fn apply_heads_checks_before_changing() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let state = &mut fixture.state;
    let mut bad = crate::outputs::config::HeadChange::new("A", true);
    bad.scale = Some(20.0);
    assert!(state.apply_heads(&[bad], false).is_err());
    let mut missing = crate::outputs::config::HeadChange::new("nope", true);
    missing.transform = Some(Transform::_90);
    assert!(state.apply_heads(&[missing], false).is_err());
    let mut rotate = crate::outputs::config::HeadChange::new("A", true);
    rotate.transform = Some(Transform::_90);
    assert!(state.apply_heads(&[rotate], false).is_ok());
    assert_eq!(fixture.output("A").current_transform(), Transform::_90);
}
