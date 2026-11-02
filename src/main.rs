mod backend;
mod geometry;
mod handlers;
mod input;
mod outputs;
mod protocols;
mod render;
mod shell;
mod state;
mod window;
mod workspaces;

use std::process::ExitCode;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use crate::state::Orbit;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let mut event_loop: EventLoop<Orbit> = EventLoop::try_new()?;
    let display: Display<Orbit> = Display::new()?;
    let mut state = Orbit::new(&mut event_loop, display)?;

    backend::winit::init(&mut event_loop, &mut state)?;

    state.listen()?;

    tracing::info!(socket = ?state.socket_name, "Orbit is listening");

    event_loop.run(None, &mut state, Orbit::after_dispatch)?;

    Ok(())
}
