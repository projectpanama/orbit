use smithay::{
    reexports::wayland_server::protocol::wl_buffer,
    wayland::{buffer::BufferHandler, compositor::CompositorHandler},
};

use crate::state::Orbit;

impl BufferHandler for Orbit {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl CompositorHandler for Orbit {
    fn compositor_state(&mut self) -> &mut smithay::wayland::compositor::CompositorState {
        todo!()
    }

    fn client_compositor_state<'a>(
        &self,
        client: &'a smithay::reexports::wayland_server::Client,
    ) -> &'a smithay::wayland::compositor::CompositorClientState {
        todo!()
    }

    fn commit(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        todo!()
    }
}
