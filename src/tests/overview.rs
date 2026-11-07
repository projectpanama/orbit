//! `orbit_overview_v1`: an output's overview, as a shell follows and
//! drives it.

use std::{collections::HashMap, time::Duration};

use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, backend::ObjectId, delegate_noop,
    protocol::wl_output::WlOutput,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1;

use self::generated::{
    orbit_overview_manager_v1::OrbitOverviewManagerV1,
    orbit_overview_v1::{self, OrbitOverviewV1},
};
use super::{Fixture, TestClient, client::ClientData, map_window, session::bind_all_outputs};

use orbit_protocols::overview::client as generated;

type Rect = (i32, i32, i32, i32);

#[derive(Default)]
pub struct Overviews {
    /// Every progress value each overview object got.
    pub progress: HashMap<ObjectId, Vec<f64>>,
    /// Completed layouts (window handle, rect), newest last.
    pub layouts: HashMap<ObjectId, Vec<Vec<(ObjectId, Rect)>>>,
    pending: HashMap<ObjectId, Vec<(ObjectId, Rect)>>,
}

struct Shell {
    manager: OrbitOverviewManagerV1,
    outputs: Vec<WlOutput>,
    _list: ExtForeignToplevelListV1,
}

fn bind(fixture: &mut Fixture, client: &mut TestClient) -> Shell {
    let qh = client.qh();
    let registry = client.registry();
    let shell = Shell {
        manager: client.bind(&registry, "orbit_overview_manager_v1", 1, &qh),
        _list: client.bind(&registry, "ext_foreign_toplevel_list_v1", 1, &qh),
        outputs: bind_all_outputs(client),
    };
    fixture.roundtrip(client);
    shell
}

fn last_progress(client: &TestClient, o: &OrbitOverviewV1) -> Option<f64> {
    client
        .data
        .overview
        .progress
        .get(&o.id())
        .and_then(|v| v.last().copied())
}

fn last_layout(client: &TestClient, o: &OrbitOverviewV1) -> Option<Vec<(ObjectId, Rect)>> {
    client
        .data
        .overview
        .layouts
        .get(&o.id())
        .and_then(|v| v.last().cloned())
}

/// Lets the animation run out, handling events meanwhile.
fn settle(fixture: &mut Fixture, client: &mut TestClient) {
    fixture.advance(Duration::from_millis(500));
    fixture.roundtrip(client);
}

#[test]
fn opens_lays_out_and_closes() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut apps = fixture.client();
    let _a = map_window(&mut fixture, &mut apps);
    let _b = map_window(&mut fixture, &mut apps);

    let mut client = fixture.client();
    let shell = bind(&mut fixture, &mut client);
    let qh = client.qh();
    let overview = shell.manager.get_overview(&shell.outputs[0], &qh, ());
    fixture.roundtrip(&mut client);
    assert_eq!(
        last_progress(&client, &overview),
        Some(0.0),
        "progress on creation"
    );
    assert!(last_layout(&client, &overview).is_none());

    overview.open();
    fixture.roundtrip(&mut client);
    settle(&mut fixture, &mut client);
    let progress = client.data.overview.progress[&overview.id()].clone();
    assert_eq!(progress.last(), Some(&1.0), "settles at exactly 1");
    assert!(
        progress.windows(2).all(|w| w[0] <= w[1]),
        "rises: {progress:?}"
    );

    let layout = last_layout(&client, &overview).expect("layout sent");
    assert_eq!(layout.len(), 2, "both windows");
    let handles: Vec<ObjectId> = client
        .data
        .desktop
        .toplevels
        .iter()
        .map(|h| h.id())
        .collect();
    for (i, (handle, (x, y, w, h))) in layout.iter().enumerate() {
        assert!(handles.contains(handle), "the client's own handles");
        assert!(
            *x >= 0 && *y >= 0 && x + w <= 1920 && y + h <= 1080,
            "inside the output"
        );
        assert!(
            *y >= crate::shell::overview::TOP_BAND,
            "below the shell's band"
        );
        for (_, (x2, y2, w2, h2)) in &layout[i + 1..] {
            let apart = x + w <= *x2 || x2 + w2 <= *x || y + h <= *y2 || y2 + h2 <= *y;
            assert!(apart, "windows don't overlap");
        }
    }

    // A window opening re-lays it out.
    let _c = map_window(&mut fixture, &mut apps);
    fixture.advance(Duration::from_millis(50));
    fixture.roundtrip(&mut client);
    assert_eq!(last_layout(&client, &overview).map(|l| l.len()), Some(3));

    overview.close();
    fixture.roundtrip(&mut client);
    settle(&mut fixture, &mut client);
    assert_eq!(
        last_progress(&client, &overview),
        Some(0.0),
        "settles at exactly 0"
    );
    let output = fixture.output("A");
    assert!(
        fixture.state.overview(&output).is_none(),
        "nothing kept once closed"
    );
}

