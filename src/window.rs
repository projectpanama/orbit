pub mod decorations;
pub mod grabs;

use std::{cell::RefCell, sync::Arc, time::Duration};

use orbit_core::{
    animation::{Animation, Timing},
    ids::WindowId,
};
use smithay::{
    desktop::Window,
    output::Output,
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel::State as XdgState,
        },
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{
        compositor::with_states,
        shell::xdg::{SurfaceCachedState, ToplevelSurface},
    },
};

use crate::{
    geometry::{ToCore, ToSmithay},
    state::Orbit,
    window::decorations::{Decoration, DecorationStyle},
};

pub use orbit_core::layout::{Layout, Side};

/// How small a window starts when it opens relative to its size.
const OPEN_START_SCALE: f64 = 0.92;

/// State of a window's open animation.
#[derive(Debug, Clone, Copy, Default)]
pub enum OpenAnimation {
    /// No open animation is active.
    #[default]
    None,
    /// The animation will start on the next sample.
    Waiting(Timing),
    /// The animation is currently running.
    Running(Animation),
}

impl OpenAnimation {
    /// Samples the animation, returning opacity and scale.
    pub fn sample(&mut self, now: Duration) -> (f32, f64) {
        let progress = match *self {
            OpenAnimation::None => return (1.0, 1.0),
            OpenAnimation::Waiting(timing) => {
                let animation = Animation::new(0.0, 1.0, now, timing);
                *self = OpenAnimation::Running(animation);
                animation.value(now)
            }
            OpenAnimation::Running(animation) => animation.value(now),
        };
        (
            progress as f32,
            OPEN_START_SCALE + (1.0 - OPEN_START_SCALE) * progress,
        )
    }

    /// Reports whether the animation is still running at `now`.
    pub fn is_running(&self, now: Duration) -> bool {
        matches!(self, OpenAnimation::Running(animation) if !animation.is_done(now))
    }

    /// Clears the animation after it reaches its end time.
    pub fn tick(&mut self, now: Duration) {
        if let OpenAnimation::Running(animation) = self
            && animation.is_done(now)
        {
            *self = OpenAnimation::None;
        }
    }
}

/// Per-window state stored in Smithay's window user data.
#[derive(Debug, Default)]
pub struct WindowState {
    /// Set when the window is registered.
    pub id: Option<WindowId>,
    /// While on a hidden workspace, where it was relative to its output's
    /// top-left corner, and when it was hidden (for stacking order).
    pub hidden_at: Option<(Point<i32, Logical>, u64)>,
    /// The client asked for attention without being allowed to take focus.
    pub urgent: bool,
    /// Buffers for server-side decorations, created on first use.
    pub decoration: Decoration,
    /// How it's title bar looks like (set when the window is created).
    pub style: Arc<DecorationStyle>,
    /// Current open animation state.
    pub open_animation: OpenAnimation,
    /// Saved content position and state generation while minimized.
    minimized: Option<(Point<i32, Logical>, u64)>,
    /// The side to which the window is tiled, if it is tiled.
    tiled: Option<Side>,
}

impl Orbit {
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        let id = self.window_surfaces.get(surface)?;
        self.windows.get(id).cloned()
    }

    pub fn focused_window(&self) -> Option<Window> {
        let focus = self.seat.get_keyboard()?.current_focus()?;
        self.window_for_surface(&focus)
    }

    pub fn content_geometry(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        let location = self.space.element_location(window)?;
        Some(Rectangle::new(location, window.geometry().size))
    }

    /// Returns the outer frame geometry for a mapped window.
    pub fn frame_geometry(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        Some(frame_at(window, self.space.element_location(window)?))
    }

    pub fn layout(window: &Window) -> Layout {
        let Some(toplevel) = window.toplevel() else {
            return Layout::Floating;
        };
        if has_state(toplevel, XdgState::Maximized) {
            return Layout::Maximized;
        }
        match with_window_state(window, |state| state.tiled) {
            Some(side) => Layout::Tiled(side),
            None => Layout::Floating,
        }
    }

    /// The frame area `layout` gives `window` on its output.
    pub fn layout_area(&self, window: &Window, layout: Layout) -> Option<Rectangle<i32, Logical>> {
        let output = self.window_output(window)?;
        self.layout_area_on(&output, window, layout)
    }

    /// The frame area `layout` would give `window` on `output`.
    pub fn layout_area_on(
        &self,
        output: &Output,
        window: &Window,
        layout: Layout,
    ) -> Option<Rectangle<i32, Logical>> {
        // Fullscreen covers the whole output and everything else stays clear
        // of the space panels reserve.
        let output = if is_fullscreen(window) {
            self.space.output_geometry(output)?
        } else {
            self.usable_geometry(output)?
        };
        layout.area(output.to_core()).map(ToSmithay::to_smithay)
    }
}

