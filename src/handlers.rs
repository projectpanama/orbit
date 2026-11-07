mod compositor;
mod xdg_shell;

use std::time::Duration;

use smithay::{
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::{CursorImageStatus, Focus},
        tablet::TabletSeatHandler,
    },
    output::Output,
    reexports::wayland_server::{
        Resource,
        protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    },
    utils::Serial,
    wayland::{
        output::OutputHandler,
        pointer_constraints::PointerConstraintsHandler,
        selection::{
            SelectionHandler,
            data_device::{
                DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
            },
            primary_selection::{
                PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
            },
        },
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};

use crate::state::Orbit;

impl OutputHandler for Orbit {
    fn output_bound(&mut self, output: Output, wl_output: WlOutput) {
        self.ext_workspace_output_bound(&output, &wl_output);
    }
}

impl SeatHandler for Orbit {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Keep clipboard offers going to the client that has keyboard focus.
        let client = focused.and_then(|s| self.display_handle.get_client(s.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
        self.toplevel_states_changed();
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.input.cursor_status = image;
        // Smithay calls this with the pointer locked (e.g. mid-motion), so
        // its location can only be read afterwards.
        self.loop_handle
            .insert_idle(|state| state.queue_redraw_pointer());
    }
}

impl TabletSeatHandler for Orbit {
    type ToolFocus = WlSurface;
}

impl PointerConstraintsHandler for Orbit {}

impl DataDeviceHandler for Orbit {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl SelectionHandler for Orbit {
    type SelectionUserData = ();
}

impl PrimarySelectionHandler for Orbit {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

impl WaylandDndGrabHandler for Orbit {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        // Orbit has no touch input yet and doesn't draw drag icons.
        let GrabType::Pointer = type_ else {
            source.cancel();
            return;
        };
        let Some(pointer) = seat.get_pointer() else {
            source.cancel();
            return;
        };
        let Some(start_data) = pointer.grab_start_data() else {
            source.cancel();
            return;
        };
        let grab = DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
        pointer.set_grab(self, grab, serial, Focus::Keep);
    }
}

impl DndGrabHandler for Orbit {}

impl XdgActivationHandler for Orbit {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if let Some(window) = self.window_for_surface(&surface) {
            // Focus may only be handed over by the client that has it, in response to
            // recent input. The token must carry a serial from our seat that isn't older
            // than the current keyboard focus. Anything else just asks for attention.
            let keyboard = self.keyboard.clone();
            let allowed = token_data.timestamp.elapsed() < Duration::from_secs(10)
                && token_data.serial.as_ref().is_some_and(|(serial, seat)| {
                    Seat::<Self>::from_resource(seat).as_ref() == Some(&self.seat)
                        && keyboard
                            .last_enter()
                            .is_some_and(|enter| serial.is_no_older_than(&enter))
                });

            if allowed {
                self.activate_window(&window);
            } else {
                self.mark_urgent(&window);
            }
            self.queue_redraw_all();
        }
        self.xdg_activation_state.remove_token(&token);
    }
}