#[test]
fn outputs_are_independent() {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let mut client = fixture.client();
    let shell = bind(&mut fixture, &mut client);
    let qh = client.qh();
    let on_a = shell.manager.get_overview(&shell.outputs[0], &qh, ());
    let on_b = shell.manager.get_overview(&shell.outputs[1], &qh, ());
    fixture.roundtrip(&mut client);
    on_a.open();
    fixture.roundtrip(&mut client);
    settle(&mut fixture, &mut client);
    assert_eq!(last_progress(&client, &on_a), Some(1.0));
    assert_eq!(last_progress(&client, &on_b), Some(0.0));
    let b = fixture.output("B");
    assert!(fixture.state.overview(&b).is_none());
    assert!(!fixture.state.in_overview(&b));
}

#[test]
fn clicking_a_window_activates_it_and_closes() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let mut apps = fixture.client();
    let _a = map_window(&mut fixture, &mut apps);
    let _b = map_window(&mut fixture, &mut apps);
    let output = fixture.output("A");
    fixture.state.open_overview(&output);
    fixture.advance(Duration::from_millis(500));

    let focused = fixture.state.focused_window();
    let (target_id, rect) = fixture
        .state
        .overview(&output)
        .expect("open")
        .layout()
        .iter()
        .find(|(id, _)| focused.as_ref().and_then(crate::workspaces::window_id) != Some(*id))
        .copied()
        .expect("an unfocused window");
    let center = (
        f64::from(rect.loc.x + rect.size.w / 2),
        f64::from(rect.loc.y + rect.size.h / 2),
    );
    assert!(fixture.state.overview_click(center.into()));
    let now_focused = fixture
        .state
        .focused_window()
        .and_then(|w| crate::workspaces::window_id(&w));
    assert_eq!(
        now_focused,
        Some(target_id),
        "the clicked window is focused"
    );
    fixture.advance(Duration::from_millis(500));
    assert!(
        fixture.state.overview(&output).is_none(),
        "and the overview closed"
    );

    // Clicking empty space just closes it.
    fixture.state.open_overview(&output);
    fixture.advance(Duration::from_millis(500));
    assert!(fixture.state.overview_click((5.0, 5.0).into()));
    fixture.advance(Duration::from_millis(500));
    assert!(fixture.state.overview(&output).is_none());
}

impl Dispatch<OrbitOverviewV1, ()> for ClientData {
    fn event(
        state: &mut Self,
        overview: &OrbitOverviewV1,
        event: orbit_overview_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let o = &mut state.overview;
        match event {
            orbit_overview_v1::Event::Progress { value } => {
                o.progress.entry(overview.id()).or_default().push(value);
            }
            orbit_overview_v1::Event::Window {
                toplevel,
                x,
                y,
                width,
                height,
            } => {
                o.pending
                    .entry(overview.id())
                    .or_default()
                    .push((toplevel.id(), (x, y, width, height)));
            }
            orbit_overview_v1::Event::Done => {
                let layout = o.pending.remove(&overview.id()).unwrap_or_default();
                o.layouts.entry(overview.id()).or_default().push(layout);
            }
            _ => {}
        }
    }
}

delegate_noop!(ClientData: ignore OrbitOverviewManagerV1);
