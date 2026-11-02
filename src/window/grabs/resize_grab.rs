use std::cell::RefCell;

use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, GestureHoldBeginEvent,
        GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
        GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData,
        MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::{self, ResizeEdge},
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{IsAlive, Logical, Point, Rectangle, Size},
    wayland::compositor::with_states,
};

use crate::{geometry::RectExt, state::Orbit};

bitflags::bitflags! {
    /// Window edges that follow the pointer during a resize.
    ///
    /// Values match xdg-shell `resize_edge`; corners combine two sides.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Edges: u32 {
        const TOP = 1;
        const BOTTOM = 2;
        const LEFT = 4;
        const RIGHT = 8;
    }
}

impl Edges {
    /// Converts an xdg-shell resize edge to Orbit's edge flags.
    pub fn from_xdg(edge: ResizeEdge) -> Self {
        Self::from_bits_truncate(edge.into())
    }

    /// Selects the two edges nearest to `pos` within `rect`.
    ///
    /// This is used for compositor-initiated Mod+drag resizes without a
    /// client-specified edge.
    pub fn nearest(rect: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> Self {
        let center = rect.center();
        let horizontal = if pos.x < center.x {
            Self::LEFT
        } else {
            Self::RIGHT
        };
        let vertical = if pos.y < center.y {
            Self::TOP
        } else {
            Self::BOTTOM
        };
        horizontal | vertical
    }

    /// Returns the cursor icon appropriate for these edges.
    ///
    /// Corner combinations take precedence over single-edge flags.
    pub fn cursor_icon(self) -> CursorIcon {
        const TOP_LEFT: Edges = Edges::TOP.union(Edges::LEFT);
        const TOP_RIGHT: Edges = Edges::TOP.union(Edges::RIGHT);
        const BOTTOM_LEFT: Edges = Edges::BOTTOM.union(Edges::LEFT);
        const BOTTOM_RIGHT: Edges = Edges::BOTTOM.union(Edges::RIGHT);
        match self {
            e if e.contains(TOP_LEFT) => CursorIcon::NwResize,
            e if e.contains(TOP_RIGHT) => CursorIcon::NeResize,
            e if e.contains(BOTTOM_LEFT) => CursorIcon::SwResize,
            e if e.contains(BOTTOM_RIGHT) => CursorIcon::SeResize,
            e if e.contains(Self::TOP) => CursorIcon::NResize,
            e if e.contains(Self::BOTTOM) => CursorIcon::SResize,
            e if e.contains(Self::LEFT) => CursorIcon::WResize,
            _ => CursorIcon::EResize,
        }
    }
}

/// Per-surface resize progress stored in the surface data map.
#[derive(Debug, Clone, Copy, Default)]
enum ResizeSurfaceState {
    #[default]
    /// No resize is in progress.
    Idle,
    /// A grab is active and the client is receiving resize configures.
    Resizing {
        /// Edges being dragged.
        edges: Edges,
        /// Frame geometry when the grab started.
        initial_rect: Rectangle<i32, Logical>,
    },
    /// The grab ended; the next client commit records the final size.
    WaitingForLastCommit {
        /// Edges that were dragged.
        edges: Edges,
        /// Frame geometry when the grab started.
        initial_rect: Rectangle<i32, Logical>,
    },
}

/// Pointer grab that resizes a window from the selected edges.
pub struct ResizeGrab {
    /// Pointer state captured when the grab started.
    start_data: GrabStartData<Orbit>,
    /// Window being resized.
    window: Window,
    /// Edges following the pointer.
    edges: Edges,
    /// Frame geometry at the start of the grab.
    initial_rect: Rectangle<i32, Logical>,
    /// Most recently configured client size.
    last_size: Size<i32, Logical>,
}

impl ResizeGrab {
    /// Starts a resize grab and marks the surface as resizing.
    pub fn new(
        state: &mut Orbit,
        start_data: GrabStartData<Orbit>,
        window: Window,
        edges: Edges,
        initial_rect: Rectangle<i32, Logical>,
    ) -> Self {
        if let Some(toplevel) = window.toplevel() {
            with_resize_state(toplevel.wl_surface(), |state| {
                *state = ResizeSurfaceState::Resizing {
                    edges,
                    initial_rect,
                };
            });
        }

        state.input.cursor_status = CursorImageStatus::Named(edges.cursor_icon());

        Self {
            start_data,
            window,
            edges,
            initial_rect,
            last_size: initial_rect.size,
        }
    }
}

impl PointerGrab<Orbit> for ResizeGrab {
    /// Updates the pending size from pointer movement and sends a configure.
    fn motion(
        &mut self,
        data: &mut Orbit,
        handle: &mut PointerInnerHandle<'_, Orbit>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);

        if !self.window.alive() {
            handle.unset_grab(self, data, event.serial, event.time, true);
            return;
        }
        let Some(toplevel) = self.window.toplevel() else {
            return;
        };

        let delta = (event.location - self.start_data.location).to_i32_round::<i32>();
        let mut width = self.initial_rect.size.w;
        let mut height = self.initial_rect.size.h;

        if self.edges.contains(Edges::LEFT) {
            width -= delta.x;
        } else if self.edges.contains(Edges::RIGHT) {
            width += delta.x;
        }
        if self.edges.contains(Edges::TOP) {
            height -= delta.y;
        } else if self.edges.contains(Edges::BOTTOM) {
            height += delta.y;
        }

        self.last_size = crate::window::clamp_size(toplevel, Size::from((width, height)));

        toplevel.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
            state.size = Some(self.last_size);
        });
        toplevel.send_pending_configure();
    }

    /// Forwards relative pointer motion to the default grab handling.
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

    /// Forwards pointer-axis events.
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

    /// Returns the pointer state captured at grab start.
    fn start_data(&self) -> &GrabStartData<Orbit> {
        &self.start_data
    }

    /// Ends the grab, sends the final configure, and waits for its commit.
    fn unset(&mut self, data: &mut Orbit) {
        data.input.cursor_status = CursorImageStatus::default_named();

        if !self.window.alive() {
            return;
        }
        let Some(toplevel) = self.window.toplevel() else {
            return;
        };
        toplevel.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Resizing);
            state.size = Some(self.last_size);
        });
        toplevel.send_pending_configure();

        with_resize_state(toplevel.wl_surface(), |state| {
            *state = ResizeSurfaceState::WaitingForLastCommit {
                edges: self.edges,
                initial_rect: self.initial_rect,
            };
        });
    }
}

