use smithay::{
    desktop::{WindowSurfaceType, layer_map_for_output},
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    wayland::{
        compositor::get_parent,
        idle_inhibit::IdleInhibitHandler,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
    },
};

use crate::state::Orbit;

impl IdleNotifierHandler for Orbit {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier
    }
}

impl IdleInhibitHandler for Orbit {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.insert(surface);
        self.refresh_idle_inhibit();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.remove(&surface);
        self.refresh_idle_inhibit();
    }
}

impl Orbit {
    /// The user did something so idle timers start over.
    pub fn note_activity(&mut self) {
        let seat = self.seat.clone();
        self.idle_notifier.notify_activity(&seat);
    }

    /// Whether `surface` (or the surface it is a subsurface of) is shown.
    fn surface_shown(&self, surface: &WlSurface) -> bool {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if self.session_lock.is_active() {
            return self.output_of_lock_surface(&root).is_some();
        }
        if self
            .space
            .elements()
            .any(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
        {
            return true;
        }
        self.space.outputs().any(|output| {
            layer_map_for_output(output)
                .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                .is_some()
        })
    }

    /// Idle is inhibited while any inhibiting surface is shown. Cheap when
    /// there are none, so it runs after every dispatch (a window can be
    /// hidden or shown without its inhibitor changing).
    pub fn refresh_idle_inhibit(&mut self) {
        self.idle_inhibitors.retain(|s| s.is_alive());
        let inhibited = self.idle_inhibitors.iter().any(|s| self.surface_shown(s));
        if self.idle_notifier.is_inhibited() != inhibited {
            self.idle_notifier.set_is_inhibited(inhibited);
        }
    }
}
