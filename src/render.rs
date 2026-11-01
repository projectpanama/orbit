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
    desktop::{Space, Window, space::SpaceElement, utils::surface_primary_scanout_output},
    output::Output,
    utils::{Logical, Rectangle},
};

use crate::state::Orbit;

smithay::backend::renderer::element::render_elements! {
    pub OrbitRenderElement<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<R>,
    RescaledSurface=RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    RescaledSolid=RescaleRenderElement<SolidColorRenderElement>,
    RescaledMemory=RescaleRenderElement<MemoryRenderBufferRenderElement<R>>,
}

pub const HIDDEN_FRAME_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub struct SpaceRefresh {
    everything: bool,
    windows: Vec<WindowId>,
    fingerprint: u64,
}

impl Orbit {
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

    /// Hashes a rectangle, or None if the rectangle is None.
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
