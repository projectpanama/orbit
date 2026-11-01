use smithay::wayland::output::OutputHandler;

use crate::state::Orbit;

mod compositor;

impl OutputHandler for Orbit {}
