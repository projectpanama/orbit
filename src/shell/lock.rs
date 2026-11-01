use std::collections::HashMap;

use smithay::{
    output::Output,
    reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1,
    wayland::session_lock::{LockSurface, SessionLocker},
};

#[derive(Debug, Default)]
pub enum LockState {
    #[default]
    Unlocked,
    Locking {
        locker: SessionLocker,
        pending: Vec<Output>,
    },
    Locked {
        owner: ExtSessionLockV1,
    },
}

#[derive(Debug, Default)]
pub struct SessionLock {
    pub state: LockState,
    pub surfaces: HashMap<Output, LockSurface>,
}

impl SessionLock {
    pub fn is_active(&self) -> bool {
        !matches!(self.state, LockState::Unlocked)
    }

    pub fn surface_on(&self, output: &Output) -> Option<&LockSurface> {
        self.surfaces.get(output).filter(|s| s.alive())
    }

    fn owner(&self) -> Option<&ExtSessionLockV1> {
        match &self.state {
            LockState::Unlocked => None,
            LockState::Locking { locker, .. } => Some(locker.ext_session_lock()),
            LockState::Locked { owner } => Some(owner),
        }
    }
}
