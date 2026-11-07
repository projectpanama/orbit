use std::{cmp::Reverse, time::Duration};

use orbit_core::ids::{WindowId, WorkspaceId};
use smithay::{
    backend::renderer::{
        ImportAll, ImportMem, Renderer, Texture,
        element::{
            Kind, RenderElementStates, default_primary_scanout_output_compare,
            memory::MemoryRenderBufferRenderElement,
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            utils::RescaleRenderElement,
        },
        utils::RendererSurfaceStateUserData,
    },
    desktop::{
        PopupManager, Space, Window, layer_map_for_output,
        space::SpaceElement,
        utils::{
            OutputPresentationFeedback, send_frames_surface_tree,
            surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
            update_surface_primary_scanout_output, with_surfaces_surface_tree,
        },
    },
    input::pointer::{CursorIcon, CursorImageStatus},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::compositor::{SurfaceData, TraversalAction, with_surface_tree_downward},
};

use crate::{
    geometry::{RectExt, ToCore, ToSmithay},
    input::cursor,
    outputs::scale,
    shell::layer,
    state::Orbit,
    window::{self, OpenAnimation, decorations::DecorationElement, with_window_state},
    workspaces::window_id,
};

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

const SNAP_PREVIEW_COLOR: [f32; 4] = [0.09, 0.15, 0.27, 0.3];
const FALLBACK_CURSOR_SIZE: i32 = 8;

/// Tracks which parts of the scene need refreshing.
#[derive(Debug, Default)]
pub struct SpaceRefresh {
    /// Forces the complete space to refresh on the next pass.
    everything: bool,
    /// Window identifiers whose surfaces committed since the last frame.
    windows: Vec<WindowId>,
    /// Hash of the scene state used to detect structural changes.
    fingerprint: u64,
}

impl SpaceRefresh {
    /// Records a commit from a window surface.
    ///
    /// The commit may change subsurfaces or popups. Size changes are detected
    /// through the scene fingerprint.
    pub fn window_committed(&mut self, window: WindowId) {
        if !self.windows.contains(&window) {
            self.windows.push(window);
        }
    }

    /// Requests a complete scene refresh.
    ///
    /// Use this for non window surfaces or destroyed surfaces.
    pub fn refresh_everything(&mut self) {
        self.everything = true;
    }
}

#[derive(Debug, Clone, Copy)]
/// Visual adjustments applied while drawing a window.
struct WindowLook {
    /// Physical translation applied to the window.
    offset: Point<i32, Physical>,
    /// Opacity used for the window and its decorations.
    alpha: f32,
    /// Scale applied around the window frame center.
    scale: f64,
}

impl WindowLook {
    /// Creates an unscaled window look with a horizontal shift.
    fn shifted(offset: i32) -> Self {
        Self {
            offset: (offset, 0).into(),
            alpha: 1.0,
            scale: 1.0,
        }
    }
}

/// Render overlays that are drawn above the window scene.
#[derive(Debug, Default)]
pub struct Overlays {
    /// Current snap preview rectangle in logical coordinates.
    pub snap_preview: Option<Rectangle<i32, Logical>>,
    /// Buffer used to draw the snap preview.
    snap_preview_buffer: SolidColorBuffer,
    /// Buffer used when a named cursor image is unavailable.
    fallback_cursor_buffer: SolidColorBuffer,
}

impl Overlays {
    /// Adds the current snap preview to a render element list.
    fn push_snap_preview<R>(
        &mut self,
        elements: &mut Vec<OrbitRenderElement<R>>,
        output_loc: Point<i32, Logical>,
        scale: Scale<f64>,
    ) where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let Some(preview) = self.snap_preview else {
            return;
        };
        self.snap_preview_buffer
            .update(preview.size, SNAP_PREVIEW_COLOR);
        let loc = (preview.loc - output_loc).to_physical_precise_round(scale);
        elements.push(OrbitRenderElement::Solid(
            SolidColorRenderElement::from_buffer(
                &self.snap_preview_buffer,
                loc,
                scale,
                1.0,
                Kind::Unspecified,
            ),
        ));
    }
}

impl Orbit {
    /// Builds the render elements for one output.
    ///
    /// Elements include cursors, lock surfaces, layers, windows, workspace
    /// slides, overview dimming, and snap previews in draw order.
    pub fn render_elements<R>(
        &mut self,
        renderer: &mut R,
        output: &Output,
        elements: &mut Vec<OrbitRenderElement<R>>,
    ) where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let _span = tracy_client::span!("render_elements");
        elements.clear();
        let Some(output_geo) = self.space.output_geometry(output) else {
            return;
        };
        let scale = output.current_scale().fractional_scale().into();
        let to_local = |loc: Point<i32, Logical>| loc - output_geo.loc;

