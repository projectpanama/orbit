use smithay::{reexports::wayland_server::protocol::wl_buffer, wayland::buffer::BufferHandler};

use crate::state::Orbit;

impl BufferHandler for Orbit {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}
