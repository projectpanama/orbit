use std::{sync::PoisonError, time::Duration};

use smithay::{
    backend::renderer::{ImportAll, ImportMem, Renderer, Texture},
    desktop::{
        LayerSurface, PopupKind, WindowSurfaceType, find_popup_root_surface,
        get_popup_toplevel_coords, layer_map_for_output, utils::surface_primary_scanout_output,
    },
    output::Output,
    reexports::wayland_server::{
        Resource,
        protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Scale},
    wayland::{
        compositor::{get_role, with_states},
        shell::{
            wlr_layer::{
                self, KeyboardInteractivity, LAYER_SURFACE_ROLE, Layer, LayerSurfaceData,
                WlrLayerShellHandler, WlrLayerShellState,
            },
            xdg::PopupSurface,
        },
    },
};

use crate::{
    render::{HIDDEN_FRAME_INTERVAL, OrbitRenderElement, push_surface_with_popups},
    state::Orbit,
    window::is_fullscreen,
};

/// Layers drawn above normal windows, ordered from topmost to bottommost.
pub const UPPER_LAYERS: [Layer; 2] = [Layer::Overlay, Layer::Top];
/// Layers drawn below normal windows, ordered from topmost to bottommost.
pub const LOWER_LAYERS: [Layer; 2] = [Layer::Bottom, Layer::Background];

impl WlrLayerShellHandler for Orbit {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: wlr_layer::LayerSurface,
        output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let Some(output) = output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.active_output())
        else {
            tracing::warn!("no output for layer surface");
            surface.send_close();
            return;
        };
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!(?err, "failed to map layer surface");
        }
    }

    fn new_popup(&mut self, _parent: wlr_layer::LayerSurface, popup: PopupSurface) {
        // The popup itself is already tracked by the xdg-shell handler.
        self.unconstrain_layer_popup(&popup);
    }

    fn layer_destroyed(&mut self, surface: wlr_layer::LayerSurface) {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            let mut map = layer_map_for_output(&output);
            let layer = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
                drop(map);
                self.layers_changed(&output);
                self.queue_redraw(&output);
            }
        }
        self.refresh_layer_focus();
    }
}

impl Orbit {
    /// Returns the output whose layer map contains `surface`.
    ///
    /// The surface must be the toplevel surface of a mapped layer. Subsurfaces
    /// and unrelated surfaces are not matched.
    pub fn output_for_layer(&self, surface: &WlSurface) -> Option<Output> {
        self.space
            .outputs()
            .find(|o| {
                layer_map_for_output(o)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
    }

    /// Re-arranges a layer's output after `surface` commits.
    ///
    /// Sends the protocol's initial configure event once the surface has been
    /// mapped. If the exclusive zone changes, the output's workspaces are
    /// arranged again and keyboard focus is refreshed.
    pub fn handle_layer_commit(&mut self, surface: &WlSurface) {
        let Some(output) = self.output_for_layer(surface) else {
            return;
        };
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .is_some_and(|data| {
                    data.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .initial_configure_sent
                })
        });

        let mut map = layer_map_for_output(&output);
        let zone = map.non_exclusive_zone();
        map.arrange();

        if !initial_configure_sent
            && let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
        {
            layer.layer_surface().send_configure();
        }
        let zone_changed = map.non_exclusive_zone() != zone;
        drop(map);

        if zone_changed {
            self.layers_changed(&output);
        }
        self.refresh_layer_focus();
    }

    /// Returns the mapped layer surface that currently owns exclusive
    /// keyboard focus.
    ///
    /// Only mapped upper-layer surfaces with `Exclusive` keyboard
    /// interactivity qualify. Overlay surfaces take precedence over top-layer
    /// surfaces and the newest surface wins within a layer.
    pub fn exclusive_layer(&self) -> Option<WlSurface> {
        let mut surfaces = Vec::new();
        let mut states = Vec::new();
        for output in self.space.outputs() {
            let map = layer_map_for_output(output);
            for layer in map.layers().rev() {
                let state = layer.cached_state();
                // A configure is acked exactly while a buffer is attached.
                states.push((
                    state.layer,
                    state.keyboard_interactivity,
                    state.last_acked.is_some(),
                ));
                surfaces.push(layer.wl_surface().clone());
            }
        }
        exclusive_candidate(states).map(|index| surfaces.swap_remove(index))
    }

    /// Updates keyboard focus after layer surfaces are mapped or unmapped.
    ///
    /// An active keyboard grab or session lock prevents focus changes. An
    /// eligible exclusive layer receives focus, otherwise focus returns to
    /// the previously focused window when the current layer focus is invalid.
    pub fn refresh_layer_focus(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        // An open popup menu keeps the keyboard until it closes; the lock
        // screen keeps it until unlock.
        if keyboard.is_grabbed() || self.session_lock.is_active() {
            return;
        }
        let current = keyboard.current_focus();
        if let Some(exclusive) = self.exclusive_layer() {
            if current.as_ref() != Some(&exclusive) {
                keyboard.set_focus(self, Some(exclusive), SERIAL_COUNTER.next_serial());
            }
            return;
        }
        let Some(current) = current else {
            return;
        };
        // The role outlives the layer surface, so this also catches
        // destroyed ones and those closed with their output.
        let lost = !current.is_alive()
            || (get_role(&current) == Some(LAYER_SURFACE_ROLE)
                && !self.layer_takes_keyboard(&current));
        if lost {
            self.restore_window_focus();
        }
    }