        let pointer = self.pointer.clone().current_location();
        if let CursorImageStatus::Named(icon) = self.input.cursor_status
            && self.input.cursor_theme.is_some()
        {
            self.push_named_cursor(
                elements,
                renderer,
                icon,
                pointer - output_geo.loc.to_f64(),
                scale,
            );
        }

        // Draw client supplied cursors.
        if let CursorImageStatus::Surface(surface) = &self.input.cursor_status {
            let hotspot = cursor::surface_hotspot(surface);
            let loc: Point<i32, Physical> = (pointer - hotspot.to_f64() - output_geo.loc.to_f64())
                .to_physical(scale)
                .to_i32_round();
            push_surface_tree(renderer, surface, loc, scale, 1.0, Kind::Cursor, &mut |e| {
                elements.push(OrbitRenderElement::Surface(e))
            });
        }

        // A locked output shows its lock surface over the background.
        // If no lock surface exists only the background is shown.
        if self.session_lock.is_active() {
            if let Some(lock) = self.session_lock.surface_on(output) {
                push_surface_tree(
                    renderer,
                    lock.wl_surface(),
                    Point::from((0, 0)),
                    scale,
                    1.0,
                    Kind::Unspecified,
                    &mut |e| elements.push(OrbitRenderElement::Surface(e)),
                );
            }
            return;
        }

        // Panels sit above windows unless a fullscreen window is on top.
        let upper = self.upper_layers_on(output);
        Self::push_layer_elements(elements, renderer, output, upper, scale);

        // Track workspace slide offsets in physical pixels.
        // The active workspace can be outside the visible range.
        let output_id = self.output_id(output);
        let slide = self
            .outputs
            .get(output)
            .and_then(|state| state.workspace_slide.as_ref());
        let mut active_offset = Some(0);
        let mut sliding: Vec<(WorkspaceId, i32)> = Vec::new();
        if let (Some(id), Some(slide)) = (output_id, slide) {
            let active = self.workspaces.active(id);
            let width = f64::from(output_geo.size.w) * scale.x;
            active_offset = None;
            for (workspace, offset) in slide.visible(self.frames.frame_time) {
                let offset = (offset * width).round() as i32;
                if Some(workspace) == active {
                    active_offset = Some(offset);
                } else {
                    sliding.push((workspace, offset));
                }
            }
        }
        let sliding_output = slide.and(output_id);

        // Draw each window decoration below its own surfaces.
        // This keeps a lower title bar from covering a higher window.
        for (index, window) in self.space.elements().rev().enumerate() {
            // Draw the snap preview below the topmost window.
            if index == 1 {
                self.overlays
                    .push_snap_preview(elements, output_geo.loc, scale);
            }
            // Windows on this output slide with the workspace.
            // Windows from other outputs stay in place.
            let offset = match sliding_output {
                Some(id)
                    if window_id(window).and_then(|window| self.workspaces.output_of(window))
                        == Some(id) =>
                {
                    match active_offset {
                        Some(offset) => offset,
                        None => continue,
                    }
                }
                _ => 0,
            };
            // Skip windows that do not intersect this output.
            let shift = (f64::from(offset) / scale.x).round() as i32;
            if !self.window_visible_on(window, output_geo, shift) {
                continue;
            }
            let Some(location) = self.space.element_location(window) else {
                continue;
            };
            let mut look = self.window_look(window, offset);
            // Move and scale windows towards their overview positions.
            if let Some((target, p)) = self.overview_target(output, window)
                && let Some(mut real) = self.frame_geometry(window)
            {
                real.loc -= output_geo.loc;
                let (shift, factor) =
                    orbit_core::overview::interpolate(real.to_core(), target.to_core(), p);
                look.offset += shift.to_smithay().to_physical(scale).to_i32_round();
                look.scale *= factor;
            }
            Self::push_window(elements, renderer, window, to_local(location), scale, look);
        }

        if self.space.elements().count() < 2 {
            self.overlays
                .push_snap_preview(elements, output_geo.loc, scale);
        }

        for (workspace, offset) in sliding {
            self.push_hidden_workspace(elements, renderer, workspace, offset, output_geo, scale);
        }

