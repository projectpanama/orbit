use std::time::Duration;

use orbit_core::{
    animation::workspace::WorkspaceSlide,
    frame::FrameScheduler,
    ids::{OutputId, WorkspaceId},
    layout::{self, geometry::Point, output::Slot},
};
use smithay::{
    desktop::Window,
    output::Output,
    reexports::wayland_server::backend::GlobalId,
    utils::{Logical, Rectangle, Size},
};

use crate::{
    geometry::{ToCore, ToSmithay},
    state::Orbit,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct OutputSetup {
    pub configured_position: Option<Point>,
    pub internal: bool,
}

#[derive(Debug)]
pub struct OutputState {
    pub id: OutputId,
    pub frame: FrameScheduler,
    pub global: GlobalId,
    pub configured_position: Option<Point>,
    pub internal: bool,
    pub last_presented: Option<Duration>,
    pub workspace_slide: Option<WorkspaceSlide<WorkspaceId>>,
}

struct WindowHome {
    window: Window,
    output: Output,
    geometry: Rectangle<i32, Logical>,
}

impl Orbit {
    pub fn add_output(&mut self, output: &Output, setup: OutputSetup) {
        let global = output.create_global::<Orbit>(&self.display_handle);

        self.outputs.insert(
            output.clone(),
            OutputState {
                id: self.output_ids.allocate(),
                frame: FrameScheduler::default(),
                global,
                configured_position: setup.configured_position,
                internal: setup.internal,
                last_presented: None,
                workspace_slide: None,
            },
        );

        self.arrange_outputs();
    }

    fn arrange_outputs(&mut self) {
        let mut outputs: Vec<(&Output, &OutputState)> = self.outputs.iter().collect();

        // A stable order so that the same monitors always end up in the same places.
        outputs.sort_by(|(a, a_state), (b, b_state)| {
            b_state
                .internal
                .cmp(&a_state.internal)
                .then_with(|| a.name().cmp(&b.name()))
        });

        let slots: Vec<Slot> = outputs
            .iter()
            .map(|(output, state)| Slot {
                size: logical_size(output).to_core(),
                configured: state.configured_position,
            })
            .collect();

        let outputs: Vec<Output> = outputs.into_iter().map(|(o, _)| o.clone()).collect();

        // Arrange the outputs according to the slots and update their positions in the space.
        for (output, arranged) in outputs.iter().zip(layout::output::arrange(&slots)) {
            let position = arranged.position.to_smithay();
            if arranged.overlapped {
                tracing::warn!(
                    output = output.name(),
                    "configured output position overlaps another output, placing it automatically"
                );
            }
            if output.current_location() != position {
                output.change_current_state(None, None, None, Some(position));
            }
            self.space.map_output(output, position);
        }
    }
}

fn logical_size(output: &Output) -> Size<i32, Logical> {
    output.current_mode().map_or_else(Size::default, |mode| {
        output
            .current_transform()
            .transform_size(mode.size)
            .to_f64()
            .to_logical(output.current_scale().fractional_scale())
            .to_i32_ceil()
    })
}
