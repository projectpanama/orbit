use orbit_core::ids::WindowId;
use smithay::desktop::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTarget {
    Index(usize),
    By(isize),
}

pub fn window_id(window: &Window) -> Option<WindowId> {
    crate::window::with_window_state(window, |state| state.id)
}
