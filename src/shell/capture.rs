use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, ImportAll, ImportMem, Offscreen, Renderer, Texture, TextureMapping,
            damage::OutputDamageTracker,
            element::{Element, Kind},
            gles::GlesTexture,
        },
    },
    output::{Output, WeakOutput},
    reexports::wayland_server::{
        Client, DisplayHandle,
        protocol::{wl_buffer::WlBuffer, wl_shm},
    },
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::{
        foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelWeakHandle},
        image_capture_source::{
            ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler,
            OutputCaptureSourceState, ToplevelCaptureSourceHandler, ToplevelCaptureSourceState,
        },
        image_copy_capture::{
            BufferConstraints, CaptureFailureReason, Frame, FrameRef, ImageCopyCaptureHandler,
            ImageCopyCaptureState, Session, SessionRef,
        },
        shm::with_buffer_contents_mut,
    },
};

use crate::{
    backend::Backend,
    protocols::privileged,
    render::{OrbitRenderElement, push_surface_with_popups},
    state::Orbit,
};

/// The formats a capture can be read back in.
const SHM_FORMATS: [wl_shm::Format; 2] = [wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888];

/// What to render for a capture right now.
enum Live {
    /// A complete output capture.
    Output(Output),
    /// A single window capture.
    Window(smithay::desktop::Window),
}

/// What a capture source shows. Stored in the source's user data.
enum Target {
    /// An output source.
    Output(WeakOutput),
    /// A foreign toplevel source.
    Toplevel(ForeignToplevelWeakHandle),
}

/// Marker set after a session delivers its first frame.
///
/// Later frames wait for a new output frame or window commit.
struct Delivered;

/// Runtime state for image capture protocol globals and sessions.
pub struct CaptureState {
    /// State for the output capture source manager.
    output_sources: OutputCaptureSourceState,
    /// State for the toplevel capture source manager.
    toplevel_sources: ToplevelCaptureSourceState,
    /// State for image copy capture sessions.
    copy: ImageCopyCaptureState,
    /// Live sessions whose drop stops the client session.
    sessions: Vec<Session>,
    /// Frames waiting for something new to capture.
    pending: Vec<(SessionRef, Frame)>,
    /// An idle callback will try the pending frames.
    flush_scheduled: bool,
}

impl CaptureState {
    /// Creates capture protocol state and registers its globals.
    ///
    /// When `enabled` is false no client can view the globals. Privileged
    /// clients are still required when the feature is enabled.
    pub fn new(dh: &DisplayHandle, enabled: bool) -> Self {
        let visible = move |client: &Client| enabled && privileged(client);
        Self {
            output_sources: OutputCaptureSourceState::new_with_filter::<Orbit, _>(dh, visible),
            toplevel_sources: ToplevelCaptureSourceState::new_with_filter::<Orbit, _>(dh, visible),
            copy: ImageCopyCaptureState::new_with_filter::<Orbit, _>(dh, visible),
            sessions: Vec::new(),
            pending: Vec::new(),
            flush_scheduled: false,
        }
    }

    /// Returns the number of active capture sessions.
    #[cfg(test)]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

impl ImageCaptureSourceHandler for Orbit {}

impl OutputCaptureSourceHandler for Orbit {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.capture.output_sources
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source
            .user_data()
            .insert_if_missing(|| Target::Output(output.downgrade()));
    }
}

impl ToplevelCaptureSourceHandler for Orbit {
    fn toplevel_capture_source_state(&mut self) -> &mut ToplevelCaptureSourceState {
        &mut self.capture.toplevel_sources
    }

    fn toplevel_source_created(
        &mut self,
        source: ImageCaptureSource,
        toplevel: ForeignToplevelHandle,
    ) {
        source
            .user_data()
            .insert_if_missing(|| Target::Toplevel(toplevel.downgrade()));
    }
}

impl ImageCopyCaptureHandler for Orbit {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.capture.copy
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        let live = self.live_target(source)?;
        self.constraints_for(&live)
    }

    fn new_session(&mut self, session: Session) {
        self.capture.sessions.push(session);
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        if session.user_data().get::<Delivered>().is_none() {
            self.capture_now(session, frame);
        } else {
            self.capture.pending.push((session.clone(), frame));
        }
    }

    fn frame_aborted(&mut self, frame: FrameRef) {
        self.capture.pending.retain(|(_, f)| **f != frame);
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        self.capture.pending.retain(|(s, _)| *s != session);
        self.capture.sessions.retain(|s| *s != session);
    }
}

