//! In-process tests: Orbit on the headless backend,
//! driven by real Wayland clients. See `fixture.rs`.

mod bench;
mod capture;
mod client;
mod clients;
mod fixture;
mod geometry;
mod output_management;
mod outputs;
mod overview;
mod protocols;
mod redraw;
mod reload;
mod session;
mod windows;

use self::{
    client::{TestClient, TestWindow},
    fixture::Fixture,
};

/// Opens a window and maps it at its configured (or a default) size.
fn map_window(fixture: &mut Fixture, client: &mut TestClient) -> TestWindow {
    let window = client.create_window();
    fixture.roundtrip(client);
    client.attach(&window, (640, 480));
    fixture.roundtrip(client);
    window
}
