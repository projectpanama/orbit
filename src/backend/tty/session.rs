use smithay::{backend::session::Session, output::Output, reexports::rustix::fs::Dev};

use crate::state::Orbit;

use super::Backend;

impl Orbit {
    /// Restores the native session after returning from another VT.
    ///
    /// Monitors are rescanned and pending frames are marked lost.
    pub(super) fn on_session_resumed(&mut self) {
        tracing::info!("session resumed");

        let Some(tty) = self.backend.as_mut().and_then(Backend::tty) else {
            return;
        };
        if tty.libinput.resume().is_err() {
            tracing::warn!("failed to resume libinput");
        }
        if let Err(err) = tty.drm.activate(false) {
            tracing::warn!(%err, "failed to reactivate the DRM device");
        }

        let outputs = self
            .with_backend(|backend, orbit| {
                let tty = backend.tty()?;
                // Retry monitors that failed earlier.
                // Keep intentionally dark monitors quiet in the log.
                tty.skipped.clear();
                tty.scan_connectors(orbit, true);
                let outputs: Vec<Output> = tty
                    .surfaces
                    .values_mut()
                    .map(|surface| {
                        surface.compositor.reset_buffers();
                        surface.output.clone()
                    })
                    .collect();
                Some(outputs)
            })
            .flatten()
            .unwrap_or_default();
        // The vblanks we were waiting for will never come.
        for output in outputs {
            self.frame_lost(&output);
        }
    }

    /// Handles removal of the GPU used by the native backend.
    ///
    /// Its outputs are removed and clients keep running. Drawing stops until
    /// Orbit is restarted.
    pub(super) fn on_gpu_removed(&mut self, device_id: Dev) {
        let outputs = self
            .with_backend(|backend, orbit| {
                let tty = backend.tty()?;
                if tty.device_id != Some(device_id) || tty.gpu_removed {
                    return None;
                }
                tty.gpu_removed = true;
                // The device file can only report errors from this point.
                if let Some(token) = tty.drm_token.take() {
                    orbit.loop_handle.remove(token);
                }
                tty.skipped.clear();
                let outputs: Vec<Output> = tty
                    .surfaces
                    .drain()
                    .map(|(_, surface)| surface.output)
                    .collect();
                Some(outputs)
            })
            .flatten();
        let Some(outputs) = outputs else {
            tracing::info!("a secondary GPU was removed");
            return;
        };
        tracing::error!(
            "the GPU Orbit draws with was removed; nothing can be shown until Orbit is restarted"
        );
        for output in outputs {
            self.remove_output(&output);
        }
    }

    /// Handles a GPU change such as a monitor being connected or removed.
    pub(super) fn on_gpu_changed(&mut self, device_id: Dev) {
        self.with_backend(|backend, orbit| {
            // Defer changes while another VT owns the display.
            // The session resume path rescans later.
            if let Some(tty) = backend.tty()
                && tty.device_id == Some(device_id)
                && tty.session.is_active()
            {
                // The kernel has already probed the connectors by now.
                tty.scan_connectors(orbit, false);
            }
        });
    }
}