impl Orbit {
    fn live_target(&self, source: &ImageCaptureSource) -> Option<Live> {
        match target(source)? {
            Target::Output(weak) => {
                let output = weak.upgrade()?;
                self.outputs
                    .contains_key(&output)
                    .then_some(Live::Output(output))
            }
            Target::Toplevel(weak) => {
                let handle = weak.upgrade()?;
                self.window_for_foreign_handle(&handle).map(Live::Window)
            }
        }
    }

    /// Returns the scale used to capture a window.
    ///
    /// The window output scale is preferred. The first available output scale
    /// is used as a fallback and one is used when no output exists.
    fn window_capture_scale(&self, window: &smithay::desktop::Window) -> f64 {
        self.space
            .outputs_for_element(window)
            .first()
            .or_else(|| self.space.outputs().next())
            .map_or(1.0, |o| o.current_scale().fractional_scale())
    }

    /// Returns the current physical size and scale for a capture target.
    fn capture_geometry(&self, live: &Live) -> Option<(Size<i32, Physical>, f64)> {
        match live {
            Live::Output(output) => {
                let geo = self.space.output_geometry(output)?;
                let scale = output.current_scale().fractional_scale();
                Some((geo.size.to_physical_precise_round(scale), scale))
            }
            Live::Window(window) => {
                let scale = self.window_capture_scale(window);
                let size = window.geometry().size.to_physical_precise_round(scale);
                (size.w > 0 && size.h > 0).then_some((size, scale))
            }
        }
    }

    fn constraints_for(&self, live: &Live) -> Option<BufferConstraints> {
        let (size, _) = self.capture_geometry(live)?;
        Some(BufferConstraints {
            size: Size::<i32, BufferCoords>::from((size.w, size.h)),
            shm: SHM_FORMATS.to_vec(),
            dma: None,
        })
    }

    /// Captures into the frame buffer or reports a capture failure.
    ///
    /// A frame is queued when capture is requested from inside the backend.
    fn capture_now(&mut self, session: &SessionRef, frame: Frame) {
        let _span = tracy_client::span!("capture");
        let Some(live) = self.live_target(&session.source()) else {
            return frame.fail(CaptureFailureReason::Stopped);
        };
        let Some((size, scale)) = self.capture_geometry(&live) else {
            return frame.fail(CaptureFailureReason::Unknown);
        };
        // A resized source needs new constraints and a different buffer.
        let expected = Size::<i32, BufferCoords>::from((size.w, size.h));
        if session
            .current_constraints()
            .is_none_or(|c| c.size != expected)
        {
            if let Some(c) = self.constraints_for(&live) {
                session.update_constraints(c);
            }
            return frame.fail(CaptureFailureReason::BufferConstraints);
        }
        let buffer = frame.buffer();
        // Sample animations at the time a matching frame would be drawn.
        if let Live::Output(output) = &live {
            self.frames.frame_time = self.next_frame_time(output);
        }
        let rendered = self.with_backend(|backend, orbit| match backend {
            Backend::Winit(winit) => orbit.render_capture::<_, GlesTexture>(
                winit.renderer(),
                &live,
                size,
                scale,
                &buffer,
                true,
            ),
            Backend::Tty(tty) => orbit.render_capture::<_, GlesTexture>(
                tty.renderer(),
                &live,
                size,
                scale,
                &buffer,
                true,
            ),
            #[cfg(test)]
            Backend::Headless(headless) => match headless.renderer.as_mut() {
                Some(renderer) => {
                    orbit.render_capture(renderer, &live, size, scale, &buffer, false)
                }
                None => Err("no renderer".into()),
            },
        });
        match rendered {
            Some(Ok(())) => {
                session.user_data().insert_if_missing(|| Delivered);
                let now: Duration = self.clock.now().into();
                frame.success(Transform::Normal, None, now);
            }
            Some(Err(err)) => {
                tracing::debug!(%err, "capture failed");
                frame.fail(CaptureFailureReason::Unknown);
            }
            // The request came from inside the backend so try again later.
            None => self.capture.pending.push((session.clone(), frame)),
        }
    }