/// Applies the final committed resize geometry to the window.
///
/// Top- and left-edge resizes move the window after the client commits, keeping
/// the opposite edge fixed. Other edge combinations require no repositioning.
pub fn handle_resize_commit(state: &mut Orbit, surface: &WlSurface) {
    let Some(window) = state.window_for_surface(surface) else {
        return;
    };

    let resize = with_resize_state(surface, |resize| {
        let current = *resize;
        if let ResizeSurfaceState::WaitingForLastCommit { .. } = current {
            *resize = ResizeSurfaceState::Idle;
        }
        current
    });

    let (edges, initial_rect) = match resize {
        ResizeSurfaceState::Resizing {
            edges,
            initial_rect,
        }
        | ResizeSurfaceState::WaitingForLastCommit {
            edges,
            initial_rect,
        } => (edges, initial_rect),
        ResizeSurfaceState::Idle => return,
    };
    if !edges.intersects(Edges::TOP | Edges::LEFT) {
        return;
    }

    let size = window.geometry().size;
    let Some(mut location) = state.space.element_location(&window) else {
        return;
    };
    if edges.contains(Edges::LEFT) {
        location.x = initial_rect.loc.x + (initial_rect.size.w - size.w);
    }
    if edges.contains(Edges::TOP) {
        location.y = initial_rect.loc.y + (initial_rect.size.h - size.h);
    }
    state.space.map_element(window, location, false);
}

/// Mutates the resize state stored for `surface`, creating it if needed.
fn with_resize_state<T>(surface: &WlSurface, f: impl FnOnce(&mut ResizeSurfaceState) -> T) -> T {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing(|| RefCell::new(ResizeSurfaceState::default()));
        let mut state = states
            .data_map
            .get::<RefCell<ResizeSurfaceState>>()
            .expect("inserted just above")
            .borrow_mut();
        f(&mut state)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_match_xdg_resize_edge() {
        let cases = [
            (ResizeEdge::None, Edges::empty()),
            (ResizeEdge::Top, Edges::TOP),
            (ResizeEdge::Bottom, Edges::BOTTOM),
            (ResizeEdge::Left, Edges::LEFT),
            (ResizeEdge::Right, Edges::RIGHT),
            (ResizeEdge::TopLeft, Edges::TOP | Edges::LEFT),
            (ResizeEdge::TopRight, Edges::TOP | Edges::RIGHT),
            (ResizeEdge::BottomLeft, Edges::BOTTOM | Edges::LEFT),
            (ResizeEdge::BottomRight, Edges::BOTTOM | Edges::RIGHT),
        ];
        for (edge, expected) in cases {
            assert_eq!(Edges::from_xdg(edge), expected, "{edge:?}");
        }
    }

    #[test]
    fn corner_cursors_win_over_sides() {
        assert_eq!(
            (Edges::BOTTOM | Edges::RIGHT).cursor_icon(),
            CursorIcon::SeResize
        );
        assert_eq!(Edges::LEFT.cursor_icon(), CursorIcon::WResize);
    }
}
