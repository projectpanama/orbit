use orbit_config::ModKey;
use orbit_core::frame::FrameOutcome;
use smithay::{
    backend::{
        SwapBuffersError,
        drm::DrmNode,
        egl::EGLDevice,
        renderer::{
            ImportDma,
            damage::{Error as OutputDamageTrackerError, OutputDamageTracker},
            element::RenderElementStates,
            gles::{GlesError, GlesRenderer},
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::utils::OutputPresentationFeedback,
    input::pointer::{CursorIcon, CursorImageStatus},
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::EventLoop,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        winit::{
            cursor::Cursor,
            dpi::LogicalSize,
            platform::{wayland::WindowAttributesWayland, x11::WindowAttributesX11},
            window::{PlatformWindowAttributes, WindowAttributes},
        },
    },
    utils::{Physical, Size, Transform},
    wayland::{dmabuf::DmabufGlobal, presentation::Refresh},
};

use crate::{outputs::OutputSetup, render::OrbitRenderElement, state::Orbit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCursor {
    /// A cursor icon supplied by the host window.
    Named(CursorIcon),
    /// The host cursor is hidden because the compositor draws its own cursor.
    Hidden,
}

/// Errors that can occur while drawing or submitting a frame.
#[derive(Debug)]
enum DrawError {
    /// The renderer could not bind the host surface.
    Bind(SwapBuffersError),
    /// Rendering the output damage failed.
    Render(OutputDamageTrackerError<GlesError>),
    /// The rendered damage could not be submitted.
    Submit(SwapBuffersError),
}

/// Result of drawing one frame before presentation.
struct DrawnFrame {
    /// Render state used for presentation feedback.
    states: RenderElementStates,
    /// Whether damage was submitted to the host window.
    submitted: bool,
}

/// Nested backend state for a winit host window.
pub struct Winit {
    /// Winit graphics backend and host window.
    backend: WinitGraphicsBackend<GlesRenderer>,
    /// Output represented by the host window.
    output: Output,
    /// Tracks damage for the output.
    damage_tracker: OutputDamageTracker,
    /// Dmabuf global used by compatible clients.
    dmabuf_global: DmabufGlobal,
    /// Cursor currently shown by the host window.
    host_cursor: Option<HostCursor>,
    /// Render elements built for the current frame.
    elements: Vec<OrbitRenderElement<GlesRenderer>>,
    /// Presentation feedback waiting for host redraw.
    pending_feedback: Option<OutputPresentationFeedback>,
    /// Tracks consecutive frame failures.
    failures: super::FailedFrames,
}

impl Winit {
    /// Returns mutable access to the GLES renderer.
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        self.backend.renderer()
    }

    /// Renders and submits one frame for the nested output.
    ///
    /// Presentation feedback is completed when the host requests the next redraw.
    pub fn render(&mut self, state: &mut Orbit, output: &Output) -> FrameOutcome {
        debug_assert!(*output == self.output);
        let _span = tracy_client::span!("render");
        self.update_host_cursor(&state.input.cursor_status);

        let frame = match self.draw(state) {
            Ok(frame) => frame,
            Err(err) => {
                self.failures.failed(&output.name(), &err);
                state.post_render(output, None);
                return FrameOutcome::Failed;
            }
        };

        self.failures.succeeded(&output.name());
        state.post_render(output, Some(&frame.states));

        if !frame.submitted {
            return FrameOutcome::NoDamage;
        }

        // Complete this feedback when the host has shown the frame.
        // Replacing older feedback tells its clients it was superseded.
        self.pending_feedback = Some(state.presentation_feedback(output, &frame.states));
        // Ask the host for a redraw event after presentation.
        // The redraw event completes the frame.
        self.backend.window().request_redraw();

        FrameOutcome::Submitted
    }

    /// Draws the current output using the damage tracker.
    fn draw(&mut self, state: &mut Orbit) -> Result<DrawnFrame, DrawError> {
        // Query the back buffer age before binding the backend.
        // EGL reports the age only while the host surface is current.
        // An unknown age causes a complete redraw.
        let age = if self.backend.egl_surface().is_current() {
            self.backend.buffer_age().unwrap_or(0)
        } else {
            0
        };
        let (renderer, mut framebuffer) = self.backend.bind().map_err(DrawError::Bind)?;
        state.render_elements(renderer, &self.output, &mut self.elements);

        let damage_span = tracy_client::span!("render_output");
        let result = self.damage_tracker.render_output(
            renderer,
            &mut framebuffer,
            age,
            &self.elements,
            [0.08, 0.09, 0.12, 1.0],
        );

        drop(damage_span);
        drop(framebuffer);
        self.elements.clear();

        let result = result.map_err(DrawError::Render)?;
        let submitted = match result.damage {
            Some(damage) => {
                self.backend
                    .submit(Some(damage))
                    .map_err(DrawError::Submit)?;
                tracy_client::frame_mark();
                true
            }
            None => false,
        };

        Ok(DrawnFrame {
            states: result.states,
            submitted,
        })
    }

    /// Updates the host cursor to match compositor cursor state.
    fn update_host_cursor(&mut self, status: &CursorImageStatus) {
        let wanted = match status {
            CursorImageStatus::Named(icon) => HostCursor::Named(*icon),
            CursorImageStatus::Hidden | CursorImageStatus::Surface(_) => HostCursor::Hidden,
        };
        if self.host_cursor == Some(wanted) {
            return;
        }
        let window = self.backend.window();
        match wanted {
            HostCursor::Named(icon) => {
                window.set_cursor(Cursor::Icon(icon));
                window.set_cursor_visible(true);
            }
            HostCursor::Hidden => window.set_cursor_visible(false),
        }
        self.host_cursor = Some(wanted);
    }
}

impl Orbit {
    /// Returns the configured nested scale or the host scale.
    pub fn nested_scale(&self, output: &Output, host: f64) -> f64 {
        self.output_settings(&output.name(), &output.description())
            .scale
            .unwrap_or(host)
    }

    /// Updates the nested output after the host window changes size.
    fn on_window_resized(&mut self, size: Size<i32, Physical>, scale_factor: f64) {
        let Some(winit) = self.backend.as_mut().and_then(super::Backend::winit) else {
            return;
        };
        let output = winit.output.clone();
        let scale = self.nested_scale(&output, scale_factor);
        if sync_output(&output, size, scale) {
            self.relayout_outputs();
        }
        self.queue_redraw(&output);
    }

    /// Completes presentation after the host requests a redraw.
    fn on_host_redraw(&mut self) {
        let Some(winit) = self.backend.as_mut().and_then(super::Backend::winit) else {
            return;
        };
        let output = winit.output.clone();
        if self
            .outputs
            .get(&output)
            .is_some_and(|state| state.frame.is_in_flight())
        {
            let now = self.clock.now();
            if let Some(mut feedback) = winit.pending_feedback.take() {
                // The host does not report refresh rate or vblank status.
                feedback.presented(
                    now,
                    Refresh::Unknown,
                    0,
                    wp_presentation_feedback::Kind::empty(),
                );
            }
            self.record_presented(&output, now.into());
            self.frame_presented(&output);
        } else {
            self.queue_redraw(&output);
        }
    }
}

/// Initializes the nested winit backend and its output.
pub fn init(event_loop: &mut EventLoop<Orbit>, state: &mut Orbit) -> anyhow::Result<()> {
    let attributes = WindowAttributes::default()
        .with_surface_size(LogicalSize::new(1280.0, 800.0))
        .with_title("Orbit (nested)")
        .with_platform_attributes(platform_attributes());

    let (mut backend, winit) = winit::init_from_attributes::<GlesRenderer>(attributes)
        .map_err(|err| anyhow::anyhow!("could not open the nested window: {err}"))?;

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Orbit".into(),
            model: "Nested".into(),
            serial_number: "Unknown".into(),
        },
    );
    output.change_current_state(None, Some(Transform::Flipped180), None, None);

    let scale = state.nested_scale(&output, backend.scale_factor());
    sync_output(&output, backend.window_size(), scale);

    // Hardware accelerated clients share buffers through dmabuf.
    let renderer = backend.renderer();
    let dmabuf_global =
        super::dmabuf::create_global(state, renderer.dmabuf_formats(), render_node(renderer));

    // The host desktop usually reserves the Super key.
    state.input.mod_key = state.config.mod_key.unwrap_or(ModKey::Alt);

    event_loop
        .handle()
        .insert_source(winit, |event, _, state| match event {
            WinitEvent::Focus(_) => {}
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => state.on_host_redraw(),
            WinitEvent::Resized { size, scale_factor } => {
                state.on_window_resized(size, scale_factor)
            }
        })
        .map_err(|err| err.error)?;

    state.add_output(&output, OutputSetup::default());

    state.backend = Some(super::Backend::Winit(Box::new(Winit {
        damage_tracker: OutputDamageTracker::from_output(&output),
        dmabuf_global,
        host_cursor: None,
        elements: Vec::new(),
        pending_feedback: None,
        failures: super::FailedFrames::default(),
        backend,
        output,
    })));

    Ok(())
}

