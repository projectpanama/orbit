use smithay::{
    backend::{
        drm::DrmNode,
        egl::EGLDevice,
        renderer::{ImportDma, damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitGraphicsBackend},
    },
    output::{Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::EventLoop,
        winit::{
            dpi::LogicalSize,
            platform::{wayland::WindowAttributesWayland, x11::WindowAttributesX11},
            window::{PlatformWindowAttributes, WindowAttributes},
        },
    },
    utils::Transform,
    wayland::dmabuf::DmabufGlobal,
};

use crate::state::Orbit;

pub struct Winit {
    backend: WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    damage_tracker: OutputDamageTracker,
    dmabuf_global: DmabufGlobal,
}

impl Winit {
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        self.backend.renderer()
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

    event_loop.handle();

    state.backend = Some(super::Backend::Winit(Box::new(Winit {
        damage_tracker: OutputDamageTracker::from_output(&output),
        dmabuf_global,
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
