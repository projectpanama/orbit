mod dmabuf;

use smithay::backend::allocator::dmabuf::Dmabuf;

/// TODO: Implement TTY, Winit and Headless backends
pub enum Backend {}

impl Backend {
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        // TODO: We need to fetch the renderer and check if it can import the dmabuf, but for now we just return true.
        true
    }
}
