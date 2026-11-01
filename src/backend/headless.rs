use std::collections::HashMap;

use orbit_core::frame::FrameOutcome;
use smithay::{
    backend::renderer::pixman::PixmanRenderer,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    utils::Transform,
};

use crate::{outputs::OutputSetup, state::Orbit};

/// State for the headless rendering backend.
pub struct Headless {
    /// Virtual outputs currently rendered by this backend.
    pub outputs: Vec<Output>,
    /// Outputs temporarily excluded from rendering.
    pub disabled: Vec<Output>,
    /// Optional CPU-backed Pixman renderer.
    pub renderer: Option<PixmanRenderer>,
    /// Number of render passes completed for each output name.
    pub renders: HashMap<String, u32>,
}

impl Headless {
    /// Renders an active headless output and reports that it had no damage.
    pub fn render(&mut self, state: &mut Orbit, output: &Output) -> FrameOutcome {
        if !self.outputs.contains(output) {
            return FrameOutcome::Skipped;
        }
        *self.renders.entry(output.name()).or_default() += 1;
        state.post_render(output, None);
        FrameOutcome::NoDamage
    }
}

impl Orbit {
    /// Plugs in a virtual monitor of `size` physical pixels at 60 Hz.
    pub fn add_headless_output(&mut self, name: &str, size: (i32, i32), scale: f64) -> Output {
        let output = Output::new(
            name.to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Orbit".into(),
                model: "Headless".into(),
                serial_number: "Unknown".into(),
            },
        );
        let mode = Mode {
            size: size.into(),
            refresh: 60_000,
        };
        output.add_mode(mode);
        output.set_preferred(mode);
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Fractional(scale)),
            None,
        );
        if let Some(super::Backend::Headless(headless)) = self.backend.as_mut() {
            headless.outputs.push(output.clone());
        }
        self.add_output(
            &output,
            OutputSetup {
                configured_position: None,
                internal: false,
            },
        );
        output
    }

    /// Removes a virtual monitor from the headless backend.
    pub fn remove_headless_output(&mut self, output: &Output) {
        if let Some(super::Backend::Headless(headless)) = self.backend.as_mut() {
            headless.outputs.retain(|o| o != output);
        }
        self.remove_output(output);
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
