use std::time::Duration;

use orbit_core::ids::{WindowId, WorkspaceId};
use smithay::{
    backend::renderer::{
        ImportAll, ImportMem,
        element::{
            RenderElementStates, memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement,
            utils::RescaleRenderElement,
        },
    },
    desktop::{
        Space, Window, layer_map_for_output,
        space::SpaceElement,
        utils::{
            OutputPresentationFeedback, surface_presentation_feedback_flags_from_states,
            surface_primary_scanout_output,
        },
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle},
    wayland::compositor::SurfaceData,
};

use crate::{state::Orbit, window::with_window_state};

smithay::backend::renderer::element::render_elements! {
    pub OrbitRenderElement<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<R>,
    RescaledSurface=RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    RescaledSolid=RescaleRenderElement<SolidColorRenderElement>,
    RescaledMemory=RescaleRenderElement<MemoryRenderBufferRenderElement<R>>,
}

/// Minimum interval between frame callbacks for hidden surfaces.
pub const HIDDEN_FRAME_INTERVAL: Duration = Duration::from_secs(1);

/// Tracks which parts of the scene need to be refreshed.
#[derive(Debug, Default)]
pub struct SpaceRefresh {
    /// Forces the complete space to refresh on the next pass.
    everything: bool,
    /// Window identifiers whose surfaces committed since the last frame.
    windows: Vec<WindowId>,
    /// Hash of the scene state used to detect structural changes.
    fingerprint: u64,
}

impl Orbit {
    /// Sends frame callbacks and refreshes the rendered scene after a frame.
    pub fn post_render(&mut self, output: &Output, _states: Option<&RenderElementStates>) {
        let _span = tracy_client::span!("post_render");
        let time = self.start_time.elapsed();
        let throttle = Some(HIDDEN_FRAME_INTERVAL);

        for window in self.space.elements() {
            window.send_frame(output, time, throttle, surface_primary_scanout_output);
        }

        let sliding: Vec<WorkspaceId> = self
            .outputs
            .get(output)
            .and_then(|state| state.workspace_slide.as_ref())
            .map(|slide| {
                slide
                    .visible(self.frames.frame_time)
                    .map(|(workspace, _)| workspace)
                    .collect()
            })
            .unwrap_or_default();

        for window in self.windows.values() {
            if self.space.element_location(window).is_some() {
                continue;
            }
            let drawn = !sliding.is_empty()
                && crate::workspaces::window_id(window)
                    .and_then(|id| self.workspaces.workspace_of(id))
                    .is_some_and(|(workspace, _)| sliding.contains(&workspace));
            if drawn {
                window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
            } else {
                window.send_frame(output, time, throttle, |_, _| None);
            }
        }

        self.refresh_space();
    }

    /// Advances workspace and window animations for an output.
    pub fn tick_animations(&mut self, output: &Output) {
        let now = self.frames.frame_time;
        if let Some(state) = self.outputs.get_mut(output) {
            let active = self.workspaces.active(state.id);
            // A switch that wasn't animated (e.g. between two empty workspaces) also ends the slide.
            if state.workspace_slide.as_ref().is_some_and(|slide| {
                slide.is_finished(now) || slide.target().is_some_and(|t| Some(t) != active)
            }) {
                state.workspace_slide = None;
            }
        }
        for window in self.space.elements() {
            with_window_state(window, |state| state.open_animation.tick(now));
        }
        // TODO: tick overview animation when shell is implemented
    }

    /// Reports whether an output currently has visible animation work.
    pub fn is_animating(&self, output: &Output) -> bool {
        let now = self.frames.frame_time;
        let sliding = self
            .outputs
            .get(output)
            .and_then(|state| state.workspace_slide.as_ref())
            .is_some_and(|slide| slide.is_moving(now));
        // TODO: should also check for overview animation when shell is implemented
        if sliding {
            return true;
        }
        let Some(output_geo) = self.space.output_geometry(output) else {
            return false;
        };
        self.space.elements().any(|window| {
            with_window_state(window, |state| state.open_animation.is_running(now))
                && self.window_visible_on(window, output_geo, 0)
        })
    }

    /// Reports whether a window intersects an output after applying `shift`.
    pub fn window_visible_on(
        &self,
        window: &Window,
        output_geo: Rectangle<i32, Logical>,
        shift: i32,
    ) -> bool {
        self.space
            .element_bbox(window)
            .into_iter()
            .chain(self.frame_geometry(window))
            .any(|mut rect| {
                rect.loc.x += shift;
                rect.overlaps(output_geo)
            })
    }

    /// Collects the presentation time requests of the surfaces shown in a
    /// frame on an output and is answered once that frame is presented.
    pub fn presentation_feedback(
        &self,
        output: &Output,
        states: &RenderElementStates,
    ) -> OutputPresentationFeedback {
        let mut feedback = OutputPresentationFeedback::new(output);
        let flags = |surface: &WlSurface, _: &SurfaceData| {
            surface_presentation_feedback_flags_from_states(surface, None, states)
        };
        for window in self.space.elements() {
            window.take_presentation_feedback(&mut feedback, surface_primary_scanout_output, flags);
        }
        for layer_surface in layer_map_for_output(output).layers() {
            layer_surface.take_presentation_feedback(
                &mut feedback,
                surface_primary_scanout_output,
                flags,
            );
        }
        feedback
    }

    /// Refreshes the scene or only windows with newly committed surfaces.
    fn refresh_space(&mut self) {
        let fingerprint = scene_fingerprint(&self.space);
        let refresh = &mut self.space_refresh;

        // If the scene has changed in any way, refresh everything. Otherwise,
        // only refresh the windows that have committed since the last frame.
        if std::mem::take(&mut refresh.everything) || fingerprint != refresh.fingerprint {
            refresh.fingerprint = fingerprint;
            refresh.windows.clear();
            self.space.refresh();
            return;
        }

        // Refresh only the windows that have committed since the last frame.
        for id in refresh.windows.drain(..) {
            if let Some(window) = self.windows.get(&id) {
                SpaceElement::refresh(window);
            }
        }
    }
}

/// Computes a hash of the current scene, including all windows and outputs.
fn scene_fingerprint(space: &Space<Window>) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};

    /// Hashes a rectangle's position and size, preserving `None`.
    fn rect(rect: Option<Rectangle<i32, Logical>>, hasher: &mut DefaultHasher) {
        rect.map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h))
            .hash(hasher);
    }

    let mut hasher = DefaultHasher::new();

    // Hash all windows and their bounding boxes.
    for window in space.elements() {
        crate::workspaces::window_id(window).hash(&mut hasher);
        rect(space.element_bbox(window), &mut hasher);
    }

    // Hash all outputs and their bounding boxes.
    for output in space.outputs() {
        rect(space.output_geometry(output), &mut hasher);
    }

    hasher.finish()
}
