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
    output::{Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::EventLoop,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        winit::{
            dpi::LogicalSize,
            platform::{wayland::WindowAttributesWayland, x11::WindowAttributesX11},
            window::{PlatformWindowAttributes, WindowAttributes},
        },
    },
    utils::Transform,
    wayland::{dmabuf::DmabufGlobal, presentation::Refresh},
};

use crate::{render::OrbitRenderElement, state::Orbit};

#[derive(Debug)]
enum DrawError {
    Bind(SwapBuffersError),
    Render(OutputDamageTrackerError<GlesError>),
    Submit(SwapBuffersError),
}

struct DrawnFrame {
    states: RenderElementStates,
    submitted: bool,
}

pub struct Winit {
    backend: WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    damage_tracker: OutputDamageTracker,
    dmabuf_global: DmabufGlobal,
    elements: Vec<OrbitRenderElement<GlesRenderer>>,
    pending_feedback: Option<OutputPresentationFeedback>,
    failures: super::FailedFrames,
}

impl Winit {
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        self.backend.renderer()
    }

    pub fn render(&mut self, state: &mut Orbit, output: &Output) -> FrameOutcome {
        debug_assert!(*output == self.output);
        let _span = tracy_client::span!("render");

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

        self.pending_feedback = Some(state.presentation_feedback(output, &frame.states));
        self.backend.window().request_redraw();
        FrameOutcome::Submitted
    }

    fn draw(&mut self, state: &mut Orbit) -> Result<DrawnFrame, DrawError> {
        let age = if self.backend.egl_surface().is_current() {
            self.backend.buffer_age().unwrap_or(0)
        } else {
            0
        };

        let (renderer, mut framebuffer) = self.backend.bind().map_err(DrawError::Bind)?;

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
}

impl Orbit {
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
                feedback.presented(
                    now,
                    Refresh::Unknown,
                    0,
                    wp_presentation_feedback::Kind::empty(),
                );
            }
        } else {
            self.queue_redraw(&output);
        }
    }
}

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

    let renderer = backend.renderer();
    let dmabuf_global =
        super::dmabuf::create_global(state, renderer.dmabuf_formats(), render_node(renderer));

    event_loop
        .handle()
        .insert_source(winit, |event, _, state| match event {
            WinitEvent::Focus(_) => {}
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            WinitEvent::Input(_) => {}
            WinitEvent::Redraw => state.on_host_redraw(),
            WinitEvent::Resized { size, scale_factor } => {}
        })
        .map_err(|err| err.error)?;

    state.backend = Some(super::Backend::Winit(Box::new(Winit {
        damage_tracker: OutputDamageTracker::from_output(&output),
        dmabuf_global,
        elements: Vec::new(),
        pending_feedback: None,
        failures: super::FailedFrames::default(),
        backend,
        output,
    })));

    Ok(())
}

fn platform_attributes() -> Box<dyn PlatformWindowAttributes> {
    let set = |var| std::env::var_os(var).is_some_and(|value| !value.is_empty());
    if set("WAYLAND_DISPLAY") || set("WAYLAND_SOCKET") {
        Box::new(WindowAttributesWayland::default().with_name("orbit", ""))
    } else {
        Box::new(WindowAttributesX11::default().with_name("orbit", "orbit"))
    }
}

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
