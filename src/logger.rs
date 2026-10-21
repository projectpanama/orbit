use anyhow::{Context, Result};
use tracing_subscriber::{EnvFilter, prelude::*};

pub fn init() -> Result<()> {
    let (baseline, verbose) = if cfg!(debug_assertions) {
        ("info", "debug")
    } else {
        ("warn", "warn")
    };

    let mut filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(baseline));

    for directive in [
        format!("orbit={verbose}"),
        format!("smithay={verbose}"),
        "calloop=error".to_owned(),
    ] {
        filter = filter.add_directive(directive.parse()?);
    }

    let (journald, journald_err) = match std::env::var_os("JOURNAL_STREAM") {
        None => (None, None),
        Some(_) => match tracing_journald::layer() {
            Ok(layer) => (Some(layer.with_syslog_identifier("orbit".into())), None),
            Err(err) => (None, Some(err)),
        },
    };

    let stderr = journald
        .is_none()
        .then(|| tracing_subscriber::fmt::layer().compact());

    tracing_subscriber::registry()
        .with(filter)
        .with(journald)
        .with(stderr)
        .try_init()
        .context("could not install the tracing subscriber")?;

    if let Some(err) = journald_err {
        tracing::warn!(?err, "journald is unavailable, logging to stderr");
    }

    log_panics_with_tracing();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        debug_build = cfg!(debug_assertions),
        "Starting Orbit"
    );

    Ok(())
}

fn log_panics_with_tracing() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        let message = info
            .payload_as_str()
            .unwrap_or("<non-string panic payload>");
        let location = info
            .location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::capture();

        if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            tracing::error!(target: "panic", thread, location, "{message}\n{backtrace}");
        } else {
            tracing::error!(target: "panic", thread, location, "{message}");
        }
    }));
}
