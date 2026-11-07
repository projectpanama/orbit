mod args;
mod backend;
mod geometry;
mod handlers;
mod input;
mod logger;
mod outputs;
mod protocols;
mod reload;
mod render;
mod shell;
mod state;
mod window;
mod workspaces;

use std::process::ExitCode;

use orbit_config::{Config, ConfigLoader, ConfigSource};
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use crate::state::Orbit;

fn main() -> ExitCode {
    let args = match crate::args::parse() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("orbit: {err}\n\n{}", crate::args::USAGE);
            return ExitCode::FAILURE;
        }
    };

    let source = ConfigSource::new(args.configs.clone());
    if args.check_config {
        return check_config(&source);
    }

    if let Err(err) = crate::logger::init() {
        eprintln!("Orbit: {err:#}");
        return ExitCode::FAILURE;
    }

    // A no-op unless built with `--features profile-with-tracy`.
    let _tracy = tracy_client::Client::start();

    // Before the event loop exists, so this small write blocks nothing.
    match source.create_user_config() {
        Ok(Some(path)) => tracing::info!(path = %path.display(), "wrote a default config"),
        Ok(None) => {}
        Err(err) => tracing::warn!(%err, "failed to write a default config"),
    }

    let mut config_loader = ConfigLoader::new(source);
    let config = config_loader.load();

    match run(config_loader, config, args.nested, &args.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(
    loader: ConfigLoader,
    config: Config,
    nested: bool,
    command: &[String],
) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<Orbit> = EventLoop::try_new()?;
    let display: Display<Orbit> = Display::new()?;
    let mut state = Orbit::new(&mut event_loop, display, config)?;

    if nested {
        backend::winit::init(&mut event_loop, &mut state)?;
    } else {
        backend::tty::init(&mut event_loop, &mut state)?;
    }

    state.watch_config(loader);
    state.listen()?;

    tracing::info!(socket = ?state.socket_name, "Orbit is listening");

    if !nested {
        state.spawn(&[
            "dbus-update-activation-environment".into(),
            "--systemd".into(),
            "WAYLAND_DISPLAY".into(),
            "XDG_SESSION_TYPE".into(),
            "XDG_CURRENT_DESKTOP".into(),
        ]);
    }

    state.spawn_startup();
    state.spawn(command);
    event_loop.run(None, &mut state, Orbit::after_dispatch)?;

    Ok(())
}

fn check_config(source: &ConfigSource) -> ExitCode {
    // The config reports invalid values as warnings. Print just those,
    // under the name of the file they are in.
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_max_level(tracing::Level::WARN)
        .without_time()
        .with_target(false)
        .init();
    let mut ok = true;
    for path in source.paths() {
        println!("{}", path.display());
        if !source.is_given() && !path.exists() {
            println!("  not found (fine: it is optional)");
            continue;
        }
        match orbit_config::check(path) {
            Ok(_) => println!("  ok"),
            Err(err) => {
                ok = false;
                println!("  error: {err}");
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
