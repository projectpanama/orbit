use orbit_core::ids::WorkspaceId;
use smithay::{
    desktop::Window,
    output::Output,
    utils::{Logical, Rectangle, Size},
    wayland::shell::xdg::ToplevelSurface,
};

use crate::state::Orbit;

impl Orbit {
    /// Applies the geometry requested by `window`'s layout state.
    ///
    /// Fullscreen windows cover their output, maximized and snapped windows
    /// fill their usable area, and floating windows keep their position.
    ///
    /// Hidden or minimized windows receive only a size until they are shown.
    ///
    /// The client is not configured by this method.
    pub fn arrange_window(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let layout = if super::is_fullscreen(window) {
            super::Layout::Maximized
        } else {
            Self::layout(window)
        };
        if let Some(area) = self.layout_area(window, layout) {
            self.place_in_frame(window, toplevel, area);
        }
    }

    /// Arranges `window` and sends a pending configure when its layout
    /// determines its geometry and it is mapped.
    pub fn arrange_and_configure(&mut self, window: &Window) {
        if !is_laid_out(window) {
            return;
        }
        self.arrange_window(window);
        if let Some(toplevel) = window.toplevel() {
            super::configure(toplevel);
        }
    }

    /// Arranges every window on `workspace`, shown or not.
    pub fn arrange(&mut self, workspace: WorkspaceId) {
        let windows: Vec<Window> = self
            .workspaces
            .windows_of(workspace)
            .iter()
            .filter_map(|id| self.windows.get(id).cloned())
            .collect();
        for window in &windows {
            self.arrange_and_configure(window);
        }
    }

    /// Arranges every workspace of `output`, e.g. after its panels
    /// reserved a different area or it changed its size.
    pub fn arrange_output(&mut self, output: &Output) {
        let Some(id) = self.output_id(output) else {
            return;
        };
        let workspaces: Vec<WorkspaceId> = self.workspaces.workspace_ids(id).collect();
        for workspace in workspaces {
            self.arrange(workspace);
        }
    }

    /// Arranges every workspace on every output, e.g. after the title-bar
    /// height changed or a panel was added or removed.
    pub fn arrange_all(&mut self) {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in &outputs {
            self.arrange_output(output);
        }
    }

    /// Lays out panels, lock surfaces and windows again after outputs
    /// changed size, moved or went away.
    pub fn arrange_after_output_change(&mut self) {
        self.arrange_layers();
        self.reconfigure_lock_surfaces();
        self.arrange_all();
    }

    /// Applies a frame rectangle to the toplevel's pending state and space.
    ///
    /// The client receives the content size below the decoration area. Hidden
    /// and minimized windows are resized but remain unmapped.
    fn place_in_frame(
        &mut self,
        window: &Window,
        toplevel: &ToplevelSurface,
        frame: Rectangle<i32, Logical>,
    ) {
        let deco = super::decoration_height(window);
        let size = Size::from((frame.size.w, (frame.size.h - deco).max(1)));
        toplevel.with_pending_state(|pending| pending.size = Some(size));

        // A hidden or minimized window is placed when it is shown again.
        if self.space.element_location(window).is_none() {
            return;
        }
        self.space
            .map_element(window.clone(), super::content_loc(window, frame.loc), false);
    }
}

/// Reports whether layout state, rather than floating geometry, controls the
/// window's position or size.
fn is_laid_out(window: &Window) -> bool {
    super::is_fullscreen(window) || Orbit::layout(window) != super::Layout::Floating
}