        // Dim the scene behind the overview windows.
        let dim = self.config.overview_dim * self.overview_progress(output);
        if dim > 0.0
            && let Some(overview) = self
                .outputs
                .get_mut(output)
                .and_then(|s| s.overview.as_mut())
        {
            overview
                .dim
                .update(output_geo.size, [0.0, 0.0, 0.0, dim as f32]);
            elements.push(OrbitRenderElement::Solid(
                SolidColorRenderElement::from_buffer(
                    &overview.dim,
                    Point::<i32, Physical>::from((0, 0)),
                    scale,
                    1.0,
                    Kind::Unspecified,
                ),
            ));
        }

        Self::push_layer_elements(elements, renderer, output, &layer::LOWER_LAYERS, scale);
    }

    /// Reports whether a window intersects an output after applying a shift.
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

    fn window_look(&self, window: &Window, offset: i32) -> WindowLook {
        let (alpha, scale) = with_window_state(window, |state| {
            // Wait for the client to draw before starting the animation.
            if matches!(state.open_animation, OpenAnimation::Waiting(_)) && window.bbox().is_empty()
            {
                return (1.0, 1.0);
            }
            state.open_animation.sample(self.frames.frame_time)
        });
        WindowLook {
            alpha,
            scale,
            ..WindowLook::shifted(offset)
        }
    }

    fn push_window<R>(
        elements: &mut Vec<OrbitRenderElement<R>>,
        renderer: &mut R,
        window: &Window,
        location: Point<i32, Logical>,
        scale: Scale<f64>,
        look: WindowLook,
    ) where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let geometry = window.geometry();
        let frame = window::frame_at(window, location);
        let render_loc = (location - geometry.loc).to_physical_precise_round(scale) + look.offset;
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let surface = toplevel.wl_surface();

        if look.scale == 1.0 {
            push_surface_with_popups(
                renderer,
                surface,
                geometry.loc,
                render_loc,
                scale,
                look.alpha,
                &mut |e| elements.push(OrbitRenderElement::Surface(e)),
            );
            let _span = tracy_client::span!("decorations");
            window::decorations::render_elements(
                renderer,
                window,
                frame,
                look.offset,
                scale,
                look.alpha,
                |e| {
                    elements.push(match e {
                        DecorationElement::Bar(e) => OrbitRenderElement::Memory(e),
                        DecorationElement::Solid(e) => OrbitRenderElement::Solid(e),
                    });
                },
            );
            return;
        }
        let center = frame.center();
        let origin = center.to_physical(scale).to_i32_round() + look.offset;
        push_surface_with_popups(
            renderer,
            surface,
            geometry.loc,
            render_loc,
            scale,
            look.alpha,
            &mut |e| {
                elements.push(OrbitRenderElement::RescaledSurface(
                    RescaleRenderElement::from_element(e, origin, look.scale),
                ))
            },
        );
        let _span = tracy_client::span!("decorations");
        window::decorations::render_elements(
            renderer,
            window,
            frame,
            look.offset,
            scale,
            look.alpha,
            |e| {
                elements.push(match e {
                    DecorationElement::Bar(e) => OrbitRenderElement::RescaledMemory(
                        RescaleRenderElement::from_element(e, origin, look.scale),
                    ),
                    DecorationElement::Solid(e) => OrbitRenderElement::RescaledSolid(
                        RescaleRenderElement::from_element(e, origin, look.scale),
                    ),
                });
            },
        );
    }

    /// Adds windows from a hidden workspace that is sliding into view.
    ///
    /// Windows are drawn at their saved positions and preserve stacking order.
    fn push_hidden_workspace<R>(
        &self,
        elements: &mut Vec<OrbitRenderElement<R>>,
        renderer: &mut R,
        workspace: WorkspaceId,
        offset: i32,
        output_geo: Rectangle<i32, Logical>,
        scale: Scale<f64>,
    ) where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let mut windows: Vec<(&Window, Point<i32, Logical>, u64)> = self
            .workspaces
            .windows_of(workspace)
            .iter()
            .filter_map(|id| self.windows.get(id))
            .filter(|window| !window::is_minimized(window))
            .filter_map(|window| {
                let (relative, stamp) = with_window_state(window, |state| state.hidden_at)?;
                Some((window, relative, stamp))
            })
            .collect();
        // Draw the most recently hidden windows first.
        windows.sort_by_key(|&(_, _, stamp)| Reverse(stamp));

        let shift = (f64::from(offset) / scale.x).round() as i32;
        let view = Rectangle::from_size(output_geo.size);
        for (window, location, _) in windows {
            let mut bbox = window.bbox();
            bbox.loc += location - window.geometry().loc + Point::from((shift, 0));
            if !window::frame_around(window, bbox).overlaps(view) {
                continue;
            }
            Self::push_window(
                elements,
                renderer,
                window,
                location,
                scale,
                WindowLook::shifted(offset),
            );
        }
    }

    /// Adds a named cursor from the cursor theme at an output local point.
    fn push_named_cursor<R>(
        &mut self,
        elements: &mut Vec<OrbitRenderElement<R>>,
        renderer: &mut R,
        icon: CursorIcon,
        pointer: Point<f64, Logical>,
        scale: Scale<f64>,
    ) where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        // Use the smallest cursor image that is sharp at the output scale.
        let buffer_scale = scale.x.ceil().max(1.0) as u32;
        let image = self
            .input
            .cursor_theme
            .as_mut()
            .and_then(|theme| theme.get(icon, buffer_scale));
        match image {
            Some(image) => {
                let loc = (pointer - image.hotspot.to_f64()).to_physical(scale);
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    loc,
                    &image.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(element) => elements.push(OrbitRenderElement::Memory(element)),
                    Err(err) => tracing::warn!(?err, "failed to upload cursor image"),
                }
            }
            None => {
                let buffer = &mut self.overlays.fallback_cursor_buffer;
                buffer.update(
                    (FALLBACK_CURSOR_SIZE, FALLBACK_CURSOR_SIZE),
                    [1.0, 1.0, 1.0, 1.0],
                );
                elements.push(OrbitRenderElement::Solid(
                    SolidColorRenderElement::from_buffer(
                        buffer,
                        pointer.to_physical(scale).to_i32_round(),
                        scale,
                        1.0,
                        Kind::Cursor,
                    ),
                ));
            }
        }
    }
}

