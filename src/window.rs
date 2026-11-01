use std::cell::RefCell;

use orbit_core::ids::WindowId;
use smithay::desktop::Window;

#[derive(Debug, Default)]
pub struct WindowState {
    /// Set when the window is registered.
    pub id: Option<WindowId>,
}

pub fn with_window_state<T>(window: &Window, f: impl FnOnce(&mut WindowState) -> T) -> T {
    let user_data = window.user_data();
    user_data.insert_if_missing(|| RefCell::new(WindowState::default()));
    f(&mut user_data
        .get::<RefCell<WindowState>>()
        .expect("inserted just above")
        .borrow_mut())
}