    /// Restores focus to the last-focused window on the focused output.
    ///
    /// If no focused output is available, clears keyboard focus instead.
    pub fn restore_window_focus(&mut self) {
        match self
            .focused_output()
            .and_then(|output| self.output_id(&output))
        {
            Some(id) => self.refocus_output(id),
            None => self.focus_window(None, SERIAL_COUNTER.next_serial()),
        }
    }

    /// Returns the upper layers drawn and hit above windows on `output`.
    ///
    /// A fullscreen window on the output removes the `Top` layer from the
    /// result, leaving only `Overlay`.
    pub fn upper_layers_on(&self, output: &Output) -> &'static [Layer] {
        let fullscreen_on_top = self.space.output_geometry(output).is_some_and(|geo| {
            self.space
                .elements()
                .rev()
                .find(|window| self.window_visible_on(window, geo, 0))
                .is_some_and(is_fullscreen)
        });
        upper_layers(fullscreen_on_top)
    }

    /// Re-arranges layer surfaces on every output.
    ///
    /// This is used after changes such as an output resize.
    pub fn arrange_layers(&self) {
        for output in self.space.outputs() {
            layer_map_for_output(output).arrange();
        }
    }

    /// Returns the portion of `output` not reserved by exclusive layer zones.
    ///
    /// The returned rectangle is in global logical coordinates. Returns
    /// `None` when the output is not present in the global layout.
    pub fn usable_geometry(&self, output: &Output) -> Option<Rectangle<i32, Logical>> {
        let output_geo = self.space.output_geometry(output)?;
        let mut zone = layer_map_for_output(output).non_exclusive_zone();
        zone.loc += output_geo.loc;
        Some(zone)
    }

    /// Finds the upper layer surface or popup under `pos`.
    ///
    /// Returns the layer surface, the hit surface and the hit surface's
    /// global logical location.
    pub fn upper_layer_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        self.layer_surface_under(pos, |output| self.upper_layers_on(output))
    }

    /// Finds a lower layer surface or popup under `pos`.
    ///
    /// The return values have the same meaning as [`Self::upper_layer_surface_under`].
    pub fn lower_layer_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        self.layer_surface_under(pos, |_| &LOWER_LAYERS)
    }

    /// Returns the layer surface targeted by a press at `pos`.
    ///
    /// Upper layers are checked first. Lower layers are checked only when no
    /// normal window is hit at that position.
    pub fn layer_hit_for_press(&self, pos: Point<f64, Logical>) -> Option<LayerSurface> {
        self.upper_layer_surface_under(pos)
            .or_else(|| {
                self.hit_test(pos)
                    .is_none()
                    .then(|| self.lower_layer_surface_under(pos))
                    .flatten()
            })
            .map(|(layer, _, _)| layer)
    }

    /// Appends render elements for `layers` on `output`.
    ///
    /// Layers are rendered in the order supplied, with each layer's surfaces
    /// processed from topmost to bottommost. Surface locations are converted
    /// from output-local logical coordinates using `scale`.
    pub fn push_layer_elements<R>(
        elements: &mut Vec<OrbitRenderElement<R>>,
        renderer: &mut R,
        output: &Output,
        layers: &[Layer],
        scale: Scale<f64>,
    ) where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let map = layer_map_for_output(output);
        for &layer in layers {
            for layer_surface in map.layers_on(layer).rev() {
                let Some(geo) = map.layer_geometry(layer_surface) else {
                    continue;
                };
                push_surface_with_popups(
                    renderer,
                    layer_surface.wl_surface(),
                    Point::from((0, 0)),
                    geo.loc.to_physical_precise_round(scale),
                    scale,
                    1.0,
                    &mut |e| elements.push(OrbitRenderElement::Surface(e)),
                );
            }
        }
    }

    /// Sends frame callbacks to layer surfaces on `output` and removes dead
    /// surfaces from the layer map.
    ///
    /// `time` is the presentation time used for the callbacks.
    pub fn post_render_layers(&self, output: &Output, time: Duration) {
        let mut map = layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.send_frame(
                output,
                time,
                Some(HIDDEN_FRAME_INTERVAL),
                surface_primary_scanout_output,
            );
        }
        map.cleanup();
    }

    /// Constrains a layer-surface popup to its output.
    ///
    /// Returns `true` when the popup's root is a mapped layer surface and its
    /// pending geometry was updated. Returns `false` when the root is not a
    /// layer surface, the output is unavailable, or the layer geometry is unknown.
    pub fn unconstrain_layer_popup(&self, popup: &PopupSurface) -> bool {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return false;
        };
        let Some(output) = self.output_for_layer(&root) else {
            return false;
        };
        let Some(output_geo) = self.space.output_geometry(&output) else {
            return false;
        };
        let map = layer_map_for_output(&output);
        let Some(layer_geo) = map
            .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
            .and_then(|l| map.layer_geometry(l))
        else {
            return false;
        };

        // The target rectangle is the output, relative to the popup's parent.
        let mut target = Rectangle::from_size(output_geo.size);
        target.loc -= layer_geo.loc;
        target.loc -= get_popup_toplevel_coords(&kind);

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
        true
    }

    /// Re-arranges `output`'s workspaces after its reserved layer area changes.
    fn layers_changed(&mut self, output: &Output) {
        layer_map_for_output(output).arrange();
        self.arrange_output(output);
    }

    /// Finds a layer surface or popup under `pos` on selected layers.
    ///
    /// The layer list is selected for the output beneath `pos` and checked
    /// from first to last. The returned location is global logical coordinates.
    fn layer_surface_under<'a>(
        &self,
        pos: Point<f64, Logical>,
        layers: impl FnOnce(&Output) -> &'a [Layer],
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        let output = self.space.output_under(pos).next()?;
        let output_loc = self.space.output_geometry(output)?.loc;
        let map = layer_map_for_output(output);
        let local = pos - output_loc.to_f64();

        layers(output).iter().find_map(|&layer| {
            let layer_surface = map.layer_under(layer, local)?;
            let layer_loc = map.layer_geometry(layer_surface)?.loc;
            let (surface, surface_loc) =
                layer_surface.surface_under(local - layer_loc.to_f64(), WindowSurfaceType::ALL)?;
            let global = (surface_loc + layer_loc + output_loc).to_f64();
            Some((layer_surface.clone(), surface, global))
        })
    }

    /// Returns whether `surface` is mapped and requests keyboard interaction.
    fn layer_takes_keyboard(&self, surface: &WlSurface) -> bool {
        self.output_for_layer(surface).is_some_and(|output| {
            layer_map_for_output(&output)
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .is_some_and(|layer| {
                    let state = layer.cached_state();
                    state.last_acked.is_some()
                        && state.keyboard_interactivity != KeyboardInteractivity::None
                })
        })
    }
}

