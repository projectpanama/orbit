use smithay::{
    desktop::{PopupKind, PopupManager, WindowSurfaceType, layer_map_for_output},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle},
    wayland::{
        input_method::{InputMethodHandler, PopupSurface},
        text_input::TextInputActivation,
    },
};

use crate::state::Orbit;

impl TextInputActivation for Orbit {}

impl InputMethodHandler for Orbit {
    /// Tracks a newly created input-method popup and schedules a redraw.
    fn new_popup(&mut self, surface: PopupSurface) {
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            tracing::warn!(?err, "failed to track an input method popup");
        }
        self.queue_redraw_all();
    }

    /// Dismisses a tracked input-method popup and schedules a redraw.
    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if let Some(parent) = surface.get_parent().map(|p| p.surface.clone()) {
            let _ = PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
        self.queue_redraw_all();
    }

    /// Schedules a redraw after an input-method popup changes position.
    fn popup_repositioned(&mut self, _surface: PopupSurface) {
        self.queue_redraw_all();
    }

    /// Returns the geometry of a parent window or layer surface.
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        if let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == parent))
        {
            return window.geometry();
        }

        // If the parent is not a toplevel, it may be a layer surface.
        // We can find its geometry by looking up the layer map for each
        // output and checking if the parent is a layer surface on that output.
        self.space
            .outputs()
            .find_map(|output| {
                let map = layer_map_for_output(output);
                let layer = map.layer_for_surface(parent, WindowSurfaceType::TOPLEVEL)?;
                map.layer_geometry(layer)
                    .map(|geo| Rectangle::new((0, 0).into(), geo.size))
            })
            .unwrap_or_default()
    }
}
