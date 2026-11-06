use std::{collections::HashSet, time::Instant};

use orbit_config::{Button, ModKey};
use orbit_core::ids::WindowId;
use smithay::input::{keyboard::Keycode, pointer::CursorImageStatus};

use crate::{
    input::{cursor::CursorTheme, gestures::WorkspaceSwipe},
    shell::overview::OverviewSwipe,
};

/// What input handling keeps between events.
pub struct InputState {
    /// Modifier that compositor keybindings and Mod+drag use.
    pub mod_key: ModKey,
    /// Keys whose press triggered a binding. Their release is swallowed
    /// too so clients never see half a key press.
    pub suppressed_keys: HashSet<Keycode>,
    /// The workspace swipe in progress, if any.
    pub workspace_swipe: Option<WorkspaceSwipe>,
    /// The overview swipe in progress, if any.
    pub overview_swipe: Option<OverviewSwipe>,
    /// For detecting double-clicks on server-side title bars.
    pub last_titlebar_click: Option<(WindowId, Instant)>,
    /// The title bar button under the pointer.
    pub decoration_hover: Option<(WindowId, Button)>,
    /// The status of the cursor image.
    pub cursor_status: CursorImageStatus,
    /// Set when Orbit draws named cursors itself.
    pub cursor_theme: Option<CursorTheme>,
}
