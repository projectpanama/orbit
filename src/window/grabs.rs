mod resize_grab;

pub use resize_grab::{Edges, handle_resize_commit};

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
