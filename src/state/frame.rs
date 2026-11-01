use std::time::Duration;

use smithay::output::Output;

/// Tracks scheduled frame work and the outputs waiting to be rendered.
#[derive(Debug, Default)]
pub struct FrameSchedule {
    /// Whether a render pass has already been scheduled.
    pub renders_scheduled: bool,
    /// Whether post-frame cleanup has already been scheduled.
    pub cleanup_scheduled: bool,
    /// Outputs queued for the next render pass.
    pub queued_outputs: Vec<Output>,
    /// When the frame being drawn will be shown on the monotonic clock.
    pub frame_time: Duration,
}
