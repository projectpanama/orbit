use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    desktop::Window,
    reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    wayland::{
        compositor::with_states,
        foreign_toplevel_list::{
            ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
        },
        shell::xdg::XdgToplevelSurfaceData,
    },
};

use orbit_core::ids::WindowId;

use crate::state::Orbit;

impl ForeignToplevelListHandler for Orbit {
    /// Returns Orbit's foreign-toplevel protocol state.
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.desktop.foreign_toplevels
    }
}

impl Orbit {
    /// Synchronizes foreign-toplevel handles with Orbit's mapped windows.
    ///
    /// New mapped windows are announced, changed titles and app IDs are sent,
    /// and handles for removed or unmapped windows are closed. Does nothing
    /// until the dirty flag is set.
    pub fn refresh_foreign_toplevels(&mut self) {
        if !std::mem::take(&mut self.desktop.toplevels_dirty) {
            return;
        }

        let desktop = &mut self.desktop;
        let windows = &self.windows;

        desktop.toplevel_handles.retain(|id, handle| {
            let keep = windows.get(id).is_some_and(is_mapped);
            if !keep {
                desktop.foreign_toplevels.remove_toplevel(handle);
            }
            keep
        });

        for (id, window) in windows {
            if let Some(handle) = desktop.toplevel_handles.get(id) {
                with_title_and_app_id(window, |title, app_id| {
                    let mut changed = false;
                    if handle.title() != title {
                        handle.send_title(title);
                        changed = true;
                    }
                    if handle.app_id() != app_id {
                        handle.send_app_id(app_id);
                        changed = true;
                    }
                    if changed {
                        handle.send_done();
                    }
                });
            } else if is_mapped(window) {
                let (title, app_id) = with_title_and_app_id(window, |title, app_id| {
                    (title.to_owned(), app_id.to_owned())
                })
                .unwrap_or_default();
                let handle = desktop
                    .foreign_toplevels
                    .new_toplevel::<Orbit>(title, app_id);
                // Lets a request on the handle find its window directly.
                handle.user_data().insert_if_missing(|| *id);
                desktop.toplevel_handles.insert(*id, handle);
            }
        }
    }

    /// Marks the foreign-toplevel list dirty when mapping changes.
    ///
    /// This is called for every surface commit but avoids a full list scan
    /// unless the window crossed the mapped/unmapped boundary.
    pub fn foreign_toplevel_committed(&mut self, window: &Window) {
        let Some(id) = crate::workspaces::window_id(window) else {
            return;
        };
        if is_mapped(window) != self.desktop.toplevel_handles.contains_key(&id) {
            self.desktop.toplevels_dirty = true;
        }
    }

    /// Resolves a protocol handle to its live window, if any.
    ///
    /// Returns `None` for invalid, closed or already-removed handles.
    pub fn window_for_foreign_toplevel(
        &self,
        resource: &ExtForeignToplevelHandleV1,
    ) -> Option<Window> {
        let handle = ForeignToplevelHandle::from_resource(resource)?;
        self.window_for_foreign_handle(&handle)
    }

    /// Resolves an internal foreign-toplevel handle to its live window.
    ///
    /// Returns `None` when the handle is closed, lacks window identity data,
    /// or no longer refers to a known window.
    pub fn window_for_foreign_handle(&self, handle: &ForeignToplevelHandle) -> Option<Window> {
        if handle.is_closed() {
            return None;
        }
        let id = handle.user_data().get::<WindowId>()?;
        self.windows.get(id).cloned()
    }
}

/// Calls `f` with a window's title and app ID.
///
/// Unset values are passed as empty strings. Returns `None` when the window
/// has no xdg-toplevel data or its state cannot be accessed.
pub fn with_title_and_app_id<T>(window: &Window, f: impl FnOnce(&str, &str) -> T) -> Option<T> {
    let toplevel = window.toplevel()?;
    with_states(toplevel.wl_surface(), |states| {
        let data = states
            .data_map
            .get::<XdgToplevelSurfaceData>()?
            .lock()
            .ok()?;
        Some(f(
            data.title.as_deref().unwrap_or_default(),
            data.app_id.as_deref().unwrap_or_default(),
        ))
    })
}

/// Returns whether `window` has committed a buffer and is mapped.
fn is_mapped(window: &Window) -> bool {
    window.toplevel().is_some_and(|toplevel| {
        with_renderer_surface_state(toplevel.wl_surface(), |state| state.buffer().is_some())
            .unwrap_or(false)
    })
}