/// Selects host window attributes for Wayland or X11.
fn platform_attributes() -> Box<dyn PlatformWindowAttributes> {
    let set = |var| std::env::var_os(var).is_some_and(|value| !value.is_empty());
    if set("WAYLAND_DISPLAY") || set("WAYLAND_SOCKET") {
        Box::new(WindowAttributesWayland::default().with_name("orbit", ""))
    } else {
        Box::new(WindowAttributesX11::default().with_name("orbit", "orbit"))
    }
}

/// Returns the render node associated with the EGL display.
fn render_node(renderer: &GlesRenderer) -> Option<DrmNode> {
    let display = renderer.egl_context().display();
    match EGLDevice::device_for_display(display).and_then(|device| device.try_get_render_node()) {
        Ok(node) => node,
        Err(err) => {
            tracing::debug!(?err, "failed to find the EGL device's render node");
            None
        }
    }
}

/// Synchronizes output mode and scale with the host window.
///
/// Returns true when the advertised output state changed.
fn sync_output(output: &Output, size: Size<i32, Physical>, scale_factor: f64) -> bool {
    let mode = Mode {
        size,
        refresh: 60_000,
    };
    let old_mode = output.current_mode();
    let changed =
        old_mode != Some(mode) || output.current_scale().fractional_scale() != scale_factor;
    if !changed {
        return false;
    }

    // Remove the previous mode so old host sizes are not advertised.
    if let Some(old_mode) = old_mode
        && old_mode != mode
    {
        output.delete_mode(old_mode);
    }
    output.change_current_state(
        Some(mode),
        None,
        Some(Scale::Fractional(scale_factor)),
        None,
    );
    output.set_preferred(mode);
    true
}
