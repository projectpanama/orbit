pub mod arrange;
pub mod decorations;
pub mod grabs;

use std::{cell::RefCell, sync::Arc, time::Duration};

use orbit_core::{
    animation::{Animation, Timing},
    ids::WindowId,
    layout,
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
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::{
        compositor::with_states,
        shell::xdg::{SurfaceCachedState, ToplevelSurface},
    },
};

use crate::{
    geometry::{ToCore, ToSmithay},
    state::Orbit,
    window::decorations::{Decoration, DecorationStyle},
    workspaces::{WorkspaceTarget, window_id},
};

pub use orbit_core::layout::{Layout, Side};

/// How small a window starts when it opens relative to its size.
const OPEN_START_SCALE: f64 = 0.92;

/// How far keyboard move/resize steps go.
const KEYBOARD_STEP: i32 = 32;

const TILED_STATES: [XdgState; 4] = [
    XdgState::TiledLeft,
    XdgState::TiledRight,
    XdgState::TiledTop,
    XdgState::TiledBottom,
];

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
    /// Where the window was while floating, saved when it leaves the
    /// floating layout so it can return there.
    floating_geometry: Option<Rectangle<i32, Logical>>,
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

    /// Applies a floating, tiled, or maximized layout to a window.
    ///
    /// The pending xdg surface state and compositor geometry are updated
    /// together. Floating geometry is saved before another layout replaces
    /// it and restored when the window returns to the floating layout.
    pub fn set_layout(&mut self, window: &Window, layout: Layout) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if Self::layout(window) == layout {
            return;
        }
        self.save_floating_geometry(window);

        toplevel.with_pending_state(|pending| {
            pending.states.unset(XdgState::Maximized);
            for state in TILED_STATES {
                pending.states.unset(state);
            }
            match layout {
                Layout::Floating => {}
                Layout::Maximized => {
                    pending.states.set(XdgState::Maximized);
                }
                Layout::Tiled(side) => {
                    pending.states.set(XdgState::TiledTop);
                    pending.states.set(XdgState::TiledBottom);
                    pending.states.set(match side {
                        Side::Left => XdgState::TiledLeft,
                        Side::Right => XdgState::TiledRight,
                    });
                }
            }
        });
        with_window_state(window, |state| {
            state.tiled = match layout {
                Layout::Tiled(side) => Some(side),
                _ => None,
            };
        });
        self.toplevel_states_changed();

        // A fullscreen window keeps covering the output.
        // The new layout takes effect when it leaves fullscreen.
        if !is_fullscreen(window) {
            if layout == Layout::Floating {
                self.restore_floating_geometry(window, toplevel);
            } else {
                self.arrange_window(window);
            }
        }
        configure(toplevel);
    }

    /// Applies a layout after moving a window to an output.
    ///
    /// Moving first ensures that a window returning to the floating layout
    /// uses the requested output.
    pub fn set_layout_on(&mut self, window: &Window, layout: Layout, output: &Output) {
        // Move first so a window leaving the floating layout returns to the
        // requested output.
        self.move_to_output(window, output);
        self.set_layout(window, layout);
    }

    /// Removes tiled state without changing the window position.
    pub fn untile_in_place(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if !matches!(Self::layout(window), Layout::Tiled(_)) {
            return;
        }
        toplevel.with_pending_state(|pending| {
            for state in TILED_STATES {
                pending.states.unset(state);
            }
        });
        with_window_state(window, |state| {
            state.tiled = None;
            state.floating_geometry = None;
        });
        configure(toplevel);
    }

    /// Maximizes a window on its current output.
    pub fn maximize(&mut self, window: &Window) {
        self.set_layout(window, Layout::Maximized);
    }

    /// Restores a maximized window to the floating layout.
    pub fn unmaximize(&mut self, window: &Window) {
        if Self::layout(window) == Layout::Maximized {
            self.set_layout(window, Layout::Floating);
        }
    }

    /// Toggles the maximized state of a window.
    pub fn toggle_maximize(&mut self, window: &Window) {
        if Self::layout(window) == Layout::Maximized {
            self.set_layout(window, Layout::Floating);
        } else {
            self.set_layout(window, Layout::Maximized);
        }
    }

    /// Toggles tiling on the requested side.
    pub fn toggle_tile(&mut self, window: &Window, side: Side) {
        if Self::layout(window) == Layout::Tiled(side) {
            self.set_layout(window, Layout::Floating);
        } else {
            self.set_layout(window, Layout::Tiled(side));
        }
    }

    /// Enters fullscreen mode for a window.
    pub fn fullscreen(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        self.save_floating_geometry(window);
        toplevel.with_pending_state(|pending| pending.states.set(XdgState::Fullscreen));
        self.toplevel_states_changed();
        self.arrange_window(window);
        self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
        configure(toplevel);
    }

    /// Leaves fullscreen mode and restores the previous window layout.
    pub fn unfullscreen(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        toplevel.with_pending_state(|pending| pending.states.unset(XdgState::Fullscreen));
        self.toplevel_states_changed();
        if Self::layout(window) == Layout::Floating {
            self.restore_floating_geometry(window, toplevel);
        } else {
            self.arrange_window(window);
        }
        configure(toplevel);
    }

    /// Toggles fullscreen mode for a window.
    pub fn toggle_fullscreen(&mut self, window: &Window) {
        if is_fullscreen(window) {
            self.unfullscreen(window);
        } else {
            self.fullscreen(window);
        }
    }

    /// Hides a window while saving its mapped position and focus order.
    pub fn minimize(&mut self, window: &Window) {
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        self.hide_stamp += 1;
        let stamp = self.hide_stamp;
        with_window_state(window, |state| state.minimized = Some((location, stamp)));
        self.space.unmap_elem(window);
        self.toplevel_states_changed();

        match self
            .window_output(window)
            .and_then(|output| self.output_id(&output))
        {
            Some(output) => self.refocus_output(output),
            None => self.focus_window(None, SERIAL_COUNTER.next_serial()),
        }
    }

    /// Restores a minimized window to its saved position.
    ///
    /// Returns false when the window was not minimized.
    pub fn unminimize(&mut self, window: &Window) -> bool {
        let Some((location, _)) = with_window_state(window, |state| state.minimized.take()) else {
            return false;
        };
        self.toplevel_states_changed();
        self.space.map_element(window.clone(), location, true);
        // Its workspace may have moved while it was hidden.
        // Its output may also have changed.
        if let Some(output) = self.window_output(window) {
            self.move_to_output(window, &output);
        } else {
            self.ensure_on_output(window);
        }
        self.arrange_window(window);
        true
    }

    /// Restores and activates the most recently minimized window.
    pub fn restore_minimized(&mut self) {
        let last = self
            .windows
            .values()
            .filter_map(|window| {
                let (_, stamp) = with_window_state(window, |state| state.minimized)?;
                Some((stamp, window))
            })
            .max_by_key(|&(stamp, _)| stamp)
            .map(|(_, window)| window.clone());
        if let Some(window) = last {
            self.activate_window(&window);
        }
    }

    /// Shows the workspace containing a window, restores it, raises it, and
    /// focuses it.
    pub fn activate_window(&mut self, window: &Window) {
        if let Some(id) = window_id(window)
            && !self.workspaces.is_visible(id)
            && let Some((_, index)) = self.workspaces.workspace_of(id)
            && let Some(output) = self.window_output(window)
        {
            self.switch_workspace(&output, WorkspaceTarget::Index(index));
        }
        self.unminimize(window);
        self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
    }

    /// Marks an unfocused window as needing attention.
    pub fn mark_urgent(&mut self, window: &Window) {
        if self.focused_window().as_ref() != Some(window) {
            with_window_state(window, |state| state.urgent = true);
        }
    }

    /// Requests that the client close a window.
    pub fn close(window: &Window) {
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        }
    }

    /// Focuses the bottom window shown by the focused output.
    ///
    /// Repeating the action cycles through shown windows because focusing
    /// raises the selected window.
    pub fn focus_next(&mut self) {
        let Some(output) = self
            .focused_output()
            .and_then(|output| self.output_id(&output))
        else {
            return;
        };
        let on_output = self.workspaces.active_windows(output);
        let shown: Vec<Window> = self
            .space
            .elements()
            .filter(|window| window_id(window).is_some_and(|id| on_output.contains(&id)))
            .take(2)
            .cloned()
            .collect();
        if let [next, _] = shown.as_slice() {
            let next = next.clone();
            self.focus_window(Some(&next), SERIAL_COUNTER.next_serial());
        }
    }

    /// Moves a window by keyboard steps.
    ///
    /// Maximized and tiled windows return to the floating layout first.
    pub fn move_by(&mut self, window: &Window, dx: i32, dy: i32) {
        if is_fullscreen(window) {
            return;
        }
        self.set_layout(window, Layout::Floating);
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        self.space.map_element(
            window.clone(),
            Point::from((
                location.x.saturating_add(dx.saturating_mul(KEYBOARD_STEP)),
                location.y.saturating_add(dy.saturating_mul(KEYBOARD_STEP)),
            )),
            false,
        );
        // A move onto another output follows that output's workspace.
        self.sync_window_output(window);
    }

    /// Resizes a window by keyboard steps from its bottom right corner.
    ///
    /// Maximized and tiled windows return to the floating layout first.
    pub fn resize_by(&mut self, window: &Window, dw: i32, dh: i32) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if is_fullscreen(window) {
            return;
        }
        match Self::layout(window) {
            Layout::Maximized => self.set_layout(window, Layout::Floating),
            Layout::Tiled(_) => self.untile_in_place(window),
            Layout::Floating => {}
        }
        let base = toplevel
            .with_pending_state(|pending| pending.size)
            .unwrap_or_else(|| window.geometry().size);
        // The base may be a client supplied size with arbitrary dimensions.
        let size = clamp_size(
            toplevel,
            (
                base.w.saturating_add(dw.saturating_mul(KEYBOARD_STEP)),
                base.h.saturating_add(dh.saturating_mul(KEYBOARD_STEP)),
            )
                .into(),
        );
        toplevel.with_pending_state(|pending| pending.size = Some(size));
        configure(toplevel);
    }

    fn save_floating_geometry(&self, window: &Window) {
        if Self::layout(window) != Layout::Floating || is_fullscreen(window) {
            return;
        }
        let Some(geometry) = self.content_geometry(window) else {
            return;
        };
        with_window_state(window, |state| state.floating_geometry = Some(geometry));
    }

    fn restore_floating_geometry(&mut self, window: &Window, toplevel: &ToplevelSurface) {
        let geometry = with_window_state(window, |state| state.floating_geometry.take());
        toplevel.with_pending_state(|pending| {
            // An empty size lets the client pick, e.g. when it was maximized
            // before ever drawing a floating frame.
            pending.size = geometry.map(|g| g.size).filter(|s| !s.is_empty());
        });
        let Some(geometry) = geometry else {
            return;
        };
        // Not shown: it floats where it was hidden once it is.
        if self.space.element_location(window).is_none() {
            return;
        }
        let mut location = geometry.loc;
        let frame = frame_around(window, geometry);
        let saved_output = self.output_for_rect(frame);
        if let Some(current) = self.window_output(window)
            && saved_output.as_ref() != Some(&current)
            && let Some(to) = self.space.output_geometry(&current)
        {
            let from = saved_output.and_then(|output| self.space.output_geometry(&output));
            let fitted =
                layout::output::fit_rect(frame.to_core(), from.map(ToCore::to_core), to.to_core());
            location = content_loc(window, fitted.to_smithay());
        }
        self.space.map_element(window.clone(), location, false);
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
        with_window_state(window, |state| state.style.height())
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
