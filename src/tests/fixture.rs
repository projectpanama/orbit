//! An Orbit running in the test process on the headless backend, and
//! real Wayland clients connected to it over socket pairs.
//!
//! Everything runs on the test's thread. Nothing blocks: the fixture
//! alternates between dispatching Orbit's event loop and reading the
//! client's socket until the client's `wl_display.sync` comes back.

use std::{os::unix::net::UnixStream, sync::Arc, time::Duration};

use orbit_config::Config;
use smithay::{
    output::Output,
    reexports::{
        calloop::EventLoop,
        wayland_server::{Client, Display},
    },
};
use wayland_client::{Connection, backend::WaylandError};

use super::client::TestClient;

use crate::{
    backend::headless,
    state::{Orbit, client::ClientState},
};

/// Gives up on a roundtrip after this many server/client turns. Every
/// roundtrip in the tests takes a handful; hitting this means a deadlock.
const MAX_TURNS: usize = 1000;

pub struct Fixture {
    pub event_loop: EventLoop<'static, Orbit>,
    pub state: Orbit,
}

impl Fixture {
    /// Orbit with the default config and no outputs.
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    pub fn with_config(config: Config) -> Self {
        let mut event_loop = EventLoop::try_new().expect("event loop");
        let display = Display::new().expect("display");
        let mut state = Orbit::new(&mut event_loop, display, config).expect("orbit");
        headless::init(&mut state);
        Self { event_loop, state }
    }

    /// Orbit with outputs of the given names, 1920×1080 at scale 1, side
    /// by side in that order.
    pub fn with_outputs(names: &[&str]) -> Self {
        let mut fixture = Self::new();
        for name in names {
            fixture.add_output(name);
        }
        fixture
    }

    pub fn add_output(&mut self, name: &str) -> Output {
        let output = self.state.add_headless_output(name, (1920, 1080), 1.0);
        self.dispatch();
        output
    }

    pub fn remove_output(&mut self, output: &Output) {
        self.state.remove_headless_output(output);
        self.dispatch();
    }

    pub fn output(&self, name: &str) -> Output {
        self.state
            .outputs
            .keys()
            .find(|output| output.name() == name)
            .cloned()
            .unwrap_or_else(|| panic!("no output {name}"))
    }

    /// Connects a new client.
    pub fn client(&mut self) -> TestClient {
        let (server, client) = UnixStream::pair().expect("socket pair");
        let handle = self
            .state
            .display_handle
            .insert_client(server, Arc::new(ClientState::default()))
            .expect("insert client");
        client.set_nonblocking(true).expect("non-blocking socket");
        let conn = Connection::from_socket(client).expect("connection");
        let mut client = TestClient::new(conn, handle);
        self.roundtrip(&mut client);
        client.bind_globals();
        self.roundtrip(&mut client);
        client
    }

    /// Runs one turn of Orbit's event loop, as `EventLoop::run` would.
    pub fn dispatch(&mut self) {
        self.event_loop
            .dispatch(Some(Duration::ZERO), &mut self.state)
            .expect("dispatch");
        self.state.after_dispatch();
    }

    /// Runs Orbit's event loop for `duration`, firing its timers.
    pub fn advance(&mut self, duration: Duration) {
        let end = std::time::Instant::now() + duration;
        while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
            self.event_loop
                .dispatch(Some(left.min(Duration::from_millis(5))), &mut self.state)
                .expect("dispatch");
            self.state.after_dispatch();
        }
    }

    /// Sends everything `client` queued and handles everything Orbit sent
    /// back, up to and including the reply to a `wl_display.sync`. Panics
    /// if Orbit disconnects the client; use [`Fixture::try_roundtrip`] to
    /// test that.
    pub fn roundtrip(&mut self, client: &mut TestClient) {
        if let Err(err) = self.try_roundtrip(client) {
            panic!("roundtrip failed: {err}");
        }
    }

    pub fn try_roundtrip(&mut self, client: &mut TestClient) -> Result<(), WaylandError> {
        let done = client.sync();
        for _ in 0..MAX_TURNS {
            client.flush()?;
            self.dispatch();
            client.read()?;
            if done.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(());
            }
        }
        panic!("no reply from Orbit after {MAX_TURNS} turns");
    }

    /// How many frames output `name` has drawn so far.
    pub fn renders(&self, name: &str) -> u32 {
        match self.state.backend.as_ref() {
            Some(crate::backend::Backend::Headless(headless)) => {
                headless.renders.get(name).copied().unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// Whether Orbit still has `client` connected.
    pub fn is_connected(&self, client: &Client) -> bool {
        self.state
            .display_handle
            .backend_handle()
            .get_client_data(client.id())
            .is_ok()
    }
}
