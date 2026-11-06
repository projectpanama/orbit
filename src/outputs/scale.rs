use smithay::{
    desktop::{find_popup_root_surface, utils::surface_primary_scanout_output},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        compositor::{SurfaceData, get_parent, with_states},
        fractional_scale::{FractionalScaleHandler, with_fractional_scale},
    },
};

use crate::state::Orbit;

impl FractionalScaleHandler for Orbit {
    /// Sends the preferred scale as soon as a client creates a fractional scale
    /// object for a surface.
    ///
    /// The initial event is sent before the first buffer is committed when the output
    /// can be determined. Later changes are propagated by the normal frame/update path.
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let Some(output) = self.scale_output_for(&surface) else {
            return;
        };
        with_states(&surface, |states| send_preferred_scale(states, &output));
    }
}

impl Orbit {
    /// Selects the output whose scale should be used for `surface`.
    ///
    /// Selection prefers the output currently scanning out the surface. If the
    /// surface is not currently displayed, it falls back to the output containing
    /// its toplevel window or layer, and finally to the active output.
    fn scale_output_for(&self, surface: &WlSurface) -> Option<Output> {
        let primary = with_states(surface, |states| {
            surface_primary_scanout_output(surface, states)
        });
        if primary.is_some() {
            return primary;
        }
        let root = self.root_surface(surface);
        if let Some(window) = self.window_for_surface(&root)
            && let Some(output) = self.window_output(&window)
        {
            return Some(output);
        }
        self.output_for_layer(&root)
            .or_else(|| self.active_output())
    }

    /// Finds the toplevel or layer surface that owns `surface`.
    ///
    /// Subsurfaces are followed through their parents. Popups are resolved to
    /// their root surface, including nested popups, before any subsurface
    /// parents are followed.
    fn root_surface(&self, surface: &WlSurface) -> WlSurface {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        // Popups aren't subsurfaces so follow them to their toplevel (through
        // nested popups too), then out of its subsurfaces again.
        let Some(popup) = self.popups.find_popup(&root) else {
            return root;
        };
        match find_popup_root_surface(&popup) {
            Ok(mut popup_root) => {
                while let Some(parent) = get_parent(&popup_root) {
                    popup_root = parent;
                }
                popup_root
            }
            Err(_) => root,
        }
    }
}

/// Advertises `output`'s current fractional scale for a surface.
pub fn send_preferred_scale(states: &SurfaceData, output: &Output) {
    // `states` must belong to the surface whose preferred scale is being
    // updated. Smithay suppresses the protocol event when the preferred scale
    // has not changed, so callers may invoke this function on every frame.
    with_fractional_scale(states, |fractional_scale| {
        fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
    });
}
