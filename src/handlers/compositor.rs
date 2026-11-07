use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    reexports::wayland_server::{
        Client,
        protocol::{wl_buffer, wl_surface::WlSurface},
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface,
        },
        shm::{ShmHandler, ShmState},
    },
};

use crate::{
    state::{Orbit, client::ClientState},
    workspaces::window_id,
};

impl CompositorHandler for Orbit {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Client state is added by the listener, security context handler,
        // or test fixture.
        &client
            .get_data::<ClientState>()
            .expect("client inserted without a ClientState")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Only outputs showing the surface can change.
        // The old geometry is still available before commit processing.
        self.queue_redraw_surface(surface);
        on_commit_buffer_handler::<Self>(surface);

        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        // Surface to output membership is refreshed after the next frame.
        let window = self.window_for_surface(&root);
        match window.as_ref().and_then(window_id) {
            Some(id) => self.space_refresh.window_committed(id),
            None => self.space_refresh.refresh_everything(),
        }

        if !is_sync_subsurface(surface)
            && let Some(window) = window
        {
            window.on_commit();
            self.foreign_toplevel_committed(&window);

            // Window captures waiting for new content can use this commit.
            self.schedule_capture_flush();
        }

        crate::window::grabs::handle_resize_commit(self, surface);
        self.handle_layer_commit(surface);

        let window = self.window_for_surface(surface);
        super::xdg_shell::handle_commit(&mut self.popups, window.as_ref(), surface);
        self.queue_redraw_surface(surface);
    }

    fn destroyed(&mut self, _surface: &WlSurface) {
        // A destroyed surface disappears without a commit.
        self.queue_redraw_all();
        self.space_refresh.refresh_everything();

        // Free its textures even when no frame is drawn next.
        self.schedule_cleanup();
    }
}

impl BufferHandler for Orbit {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Orbit {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