impl Orbit {
    /// Sends frame callbacks and updates scene state after a rendered frame.
    pub fn post_render(&mut self, output: &Output, states: Option<&RenderElementStates>) {
        let _span = tracy_client::span!("post_render");
        if let Some(states) = states {
            self.update_primary_outputs(output, states);
        }

        let time = self.start_time.elapsed();
        let throttle = Some(HIDDEN_FRAME_INTERVAL);
        for window in self.space.elements() {
            window.send_frame(output, time, throttle, surface_primary_scanout_output);
        }
        // Hidden and minimized windows are not drawn.
        // They still receive occasional callbacks so they do not stall.
        // Sliding workspaces receive callbacks as if they were drawn.
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
                && window_id(window)
                    .and_then(|id| self.workspaces.workspace_of(id))
                    .is_some_and(|(workspace, _)| sliding.contains(&workspace));
            if drawn {
                window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
            } else {
                window.send_frame(output, time, throttle, |_, _| None);
            }
        }
        if let CursorImageStatus::Surface(surface) = &self.input.cursor_status {
            send_frames_surface_tree(
                surface,
                output,
                time,
                throttle,
                surface_primary_scanout_output,
            );
        }
        self.post_render_layers(output, time);
        if let Some(lock) = self.session_lock.surface_on(output) {
            send_frames_surface_tree(
                lock.wl_surface(),
                output,
                time,
                throttle,
                surface_primary_scanout_output,
            );
        }
        self.lock_frame_drawn(output);
        // Let output captures waiting for a new frame use this frame.
        self.schedule_capture_flush();
        // Notify overview clients about progress and layout.
        self.refresh_overview_protocol(output);

