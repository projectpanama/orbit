use std::collections::HashMap;

use orbit_core::frame::FrameOutcome;
use smithay::{backend::renderer::pixman::PixmanRenderer, output::Output};

use crate::state::Orbit;

pub struct Headless {
    pub outputs: Vec<Output>,
    pub disabled: Vec<Output>,
    pub renderer: Option<PixmanRenderer>,
    pub renders: HashMap<String, u32>,
}

impl Headless {
    pub fn render(&mut self, state: &mut Orbit, output: &Output) -> FrameOutcome {
        if !self.outputs.contains(output) {
            return FrameOutcome::Skipped;
        }
        *self.renders.entry(output.name()).or_default() += 1;
        state.post_render(output, None);
        FrameOutcome::NoDamage
    }
}

pub fn init(state: &mut Orbit) {
    state.backend = Some(super::Backend::Headless(Box::new(Headless {
        disabled: Vec::new(),
        outputs: Vec::new(),
        renderer: PixmanRenderer::new().ok(),
        renders: HashMap::new(),
    })));
}
