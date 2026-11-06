mod move_grab;
mod resize_grab;

use smithay::{
    desktop::Window,
    input::pointer::{Focus, GrabStartData},
    utils::{Point, Rectangle, Serial},
};

use crate::{
    state::Orbit,
    window::{self, Layout},
};
use move_grab::MoveGrab;
use resize_grab::ResizeGrab;

pub use resize_grab::{Edges, handle_resize_commit};

impl Orbit {
    /// Starts a pointer grab that moves `window`.
    ///
    /// Maximized and tiled windows are restored to the floating layout before
    /// the grab begins. Their position is adjusted so the pointer remains at
    /// the same relative horizontal position instead of jumping to an edge.
    ///
    /// The request is ignored for fullscreen windows or windows that are not
    /// currently mapped in the compositor's space.
    pub fn start_move(&mut self, window: Window, start_data: GrabStartData<Orbit>, serial: Serial) {
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };
        if window::is_fullscreen(&window) {
            return;
        }
        let Some(mut location) = self.space.element_location(&window) else {
            return;
        };

        if Self::layout(&window) != Layout::Floating {
            let pointer = start_data.location;
            let laid_out_width = window.geometry().size.w.max(1) as f64;
            let ratio = ((pointer.x - location.x as f64) / laid_out_width).clamp(0.0, 1.0);

            self.set_layout(&window, Layout::Floating);
            let restored_width = toplevel
                .with_pending_state(|s| s.size)
                .map_or(laid_out_width, |size| size.w as f64);
            location = Point::from((
                (pointer.x - ratio * restored_width).round() as i32,
                location.y,
            ));
            self.space.map_element(window.clone(), location, true);
        }

        let grab = MoveGrab::new(self, start_data, window, location);
        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    /// Starts a pointer grab that resizes `window` from `edges`.
    ///
    /// Maximized and fullscreen windows are not resizable through this grab.
    /// A tiled window is first restored to its current position in the
    /// floating layout, then resized from that position.
    ///
    /// The request is ignored when the window is not currently mapped in the
    /// compositor's space.
    pub fn start_resize(
        &mut self,
        window: Window,
        start_data: GrabStartData<Orbit>,
        edges: Edges,
        serial: Serial,
    ) {
        if window::is_fullscreen(&window) || Self::layout(&window) == Layout::Maximized {
            return;
        }

        self.untile_in_place(&window);

        let Some(location) = self.space.element_location(&window) else {
            return;
        };

        let initial_rect = Rectangle::new(location, window.geometry().size);
        let grab = ResizeGrab::new(self, start_data, window, edges, initial_rect);
        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }
}

/// Forwards every gesture callback of a [`PointerGrab`] straight to the
/// client. Window grabs only care about motion and buttons.
macro_rules! forward_gestures {
    () => {
        fn gesture_swipe_begin(
            &mut self,
            data: &mut Orbit,
            handle: &mut PointerInnerHandle<'_, Orbit>,
            event: &GestureSwipeBeginEvent,
        ) {
            handle.gesture_swipe_begin(data, event)
        }

        fn gesture_swipe_update(
            &mut self,
            data: &mut Orbit,
            handle: &mut PointerInnerHandle<'_, Orbit>,
            event: &GestureSwipeUpdateEvent,
        ) {
            handle.gesture_swipe_update(data, event)
        }

        fn gesture_swipe_end(
            &mut self,
            data: &mut Orbit,
            handle: &mut PointerInnerHandle<'_, Orbit>,
            event: &GestureSwipeEndEvent,
        ) {
            handle.gesture_swipe_end(data, event)
        }

        fn gesture_pinch_begin(
            &mut self,
            data: &mut Orbit,
            handle: &mut PointerInnerHandle<'_, Orbit>,
            event: &GesturePinchBeginEvent,
        ) {
            handle.gesture_pinch_begin(data, event)
        }

        fn gesture_pinch_update(
            &mut self,
            data: &mut Orbit,
            handle: &mut PointerInnerHandle<'_, Orbit>,
            event: &GesturePinchUpdateEvent,
        ) {
            handle.gesture_pinch_update(data, event)
        }

        fn gesture_pinch_end(
            &mut self,
            data: &mut Orbit,
            handle: &mut PointerInnerHandle<'_, Orbit>,
            event: &GesturePinchEndEvent,
        ) {
            handle.gesture_pinch_end(data, event)
        }

        fn gesture_hold_begin(
            &mut self,
            data: &mut Orbit,
            handle: &mut PointerInnerHandle<'_, Orbit>,
            event: &GestureHoldBeginEvent,
        ) {
            handle.gesture_hold_begin(data, event)
        }

        fn gesture_hold_end(
            &mut self,
            data: &mut Orbit,
            handle: &mut PointerInnerHandle<'_, Orbit>,
            event: &GestureHoldEndEvent,
        ) {
            handle.gesture_hold_end(data, event)
        }
    };
}

use forward_gestures;
