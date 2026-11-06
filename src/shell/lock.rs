use std::collections::HashMap;

use smithay::{
    backend::input::InputTime,
    desktop::{WindowSurfaceType, utils::under_from_surface_tree},
    output::Output,
    reexports::{
        wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1,
        wayland_server::{
            Resource,
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};

use crate::state::Orbit;

#[derive(Debug, Default)]
pub enum LockState {
    /// No client is currently locking the session.
    #[default]
    Unlocked,
    /// A client is acquiring the lock and the desktop is being hidden.
    ///
    /// The `locked` event is sent once every output in `pending` has drawn a
    /// frame without desktop content.
    Locking {
        /// Client session-lock resource waiting for confirmation.
        locker: SessionLocker,
        /// Outputs that have not yet drawn a desktop-free frame.
        pending: Vec<Output>,
    },
    /// A client owns the lock and the desktop is hidden.
    ///
    /// If `owner` dies, the session remains locked but another client may acquire it.
    Locked {
        /// Session-lock resource that owns the confirmed lock.
        owner: ExtSessionLockV1,
    },
}

/// Session-lock state and lock surfaces associated with each output.
#[derive(Debug, Default)]
pub struct SessionLock {
    /// Current lock lifecycle state.
    pub state: LockState,
    /// Lock surfaces indexed by the output on which they are displayed.
    pub surfaces: HashMap<Output, LockSurface>,
}

impl SessionLock {
    /// Returns whether the desktop is hidden or being hidden.
    ///
    /// This remains `true` after the owning client dies, until another client
    /// unlocks the session or takes over the lock.
    pub fn is_active(&self) -> bool {
        !matches!(self.state, LockState::Unlocked)
    }

    /// Returns the live lock surface associated with `output`.
    ///
    /// Dead protocol resources are treated as absent.
    pub fn surface_on(&self, output: &Output) -> Option<&LockSurface> {
        self.surfaces.get(output).filter(|s| s.alive())
    }

    /// Returns the session-lock resource currently acquiring or owning the lock.
    fn owner(&self) -> Option<&ExtSessionLockV1> {
        match &self.state {
            LockState::Unlocked => None,
            LockState::Locking { locker, .. } => Some(locker.ext_session_lock()),
            LockState::Locked { owner } => Some(owner),
        }
    }
}

impl Orbit {
    /// Selects the lock surface that should receive keyboard focus.
    ///
    /// The live surface on the focused output is preferred. If it is not
    /// available, the first live lock surface on any output is used.
    pub fn lock_focus_target(&self) -> Option<WlSurface> {
        let lock = &self.session_lock;
        self.focused_output()
            .and_then(|output| lock.surface_on(&output))
            .or_else(|| lock.surfaces.values().find(|s| s.alive()))
            .map(|s| s.wl_surface().clone())
    }

    /// Gives keyboard focus to the selected lock surface.
    ///
    /// Focus is updated only when the target differs from the current focus.
    pub fn focus_lock_surface(&mut self) {
        let target = self.lock_focus_target();
        let keyboard = self.keyboard.clone();
        if keyboard.current_focus() != target {
            keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
        }
    }

    /// Finds the lock surface or one of its subsurfaces under `pos`.
    ///
    /// Returns the hit surface and its location in global logical
    /// coordinates. Returns `None` when the position is outside every output
    /// or no live lock surface covers it.
    pub fn lock_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.space.output_under(pos).next()?;
        let output_loc = self.space.output_geometry(output)?.loc;
        let surface = self.session_lock.surface_on(output)?;
        let (surface, loc) = under_from_surface_tree(
            surface.wl_surface(),
            pos - output_loc.to_f64(),
            (0, 0),
            WindowSurfaceType::ALL,
        )?;
        Some((surface, (loc + output_loc).to_f64()))
    }

    /// Returns the output associated with a lock surface.
    pub fn output_of_lock_surface(&self, surface: &WlSurface) -> Option<Output> {
        self.session_lock
            .surfaces
            .iter()
            .find(|(_, s)| s.wl_surface() == surface)
            .map(|(output, _)| output.clone())
    }

    /// Configures `output`'s lock surface to cover the output.
    ///
    /// The pending size is at least one pixel in each dimension. A configure
    /// event is sent only when the size changed.
    pub fn configure_lock_surface(&self, output: &Output) {
        let Some(surface) = self.session_lock.surface_on(output) else {
            return;
        };
        let Some(geo) = self.space.output_geometry(output) else {
            return;
        };
        let size = (geo.size.w.max(1) as u32, geo.size.h.max(1) as u32);
        let changed = surface.with_pending_state(|state| {
            let changed = state.size != Some(size.into());
            state.size = Some(size.into());
            changed
        });
        if changed {
            surface.send_configure();
        }
    }

    /// Removes dead lock surfaces and reconfigures all remaining surfaces.
    ///
    /// This is called after output geometry, mode, or scale changes.
    pub fn reconfigure_lock_surfaces(&mut self) {
        self.session_lock.surfaces.retain(|_, s| s.alive());
        let outputs: Vec<Output> = self.session_lock.surfaces.keys().cloned().collect();
        for output in outputs {
            self.configure_lock_surface(&output);
        }
    }

    /// Records that `output` drew a desktop-free frame during lock acquisition.
    ///
    /// Once every pending output has drawn such a frame, the lock is confirmed.
    /// Outputs removed during acquisition no longer block confirmation.
    pub fn lock_frame_drawn(&mut self, output: &Output) {
        let LockState::Locking { pending, .. } = &mut self.session_lock.state else {
            return;
        };
        pending.retain(|o| o != output);
        // Outputs unplugged meanwhile don't hold the lock up.
        pending.retain(|o| self.outputs.contains_key(o));
        if pending.is_empty() {
            self.confirm_lock();
        }
    }

    /// Sends the protocol's `locked` event after all outputs are safe to show
    /// without desktop content.
    fn confirm_lock(&mut self) {
        let state = std::mem::take(&mut self.session_lock.state);
        let LockState::Locking { locker, .. } = state else {
            self.session_lock.state = state;
            return;
        };
        let owner = locker.ext_session_lock().clone();
        locker.lock();
        tracing::info!("session locked");
        self.session_lock.state = LockState::Locked { owner };
    }

    /// Removes an output's lock surface and releases any pending lock wait.
    ///
    /// If the session is active, keyboard focus is refreshed after removal.
    pub fn lock_output_removed(&mut self, output: &Output) {
        self.session_lock.surfaces.remove(output);
        if self.session_lock.is_active() {
            self.lock_frame_drawn(output);
            self.focus_lock_surface();
        }
    }
}

impl SessionLockHandler for Orbit {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // If the current owner is alive, it has the lock and we refuse a second locker.
        if self
            .session_lock
            .owner()
            .is_some_and(|owner| owner.is_alive())
        {
            tracing::debug!("refusing a second session lock");
            return;
        }

        let relock = self.session_lock.is_active();

        // A previous locker's surfaces are gone with it.
        self.session_lock.surfaces.retain(|_, s| s.alive());

        // Nothing but the lock may keep input.
        let time = InputTime::now();
        self.pointer
            .clone()
            .unset_grab(self, SERIAL_COUNTER.next_serial(), time);
        self.keyboard.clone().unset_grab(self);
        self.input.workspace_swipe = None;
        self.drop_overviews();

        let pending: Vec<Output> = self.outputs.keys().cloned().collect();
        self.session_lock.state = LockState::Locking {
            locker: confirmation,
            pending,
        };

        if relock || self.outputs.is_empty() {
            // Already showing no desktop (or nothing at all).
            self.confirm_lock();
        }

        self.focus_lock_surface();
        self.queue_redraw_all();
    }

    fn unlock(&mut self) {
        tracing::info!("session unlocked");
        self.session_lock = SessionLock::default();
        self.restore_window_focus();
        self.queue_redraw_all();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        // Only the current locker's surfaces count.
        let ours = self
            .session_lock
            .owner()
            .is_some_and(|owner| owner == surface.ext_session_lock());
        let Some(output) = Output::from_resource(&wl_output) else {
            return;
        };
        if !ours {
            return;
        }
        self.session_lock.surfaces.insert(output.clone(), surface);
        // Smithay sends the configuration right after this returns.
        if let Some(geo) = self.space.output_geometry(&output)
            && let Some(s) = self.session_lock.surfaces.get(&output)
        {
            let size = (geo.size.w.max(1) as u32, geo.size.h.max(1) as u32);
            s.with_pending_state(|state| state.size = Some(size.into()));
        }
        self.focus_lock_surface();
        self.queue_redraw_all();
    }
}