    /// Renders `live` into a shared memory buffer.
    ///
    /// The buffer must be at least `size`. The `bottom_up` flag identifies
    /// renderers that return rows from the bottom upward.
    fn render_capture<R, T>(
        &mut self,
        renderer: &mut R,
        live: &Live,
        size: Size<i32, Physical>,
        scale: f64,
        buffer: &WlBuffer,
        bottom_up: bool,
    ) -> Result<(), String>
    where
        R: Renderer + ImportAll + ImportMem + Offscreen<T> + Bind<T> + ExportMem,
        R::TextureId: Texture + Clone + Send + 'static,
        T: 'static,
    {
        let scale_f = Scale::from(scale);
        let mut elements: Vec<OrbitRenderElement<R>> = Vec::new();
        match live {
            Live::Output(output) => {
                self.render_elements(renderer, output, &mut elements);
                // Screenshots and thumbnails do not include the pointer.
                elements.retain(|e| e.kind() != Kind::Cursor);
            }
            Live::Window(window) => {
                let loc: Point<i32, Physical> =
                    (Point::from((0, 0)) - window.geometry().loc).to_physical_precise_round(scale);
                if let Some(toplevel) = window.toplevel() {
                    push_surface_with_popups(
                        renderer,
                        toplevel.wl_surface(),
                        window.geometry().loc,
                        loc,
                        scale_f,
                        1.0,
                        &mut |e| elements.push(OrbitRenderElement::Surface(e)),
                    );
                }
            }
        }

        let format = smithay::wayland::shm::with_buffer_contents(buffer, |_, _, data| data.format)
            .map_err(|e| format!("not a shm buffer: {e:?}"))?;
        let fourcc = match format {
            wl_shm::Format::Argb8888 => Fourcc::Argb8888,
            wl_shm::Format::Xrgb8888 => Fourcc::Xrgb8888,
            other => return Err(format!("unsupported format {other:?}")),
        };
        let buffer_size = Size::<i32, BufferCoords>::from((size.w, size.h));
        let mut target: T = renderer
            .create_buffer(Fourcc::Abgr8888, buffer_size)
            .map_err(|e| format!("offscreen buffer: {e:?}"))?;
        let pixels = {
            let mut fb = renderer
                .bind(&mut target)
                .map_err(|e| format!("bind: {e:?}"))?;
            let mut damage = OutputDamageTracker::new(size, scale, Transform::Normal);
            damage
                .render_output(renderer, &mut fb, 0, &elements, [0.0, 0.0, 0.0, 0.0])
                .map_err(|e| format!("render: {e:?}"))?;
            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), fourcc)
                .map_err(|e| format!("read back: {e:?}"))?;
            let flipped = mapping.flipped() != bottom_up;
            let data = renderer
                .map_texture(&mapping)
                .map_err(|e| format!("map: {e:?}"))?;
            (data.to_vec(), flipped)
        };
        drop(elements);
        let (pixels, flipped) = pixels;
        copy_rows(buffer, &pixels, size.w, size.h, flipped)
    }

    /// Tries frames waiting for a new output frame or window commit.
    pub fn schedule_capture_flush(&mut self) {
        if self.capture.pending.is_empty()
            || std::mem::replace(&mut self.capture.flush_scheduled, true)
        {
            return;
        }
        self.loop_handle.insert_idle(|state| {
            state.capture.flush_scheduled = false;
            let pending = std::mem::take(&mut state.capture.pending);
            for (session, frame) in pending {
                if frame.has_failed() {
                    continue;
                }
                state.capture_now(&session, frame);
            }
        });
    }

    /// Stops sessions whose source is no longer available.
    ///
    /// This covers unplugged outputs and closed windows.
    pub fn stop_dead_captures(&mut self) {
        if self.capture.sessions.is_empty() {
            return;
        }
        let sessions = std::mem::take(&mut self.capture.sessions);
        let (live, dead): (Vec<Session>, Vec<Session>) = sessions
            .into_iter()
            .partition(|s| self.live_target(&s.source()).is_some());
        self.capture
            .pending
            .retain(|(s, _)| !dead.iter().any(|d| *d == *s));
        self.capture.sessions = live;
        for session in dead {
            session.stop();
        }
    }
}

fn target(source: &ImageCaptureSource) -> Option<&Target> {
    source.user_data().get::<Target>()
}

/// Copies tightly packed four byte rows into a shared memory buffer.
///
/// The destination offset and stride are respected and rows can be flipped.
fn copy_rows(
    buffer: &WlBuffer,
    pixels: &[u8],
    width: i32,
    height: i32,
    flipped: bool,
) -> Result<(), String> {
    let _span = tracy_client::span!("capture_copy");
    let row = width as usize * 4;
    if pixels.len() < row * height as usize {
        return Err("short read-back".into());
    }
    with_buffer_contents_mut(buffer, |ptr, len, data| {
        let stride = data.stride as usize;
        let offset = data.offset as usize;
        let rows = (data.height.min(height)) as usize;
        let copy = row.min(data.width as usize * 4).min(stride);
        if offset + stride * rows.saturating_sub(1) + copy > len {
            return Err("buffer too small".to_string());
        }
        for y in 0..rows {
            let src_row = if flipped { height as usize - 1 - y } else { y };
            let src = &pixels[src_row * row..src_row * row + copy];
            // SAFETY the range was checked against the pool length above.
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), ptr.add(offset + y * stride), copy);
            }
        }
        Ok(())
    })
    .map_err(|e| format!("buffer access: {e:?}"))?
}