        self.refresh_space();
        self.popups.cleanup();
    }

    /// Advances workspace, window, and overview animations for an output.
    pub fn tick_animations(&mut self, output: &Output) {
        let now = self.frames.frame_time;
        if let Some(state) = self.outputs.get_mut(output) {
            let active = self.workspaces.active(state.id);
            // A switch without animation also ends the slide.
            if state.workspace_slide.as_ref().is_some_and(|slide| {
                slide.is_finished(now) || slide.target().is_some_and(|t| Some(t) != active)
            }) {
                state.workspace_slide = None;
            }
        }
        for window in self.space.elements() {
            with_window_state(window, |state| state.open_animation.tick(now));
        }
        self.tick_overview(output);
    }

    /// Reports whether an output has visible animation work.
    pub fn is_animating(&self, output: &Output) -> bool {
        let now = self.frames.frame_time;
        let sliding = self
            .outputs
            .get(output)
            .and_then(|state| state.workspace_slide.as_ref())
            .is_some_and(|slide| slide.is_moving(now));
        if sliding || self.overview_animating(output) {
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

    /// Collects presentation feedback for surfaces shown on an output.
    ///
    /// The returned feedback is completed when the corresponding frame is
    /// presented.
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

    /// Updates primary scanout outputs and preferred scale for surfaces.
    fn update_primary_outputs(&self, output: &Output, states: &RenderElementStates) {
        let update = |surface: &WlSurface, data: &SurfaceData| {
            let primary = update_surface_primary_scanout_output(
                surface,
                output,
                data,
                None,
                states,
                default_primary_scanout_output_compare,
            );
            if let Some(primary) = primary {
                scale::send_preferred_scale(data, &primary);
            }
        };
        for window in self.space.elements() {
            window.with_surfaces(update);
        }
        for layer_surface in layer_map_for_output(output).layers() {
            layer_surface.with_surfaces(update);
        }
        if let CursorImageStatus::Surface(surface) = &self.input.cursor_status {
            with_surfaces_surface_tree(surface, update);
        }
    }

    /// Refreshes the complete scene or only newly committed windows.
    fn refresh_space(&mut self) {
        let fingerprint = scene_fingerprint(&self.space);
        let refresh = &mut self.space_refresh;

        // A structural change refreshes the complete scene.
        // Otherwise only committed windows need refreshing.
        if std::mem::take(&mut refresh.everything) || fingerprint != refresh.fingerprint {
            refresh.fingerprint = fingerprint;
            refresh.windows.clear();
            self.space.refresh();
            return;
        }

        // Refresh windows that committed since the last frame.
        for id in refresh.windows.drain(..) {
            if let Some(window) = self.windows.get(&id) {
                SpaceElement::refresh(window);
            }
        }
    }
}

/// Adds a surface tree to a render element list.
///
/// Subsurfaces are placed using their compositor view offsets. Surfaces
/// without a renderable view and their children are skipped.
pub fn push_surface_tree<R>(
    renderer: &mut R,
    surface: &WlSurface,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
    kind: Kind,
    push: &mut impl FnMut(WaylandSurfaceRenderElement<R>),
) where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    // Apply each surface view offset to its content location.
    let placed = |states: &SurfaceData, location: Point<f64, Physical>| {
        let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
        let view = data.lock().ok()?.view()?;
        Some(location + view.offset.to_f64().to_physical(scale))
    };
    with_surface_tree_downward(
        surface,
        location.to_f64(),
        |_, states, &location| match placed(states, location) {
            Some(location) => TraversalAction::DoChildren(location),
            None => TraversalAction::SkipChildren,
        },
        |surface, states, &location| {
            let Some(location) = placed(states, location) else {
                return;
            };
            match WaylandSurfaceRenderElement::from_surface(
                renderer, surface, states, location, alpha, kind,
            ) {
                Ok(Some(element)) => push(element),
                // The surface is not mapped.
                Ok(None) => {}
                Err(err) => tracing::warn!(?err, "failed to import a surface"),
            }
        },
        |_, _, _| true,
    );
}

/// Adds a surface and all of its popups to a render element list.
///
/// Popup locations are converted from logical geometry to physical output
/// coordinates before their surface trees are added.
pub fn push_surface_with_popups<R>(
    renderer: &mut R,
    surface: &WlSurface,
    geometry_loc: Point<i32, Logical>,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
    push: &mut impl FnMut(WaylandSurfaceRenderElement<R>),
) where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
        let offset =
            (geometry_loc + popup_offset - popup.geometry().loc).to_physical_precise_round(scale);
        push_surface_tree(
            renderer,
            popup.wl_surface(),
            location + offset,
            scale,
            alpha,
            Kind::Unspecified,
            push,
        );
    }
    push_surface_tree(
        renderer,
        surface,
        location,
        scale,
        alpha,
        Kind::Unspecified,
        push,
    );
}

/// Computes a hash of window and output geometry in the current scene.
fn scene_fingerprint(space: &Space<Window>) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};

    /// Hashes a rectangle's position and size, preserving `None`.
    fn rect(rect: Option<Rectangle<i32, Logical>>, hasher: &mut DefaultHasher) {
        rect.map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h))
            .hash(hasher);
    }

    let mut hasher = DefaultHasher::new();

    // Include window identifiers and bounding boxes.
    for window in space.elements() {
        crate::workspaces::window_id(window).hash(&mut hasher);
        rect(space.element_bbox(window), &mut hasher);
    }

    // Include output bounding boxes.
    for output in space.outputs() {
        rect(space.output_geometry(output), &mut hasher);
    }

    hasher.finish()
}
