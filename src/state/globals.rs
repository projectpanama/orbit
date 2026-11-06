use smithay::{
    reexports::wayland_server::DisplayHandle,
    utils::{ClockSource, Monotonic},
    wayland::{
        cursor_shape::CursorShapeManagerState, fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState, input_method::InputMethodManagerState,
        output::OutputManagerState, presentation::PresentationState,
        shell::xdg::decoration::XdgDecorationState, single_pixel_buffer::SinglePixelBufferState,
        text_input::TextInputManagerState, viewporter::ViewporterState,
    },
};

use crate::state::Orbit;

pub struct Globals {
    output_manager: OutputManagerState,
    presentation: PresentationState,
    cursor_shape: CursorShapeManagerState,
    fractional_scale: FractionalScaleManagerState,
    viewporter: ViewporterState,
    single_pixel_buffer: SinglePixelBufferState,
    xdg_decoration: XdgDecorationState,
    idle_inhibit: IdleInhibitManagerState,
    text_input: TextInputManagerState,
    input_method: InputMethodManagerState,
}

impl Globals {
    pub fn new(display_handle: &DisplayHandle) -> Self {
        Self {
            output_manager: OutputManagerState::new_with_xdg_output::<Orbit>(display_handle),
            presentation: PresentationState::new::<Orbit>(display_handle, Monotonic::ID as u32),
            cursor_shape: CursorShapeManagerState::new::<Orbit>(display_handle),
            fractional_scale: FractionalScaleManagerState::new::<Orbit>(display_handle),
            viewporter: ViewporterState::new::<Orbit>(display_handle),
            single_pixel_buffer: SinglePixelBufferState::new::<Orbit>(display_handle),
            xdg_decoration: XdgDecorationState::new::<Orbit>(display_handle),
            idle_inhibit: IdleInhibitManagerState::new::<Orbit>(display_handle),
            text_input: TextInputManagerState::new::<Orbit>(display_handle),
            input_method: InputMethodManagerState::new::<Orbit, _>(
                display_handle,
                crate::protocols::privileged,
            ),
        }
    }
}
