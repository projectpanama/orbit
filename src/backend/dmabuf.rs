//! DMA-BUF protocol support for the compositor backend.
//!
//! The compositor advertises DMA-BUF formats through Smithay and uses the
//! available DRM render node to provide explicit feedback when possible.

use smithay::{
    backend::{
        allocator::{dmabuf::Dmabuf, format::FormatSet},
        drm::DrmNode,
    },
    wayland::dmabuf::{
        DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
    },
};

use crate::state::Orbit;

/// Creates a DMA-BUF global for the compositor, advertising the supported formats.
///
/// When a render node is available and Smithay can build feedback for it, the
/// global advertises that feedback to clients. If no render node is known or
/// feedback creation fails, the compositor falls back to the basic DMA-BUF
/// protocol global using the supplied formats.
///
/// The formats are also used by the fallback path, so callers should provide
/// every DMA-BUF format that the active renderer can import.
pub fn create_global(
    state: &mut Orbit,
    formats: FormatSet,
    render_node: Option<DrmNode>,
) -> DmabufGlobal {
    if let Some(node) = render_node {
        match DmabufFeedbackBuilder::new(node.dev_id(), formats.clone()).build() {
            Ok(feedback) => {
                tracing::debug!(%node, "advertising dmabuf with default feedback");
                return state
                    .dmabuf_state
                    .create_global_with_default_feedback::<Orbit>(
                        &state.display_handle,
                        &feedback,
                    );
            }
            Err(err) => {
                tracing::debug!(%node, %err, "failed to build dmabuf feedback, falling back to v3");
            }
        }
    } else {
        tracing::debug!("no render node known, advertising dmabuf v3");
    }

    state
        .dmabuf_state
        .create_global::<Orbit>(&state.display_handle, formats)
}

impl DmabufHandler for Orbit {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // Accept only buffers the renderer can use, so clients
        // knows about unsupported imports before rendering fails.
        let imported = self
            .backend
            .as_mut()
            .is_some_and(|backend| backend.import_dmabuf(&dmabuf));

        if imported {
            let _ = notifier.successful::<Orbit>();
        } else {
            notifier.failed();
        }
    }
}
