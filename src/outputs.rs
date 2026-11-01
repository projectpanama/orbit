use std::time::Duration;

use orbit_core::{
    animation::workspace::WorkspaceSlide,
    frame::FrameScheduler,
    ids::{OutputId, WorkspaceId},
    layout::geometry::Point,
};
use smithay::{
    desktop::Window,
    output::Output,
    reexports::wayland_server::backend::GlobalId,
    utils::{Logical, Rectangle},
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
    // pub overview: Option<OverviewState>,
}

struct WindowHome {
    window: Window,
    output: Output,
    geometry: Rectangle<i32, Logical>,
}
