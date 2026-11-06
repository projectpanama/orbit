use orbit_core::snap;
use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, GestureHoldBeginEvent,
        GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
        GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData,
        MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Logical, Point},
};

use crate::{
    geometry::{ToCore, ToSmithay},
    state::Orbit,
    window::{Layout, frame_at},
};

/// Drags a window until all pointer buttons are released.
///
/// The window magnetizes to nearby edges and displays a preview when the
/// pointer enters a maximize or half-tile drop zone.
pub struct MoveGrab {
    /// Pointer state captured when the grab began.
    start_data: GrabStartData<Orbit>,
    /// Window being moved.
    window: Window,
    /// Window content position before the drag began.
    initial_window_location: Point<i32, Logical>,
    /// Pending drop layout and the output it applies to.
    drop_target: Option<(Layout, Output)>,
}

impl MoveGrab {
    /// Creates a move grab and changes the cursor to the grabbing icon.
    pub fn new(
        state: &mut Orbit,
        start_data: GrabStartData<Orbit>,
        window: Window,
        initial_window_location: Point<i32, Logical>,
    ) -> Self {
        state.input.cursor_status = CursorImageStatus::Named(CursorIcon::Grabbing);
        Self {
            start_data,
            window,
            initial_window_location,
            drop_target: None,
        }
    }
}

impl PointerGrab<Orbit> for MoveGrab {
    /// Moves the window with the pointer and updates snapping/drop previews.
    fn motion(
        &mut self,
        data: &mut Orbit,
        handle: &mut PointerInnerHandle<'_, Orbit>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // No client gets pointer focus while a window is being dragged.
        handle.motion(data, None, event);

        // Gone, or its workspace was switched away from mid-drag.
        if !self.window.alive() || data.space.element_location(&self.window).is_none() {
            handle.unset_grab(self, data, event.serial, event.time, true);
            return;
        }

        data.queue_redraw_window(&self.window);
        let delta = event.location - self.start_data.location;
        let mut location = (self.initial_window_location.to_f64() + delta).to_i32_round();

        // Snapping follows the pointer from output to output.
        let output = data
            .space
            .output_under(event.location)
            .next()
            .cloned()
            .or_else(|| data.output_under_window(&self.window));

        if let Some(output) = output
            && let Some(output_geo) = data.space.output_geometry(&output)
        {
            // Dragged onto another output, it joins what that one shows.
            data.assign_window_to_output(&self.window, &output);
            let frame = frame_at(&self.window, location);
            let others = data
                .space
                .elements()
                .filter(|w| **w != self.window)
                .filter_map(|w| data.frame_geometry(w))
                .map(ToCore::to_core);
            location += snap::magnetize(frame.to_core(), output_geo.to_core(), others).to_smithay();
            self.drop_target = snap::drop_zone(event.location.to_core(), output_geo.to_core())
                .map(|layout| (layout, output));
            data.overlays.snap_preview = self
                .drop_target
                .as_ref()
                .and_then(|(layout, output)| data.layout_area_on(output, &self.window, *layout));
        }

        data.space.map_element(self.window.clone(), location, true);
        data.queue_redraw_window(&self.window);
    }

    /// Forwards relative pointer motion without changing the window position.
    fn relative_motion(
        &mut self,
        data: &mut Orbit,
        handle: &mut PointerInnerHandle<'_, Orbit>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    /// Forwards button events and ends the grab after the last button release.
    fn button(
        &mut self,
        data: &mut Orbit,
        handle: &mut PointerInnerHandle<'_, Orbit>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    /// Forwards pointer-axis events to the active grab.
    fn axis(
        &mut self,
        data: &mut Orbit,
        handle: &mut PointerInnerHandle<'_, Orbit>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    /// Forwards the pointer frame event.
    fn frame(&mut self, data: &mut Orbit, handle: &mut PointerInnerHandle<'_, Orbit>) {
        handle.frame(data);
    }

    super::forward_gestures!();

    /// Returns the pointer state captured when the grab started.
    fn start_data(&self) -> &GrabStartData<Orbit> {
        &self.start_data
    }

    /// Restores the cursor, removes the preview, and applies the drop layout.
    fn unset(&mut self, data: &mut Orbit) {
        data.input.cursor_status = CursorImageStatus::default_named();
        data.overlays.snap_preview = None;
        if let Some((layout, output)) = self.drop_target.take()
            && self.window.alive()
            && data.space.element_location(&self.window).is_some()
        {
            data.set_layout_on(&self.window, layout, &output);
        }
    }
}