/// Returns the layer-shell layers drawn and hit above normal windows.
///
/// When `fullscreen_on_top` is true, a fullscreen window covers the `Top`
/// layer, leaving only `Overlay` above it. Otherwise both upper layers are
/// returned, ordered from topmost to bottommost.
pub fn upper_layers(fullscreen_on_top: bool) -> &'static [Layer] {
    if fullscreen_on_top {
        &UPPER_LAYERS[..1]
    } else {
        &UPPER_LAYERS
    }
}

/// Which layer surface takes the keyboard exclusively, given each one's
/// layer, keyboard interactivity and whether it is mapped, newest first.
///
/// Only mapped `Exclusive` surfaces above windows qualify (for the layers
/// below windows the protocol leaves it to the compositor, and Orbit treats
/// them as on-demand). `Overlay` beats `Top`, within a layer the newest wins.
fn exclusive_candidate(
    layers: impl IntoIterator<Item = (Layer, KeyboardInteractivity, bool)>,
) -> Option<usize> {
    layers
        .into_iter()
        .enumerate()
        .filter(|(_, (_, interactivity, mapped))| {
            *mapped && *interactivity == KeyboardInteractivity::Exclusive
        })
        .filter_map(|(index, (layer, _, _))| {
            UPPER_LAYERS
                .iter()
                .position(|&upper| upper == layer)
                .map(|rank| (rank, index))
        })
        .min_by_key(|&(rank, _)| rank)
        .map(|(_, index)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    use KeyboardInteractivity::{Exclusive, None as NoKeys, OnDemand};

    #[test]
    fn fullscreen_hides_only_the_top_layer() {
        assert_eq!(upper_layers(false), &[Layer::Overlay, Layer::Top]);
        assert_eq!(upper_layers(true), &[Layer::Overlay]);
    }

    #[test]
    fn no_exclusive_surface_means_no_candidate() {
        let layers = [(Layer::Top, OnDemand, true), (Layer::Overlay, NoKeys, true)];
        assert_eq!(exclusive_candidate(layers), None);
        assert_eq!(exclusive_candidate([]), None);
    }

    #[test]
    fn overlay_beats_top() {
        let layers = [
            (Layer::Top, Exclusive, true),
            (Layer::Overlay, Exclusive, true),
        ];
        assert_eq!(exclusive_candidate(layers), Some(1));
    }

    #[test]
    fn newest_wins_within_a_layer() {
        let layers = [(Layer::Top, Exclusive, true), (Layer::Top, Exclusive, true)];
        assert_eq!(exclusive_candidate(layers), Some(0));
    }

    #[test]
    fn unmapped_surfaces_are_skipped() {
        let layers = [
            (Layer::Overlay, Exclusive, false),
            (Layer::Top, Exclusive, true),
        ];
        assert_eq!(exclusive_candidate(layers), Some(1));
    }

    #[test]
    fn layers_below_windows_never_take_the_keyboard() {
        let layers = [
            (Layer::Bottom, Exclusive, true),
            (Layer::Background, Exclusive, true),
        ];
        assert_eq!(exclusive_candidate(layers), None);
    }
}
