use smithay::{
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy, Window,
        find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{
        Seat,
        pointer::{Focus, GrabStartData},
    },
    output::Output,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Resource,
            protocol::{wl_output::WlOutput, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Point, Serial},
    wayland::shell::xdg::{
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    },
};

use crate::{
    state::Orbit,
    window::grabs::Edges,
    window::{OpenAnimation, with_window_state},
    workspaces::window_id,
};

/// Offset between successively opened windows so they don't stack exactly.
const CASCADE_STEP: i32 = 32;

impl XdgShellHandler for Orbit {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });

        // Open on the focused output below its panels.
        // Leave room for a server side title bar and cascade new windows.
        let window = Window::new_wayland_window(surface.clone());
        let style = self.decoration_style.clone();
        let titlebar = style.height();
        with_window_state(&window, |state| state.style = style);
        let timing = self.config.animations.window_open;
        if !timing.is_off() {
            with_window_state(&window, |state| {
                state.open_animation = OpenAnimation::Waiting(timing);
            });
        }
        let visible = self.register_window(&window);
        let output = self.window_output(&window);
        let area = output
            .as_ref()
            .and_then(|output| self.usable_geometry(output));
        let siblings = output
            .as_ref()
            .and_then(|output| self.output_id(output))
            .map_or(0, |id| {
                self.workspaces.active_windows(id).len().saturating_sub(1)
            });
        let offset = (siblings as i32 % 10) * CASCADE_STEP;
        let placement = Point::from((offset, offset + titlebar));

        if visible {
            let origin = area.map(|area| area.loc).unwrap_or_default();
            self.space
                .map_element(window.clone(), origin + placement, true);
            self.focus_window(Some(&window), Serial::from(0));
        } else {
            // Save the position until an output becomes available.
            with_window_state(&window, |state| state.hidden_at = Some((placement, 0)));
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.space.unmap_elem(&window);
            self.unregister_window(&window);
        }
        self.toplevel_states_changed();

        // Restore focus when the destroyed window held keyboard focus.
        if self.focused_window().is_none()
            && let Some(id) = self
                .focused_output()
                .and_then(|output| self.output_id(&output))
        {
            self.refocus_output(id);
        }
        self.queue_redraw_all();
    }

    fn title_changed(&mut self, _surface: ToplevelSurface) {
        self.desktop.toplevels_dirty = true;
    }

    fn app_id_changed(&mut self, _surface: ToplevelSurface) {
        self.desktop.toplevels_dirty = true;
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.queue_redraw_all();
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
        });
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(?err, "failed to track popup");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        // Ignore a seat resource that is no longer valid.
        let Some(seat) = Seat::from_resource(&seat) else {
            return;
        };
        let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial) else {
            return;
        };
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.start_move(window, start_data, serial);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        // Ignore a seat resource that is no longer valid.
        let Some(seat) = Seat::from_resource(&seat) else {
            return;
        };
        let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial) else {
            return;
        };
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.start_resize(window, start_data, Edges::from_xdg(edges), serial);
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.maximize(&window);
            self.queue_redraw_all();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.unmaximize(&window);
            self.queue_redraw_all();
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            // Without an output preference, keep the current output.
            if let Some(output) = output.as_ref().and_then(Output::from_resource) {
                self.move_window_to_output(&window, &output);
            }
            self.fullscreen(&window);
            self.queue_redraw_all();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.unfullscreen(&window);
            self.queue_redraw_all();
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.minimize(&window);
            self.queue_redraw_all();
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let mut grab = match self.popups.grab_popup(root, popup, &seat, serial) {
            Ok(grab) => grab,
            Err(err) => {
                tracing::debug!(?err, "denied popup grab");
                return;
            }
        };

        // A popup may grab only in response to the input holding the grab.
        // It may also extend an existing popup grab.
        let previous_serial = grab.previous_serial().unwrap_or(serial);

        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(previous_serial))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }

        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial) || pointer.has_grab(previous_serial))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }
}

impl Orbit {
    /// Raises a window, marks it active, and gives it keyboard focus.
    ///
    /// Passing None clears window focus. The window output becomes focused
    /// and remembers the window for later workspace focus.
    pub fn focus_window(&mut self, window: Option<&Window>, serial: Serial) {
        if let Some(window) = window {
            self.space.raise_element(window, true);
            with_window_state(window, |state| state.urgent = false);
            if let Some(id) = window_id(window) {
                self.workspaces.focused(id);
                if let Some(output) = self.workspaces.output_of(id) {
                    self.focused_output = Some(output);
                }
            }
        }
        // Update hidden and minimized windows so none remains active when
        // it becomes visible again.
        for w in self.windows.values() {
            w.set_activated(Some(w) == window);
            if let Some(toplevel) = w.toplevel()
                && toplevel.is_initial_configure_sent()
            {
                toplevel.send_pending_configure();
            }
        }

        // The lock screen keeps the keyboard until unlock.
        if self.session_lock.is_active() {
            self.focus_lock_surface();
            return;
        }
        // An exclusive layer surface such as a launcher keeps the keyboard
        // until it closes.
        let focus = self.exclusive_layer().or_else(|| {
            window
                .and_then(|w| w.toplevel())
                .map(|t| t.wl_surface().clone())
        });
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, focus, serial);
    }

    /// Keeps a popup inside the output containing its parent window.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self
            .window_for_surface(&root)
            .filter(|window| self.space.element_location(window).is_some())
        else {
            self.unconstrain_layer_popup(popup);
            return;
        };
        let Some(output_geo) = self
            .output_under_window(&window)
            .and_then(|output| self.space.output_geometry(&output))
        else {
            return;
        };
        let Some(window_geo) = self.space.element_geometry(&window) else {
            return;
        };

        // Express the output rectangle relative to the popup parent surface.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

/// Validates a client move or resize request.
///
/// The request must come from the pointer press holding the implicit grab on
/// the same client surface.
fn check_grab(
    seat: &Seat<Orbit>,
    surface: &WlSurface,
    serial: Serial,
) -> Option<GrabStartData<Orbit>> {
    let pointer = seat.get_pointer()?;
    if !pointer.has_grab(serial) {
        return None;
    }
    let start_data = pointer.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }
    Some(start_data)
}

/// Sends initial configures after the first toplevel or popup commit.
pub fn handle_commit(popups: &mut PopupManager, window: Option<&Window>, surface: &WlSurface) {
    // Hidden workspace windows can receive this before their first commit.
    if let Some(toplevel) = window.and_then(Window::toplevel)
        && toplevel.wl_surface() == surface
        && !toplevel.is_initial_configure_sent()
    {
        toplevel.send_configure();
    }

    popups.commit(surface);
    if let Some(PopupKind::Xdg(popup)) = popups.find_popup(surface)
        && !popup.is_initial_configure_sent()
    {
        // Failure means the popup parent is already gone.
        if let Err(err) = popup.send_configure() {
            tracing::warn!(?err, "failed to send initial popup configure");
        }
    }
}
