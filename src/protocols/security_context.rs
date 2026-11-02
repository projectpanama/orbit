use std::sync::Arc;

use smithay::wayland::security_context::{
    SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
};

use crate::state::{Orbit, client::ClientState};

impl SecurityContextHandler for Orbit {
    /// Registers a listener for clients created by a security context.
    ///
    /// Each accepted client receives the context metadata in its [`ClientState`],
    /// allowing privileged-global checks to identify it as sandboxed.
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        tracing::debug!(
            sandbox = ?context.sandbox_engine,
            app_id = ?context.app_id,
            "security context created"
        );
        let result = self
            .loop_handle
            .insert_source(source, move |stream, _, state| {
                let data = ClientState {
                    security_context: Some(context.clone()),
                    ..ClientState::default()
                };
                if let Err(err) = state.display_handle.insert_client(stream, Arc::new(data)) {
                    tracing::warn!(?err, "failed to insert sandboxed client");
                }
            });
        if let Err(err) = result {
            tracing::warn!(%err, "failed to listen on a security context socket");
        }
    }
}
