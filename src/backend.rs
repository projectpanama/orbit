mod dmabuf;

pub mod winit;

use smithay::backend::allocator::dmabuf::Dmabuf;

use crate::backend::winit::Winit;

/// TODO: Implement TTY and Headless backends
pub enum Backend {
    Winit(Box<Winit>),
}

impl Backend {
    pub fn winit(&mut self) -> Option<&mut Winit> {
        match self {
            Backend::Winit(winit) => Some(winit),
            _ => None,
        }
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        // TODO: We need to fetch the renderer and check if it can import the dmabuf, but for now we just return true.
        true
    }
}
