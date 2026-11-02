use std::{collections::HashSet, time::Instant};

use orbit_core::ids::WindowId;
use smithay::input::{keyboard::Keycode, pointer::CursorImageStatus};

use crate::input::{cursor::CursorTheme, gestures::Swipe};

pub struct InputState {
    /// Keys whose press triggered a binding. Their release is swallowed
    /// too so clients never see half a key press.
    pub suppressed_keys: HashSet<Keycode>,
    pub swipe: Option<Swipe>,
    /// For detecting double-clicks on server-side title bars.
    pub last_titlebar_click: Option<(WindowId, Instant)>,
    pub cursor_status: CursorImageStatus,
    // / Set when Orbit draws named cursors itself.
    pub cursor_theme: Option<CursorTheme>,
}
