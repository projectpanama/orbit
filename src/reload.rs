use std::{ffi::OsString, os::fd::OwnedFd, path::PathBuf, sync::Arc, time::Duration};

use calloop::signals::{Signal, Signals};
use orbit_config::{Config, ConfigLoader, ModKey};
use smithay::reexports::{
    calloop::{
        Interest, Mode, PostAction,
        generic::Generic,
        timer::{TimeoutAction, Timer},
    },
    rustix::fs::inotify,
};

use crate::{backend::Backend, state::Orbit, window::decorations::DecorationStyle};

/// Quiet period required before a file change triggers a reload.
const SETTLE: Duration = Duration::from_millis(200);

impl Orbit {
    /// Starts watching configuration files and SIGHUP.
    ///
    /// File changes are debounced so editors that write in several steps
    /// cause one reload after the changes settle.
    pub fn watch_config(&mut self, loader: ConfigLoader) {
        let names: Vec<(PathBuf, OsString)> = loader
            .source()
            .paths()
            .into_iter()
            .filter_map(|p| Some((p.parent()?.to_path_buf(), p.file_name()?.to_os_string())))
            .collect();

        self.config_reload.loader = Some(loader);

        match inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK) {
            Ok(fd) => {
                let mut watched = Vec::new();
                for (dir, _) in &names {
                    if watched.contains(dir) {
                        continue;
                    }
                    let flags = inotify::WatchFlags::CLOSE_WRITE
                        | inotify::WatchFlags::MOVED_TO
                        | inotify::WatchFlags::MOVED_FROM
                        | inotify::WatchFlags::CREATE
                        | inotify::WatchFlags::DELETE;
                    if let Err(err) = inotify::add_watch(&fd, dir, flags) {
                        tracing::debug!(dir = %dir.display(), %err, "can't watch config directory");
                    } else {
                        watched.push(dir.clone());
                    }
                }
                let names: Arc<Vec<OsString>> =
                    Arc::new(names.into_iter().map(|(_, n)| n).collect());
                let source = Generic::new(fd, Interest::READ, Mode::Level);
                let result = self.loop_handle.insert_source(source, move |_, fd, state| {
                    if config_event(fd, &names) {
                        state.schedule_reload();
                    }
                    Ok(PostAction::Continue)
                });
                if let Err(err) = result {
                    tracing::warn!(%err, "failed to watch the config files");
                }
            }
            Err(err) => {
                tracing::warn!(%err, "inotify unavailable; the config reloads only on SIGHUP")
            }
        }

        match Signals::new(&[Signal::SIGHUP]) {
            Ok(signals) => {
                if let Err(err) = self.loop_handle.insert_source(signals, |_, _, state| {
                    tracing::info!("SIGHUP: reloading config");
                    state.reload_config();
                }) {
                    tracing::warn!(%err, "failed to listen for SIGHUP");
                }
            }
            Err(err) => tracing::warn!(%err, "failed to listen for SIGHUP"),
        }
    }

    /// Reads the configuration files again and applies the result.
    ///
    /// The call is ignored until configuration watching has been started.
    pub fn reload_config(&mut self) {
        let Some(loader) = self.config_reload.loader.as_mut() else {
            return;
        };
        let new = loader.load();
        self.apply_config(new);
        tracing::info!("reloaded config");
    }

    /// Applies a new configuration to the running compositor.
    ///
    /// Input, decoration, and output settings are updated when they differ.
    /// A full redraw is requested after the changes are applied.
    pub fn apply_config(&mut self, new: Config) {
        let old = std::mem::replace(&mut self.config, new);
        let new = &self.config;
        if let Some(mod_key) = new.mod_key {
            self.input.mod_key = mod_key;
        } else if old.mod_key.is_some() {
            self.input.mod_key = match self.backend {
                Some(Backend::Winit(_)) => ModKey::Alt,
                _ => ModKey::Super,
            };
        }
        if old.desktop_integration != new.desktop_integration {
            tracing::warn!("[desktop-integration] changes take effect after a restart");
        }

        let decorations = old.decorations != new.decorations;
        let input = old.input != new.input;
        let outputs = old.outputs_differ(new);
        if decorations {
            self.restyle_decorations();
        }
        if input {
            self.apply_input_config();
        }
        if outputs {
            self.reapply_output_config();
        }
        self.queue_redraw_all();
    }

    /// Applies the input configuration to the keyboard and pointer devices.
    pub fn apply_input_config(&mut self) {
        self.apply_keyboard_config();
        let input = self.config.input.clone();
        if let Some(tty) = self.backend.as_mut().and_then(Backend::tty) {
            tty.configure_devices(&input);
        }
    }

    /// Schedules a debounced configuration reload.
    ///
    /// An existing timer is replaced when another file event arrives.
    fn schedule_reload(&mut self) {
        if let Some(token) = self.config_reload.timer.take() {
            self.loop_handle.remove(token);
        }
        self.config_reload.timer = self
            .loop_handle
            .insert_source(Timer::from_duration(SETTLE), |_, _, state| {
                state.config_reload.timer = None;
                state.reload_config();
                TimeoutAction::Drop
            })
            .ok();
    }

    /// Applies the configured title bar style to every window.
    ///
    /// A changed font loads in the background. Existing bars keep the old
    /// font until the new one is ready.
    fn restyle_decorations(&mut self) {
        let config = self.config.decorations.clone();
        if self.decoration_style.font_differs(&config) {
            self.font_loader.request(&config);
        }
        let font = config
            .title
            .then(|| self.decoration_style.font().cloned())
            .flatten();
        self.set_decoration_style(Arc::new(DecorationStyle::new(config, font)));
        self.arrange_all();
    }
}

/// Drains inotify events and reports whether a configuration file changed.
fn config_event(fd: &OwnedFd, names: &[OsString]) -> bool {
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
    let mut reader = inotify::Reader::new(fd, &mut buf);
    let mut ours = false;
    while let Ok(event) = reader.next() {
        if let Some(name) = event.file_name() {
            let name = std::ffi::OsStr::new(&*name.to_string_lossy()).to_os_string();
            ours |= names.contains(&name);
        }
    }
    ours
}