/// Reports whether the window has saved minimized state.
pub fn is_minimized(window: &Window) -> bool {
    with_window_state(window, |state| state.minimized.is_some())
}

/// Reports whether the window's pending state marks it fullscreen.
pub fn is_fullscreen(window: &Window) -> bool {
    window
        .toplevel()
        .is_some_and(|t| has_state(t, XdgState::Fullscreen))
}

/// Builds frame geometry from a window's content location.
pub fn frame_at(window: &Window, content_loc: Point<i32, Logical>) -> Rectangle<i32, Logical> {
    frame_around(window, Rectangle::new(content_loc, window.geometry().size))
}

/// Expands content geometry to include the server-side decoration area.
pub fn frame_around(window: &Window, content: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let deco = decoration_height(window);
    Rectangle::new(
        (content.loc.x, content.loc.y - deco).into(),
        (content.size.w, content.size.h + deco).into(),
    )
}

/// Converts a frame position into the corresponding content position.
pub fn content_loc(window: &Window, frame_loc: Point<i32, Logical>) -> Point<i32, Logical> {
    frame_loc + Point::from((0, decoration_height(window)))
}

/// Returns the height reserved for a window's server-side decorations.
pub fn decoration_height(window: &Window) -> i32 {
    if uses_server_side_decorations(window) {
        // TODO: This should come from a config, but we have not yet
        // implemented it yet so we just hardcode a value for now.
        32
    } else {
        0
    }
}

/// Sends the pending toplevel configure when the initial configure exists.
pub fn configure(toplevel: &ToplevelSurface) {
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}

/// Clamps a requested size to the surface's pending min/max size constraints.
pub fn clamp_size(toplevel: &ToplevelSurface, size: Size<i32, Logical>) -> Size<i32, Logical> {
    let (min, max) = with_states(toplevel.wl_surface(), |states| {
        let mut cached = states.cached_state.get::<SurfaceCachedState>();
        let current = cached.current();
        (current.min_size, current.max_size)
    });
    let clamp = |value: i32, min: i32, max: i32| {
        let value = value.max(min.max(1));
        if max > 0 { value.min(max) } else { value }
    };
    Size::from((clamp(size.w, min.w, max.w), clamp(size.h, min.h, max.h)))
}

/// Reports whether the window requests server-side decorations.
///
/// Fullscreen windows do not reserve decoration space even when their pending
/// decoration mode requests server-side decorations.
pub fn uses_server_side_decorations(window: &Window) -> bool {
    window.toplevel().is_some_and(|t| {
        t.with_pending_state(|pending| {
            pending.decoration_mode == Some(DecorationMode::ServerSide)
                && !pending.states.contains(XdgState::Fullscreen)
        })
    })
}

/// Provides mutable access to the state stored for `window`.
pub fn with_window_state<T>(window: &Window, f: impl FnOnce(&mut WindowState) -> T) -> T {
    let user_data = window.user_data();
    user_data.insert_if_missing(|| RefCell::new(WindowState::default()));
    f(&mut user_data
        .get::<RefCell<WindowState>>()
        .expect("inserted just above")
        .borrow_mut())
}

/// Reports whether a pending toplevel state contains `state`.
fn has_state(toplevel: &ToplevelSurface, state: XdgState) -> bool {
    toplevel.with_pending_state(|pending| pending.states.contains(state))
}
