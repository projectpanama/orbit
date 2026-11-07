pub mod config;
pub mod cursor;
pub mod gestures;
pub mod method;

use orbit_config::{Action, Modifiers};
use orbit_core::layout::output::{self, Direction};
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent,
        GestureEndEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent, InputTime, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    backend::session::Session,
    desktop::Window,
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, CursorImageStatus, GrabStartData, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER, Serial},
};

use crate::{
    geometry::{ToCore, ToSmithay},
    input::gestures::SwipeSource,
    state::{Hit, Orbit},
    window::Side,
    window::decorations::DecorationHit,
    window::grabs::Edges,
    workspaces::{WorkspaceTarget, window_id},
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
/// Two clicks on a title bar within this many milliseconds toggles maximize.
const DOUBLE_CLICK_MS: u128 = 400;

impl Orbit {
    /// Processes one backend input event.
    ///
    /// Activity is recorded before the event is dispatched. Pointer motion
    /// redraws only the old and new cursor locations while other input queues
    /// a redraw for the complete scene.
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        let activity = matches!(
            event,
            InputEvent::Keyboard { .. }
                | InputEvent::PointerMotion { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerButton { .. }
                | InputEvent::PointerAxis { .. }
                | InputEvent::GestureSwipeBegin { .. }
                | InputEvent::GestureSwipeUpdate { .. }
                | InputEvent::GestureSwipeEnd { .. }
                | InputEvent::GesturePinchBegin { .. }
                | InputEvent::GesturePinchUpdate { .. }
                | InputEvent::GesturePinchEnd { .. }
                | InputEvent::GestureHoldBegin { .. }
                | InputEvent::GestureHoldEnd { .. }
        );
        if activity {
            self.note_activity();
        }
        // Motion redraws the old and new cursor locations.
        // Dragged windows and hovered buttons request their own redraws.
        let motion = matches!(
            event,
            InputEvent::PointerMotion { .. } | InputEvent::PointerMotionAbsolute { .. }
        );
        match event {
            InputEvent::Keyboard { event, .. } => self.on_keyboard::<I>(event),
            InputEvent::PointerMotion { event, .. } => self.on_pointer_motion::<I>(event),
            InputEvent::PointerMotionAbsolute { event, .. } => {
                self.on_pointer_motion_absolute::<I>(event)
            }
            InputEvent::PointerButton { event, .. } => self.on_pointer_button::<I>(event),
            InputEvent::PointerAxis { event, .. } => self.on_pointer_axis::<I>(event),
            InputEvent::GestureSwipeBegin { event, .. } => {
                // Nothing moves behind the lock screen.
                let locked = self.session_lock.is_active();
                if event.fingers() == self.config.overview_swipe.fingers && !locked {
                    self.overview_swipe_begin();
                } else if event.fingers() == self.config.workspace_swipe.fingers && !locked {
                    self.swipe_begin(SwipeSource::Touchpad);
                }
            }
            InputEvent::GestureSwipeUpdate { event, .. } => {
                if self.overview_swiping() {
                    self.overview_swipe_update(event.delta_y());
                } else if self.touchpad_swiping() {
                    self.swipe_update(event.delta_x());
                }
            }
            InputEvent::GestureSwipeEnd { event, .. } => {
                if self.overview_swiping() {
                    self.overview_swipe_end(event.cancelled());
                } else if self.touchpad_swiping() {
                    self.swipe_end(event.cancelled());
                }
            }
            _ => return,
        }
        if !motion {
            // Other input can change the cursor or focus, move or resize
            // windows, or run a binding. One damage check is cheaper than
            // tracking each possible change.
            self.queue_redraw_all();
        }
    }

    /// Reports whether a workspace touchpad swipe is active.
    fn touchpad_swiping(&self) -> bool {
        self.input
            .workspace_swipe
            .as_ref()
            .is_some_and(|swipe| swipe.source == SwipeSource::Touchpad)
    }

    /// Handles a keyboard event and runs a matching compositor action.
    fn on_keyboard<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time(&event);
        let keycode = event.key_code();
        let key_state = event.state();
        let keyboard = self.keyboard.clone();

        let action = keyboard.input(
            self,
            keycode,
            key_state,
            serial,
            time,
            |state, modifiers, handle| {
                if key_state == KeyState::Released {
                    return if state.input.suppressed_keys.remove(&keycode) {
                        FilterResult::Intercept(None)
                    } else {
                        FilterResult::Forward
                    };
                }
                // Ctrl Alt F<n> produces XF86Switch_VT_<n>.
                // On a TTY this switches virtual terminals.
                let modified = handle.modified_sym().raw();
                let first_vt = Keysym::XF86_Switch_VT_1.raw();
                if state.session.is_some()
                    && (first_vt..=Keysym::XF86_Switch_VT_12.raw()).contains(&modified)
                {
                    state.input.suppressed_keys.insert(keycode);
                    let vt = (modified - first_vt + 1) as i32;
                    return FilterResult::Intercept(Some(Action::SwitchVt(vt)));
                }
                // Match the unshifted symbol so bindings do not depend on
                // Shift or the active layout shift level.
                let keysym = handle
                    .raw_latin_sym_or_raw_current_sym()
                    .unwrap_or_else(|| handle.modified_sym());
                // Escape leaves the overview.
                if keysym == Keysym::Escape
                    && !state.session_lock.is_active()
                    && let Some(output) = state.overview_for_escape()
                {
                    state.input.suppressed_keys.insert(keycode);
                    state.close_overview(&output);
                    return FilterResult::Intercept(None);
                }
                // While locked only bindings marked allow when locked run.
                // The remaining keys go to the lock screen.
                match state.config.bind_for(
                    state.input.mod_key,
                    &self::modifiers(modifiers),
                    keysym,
                ) {
                    Some((action, allowed)) if allowed || !state.session_lock.is_active() => {
                        state.input.suppressed_keys.insert(keycode);
                        FilterResult::Intercept(Some(action))
                    }
                    _ => FilterResult::Forward,
                }
            },
        );

        if key_state == KeyState::Released
            && !self
                .input
                .mod_key
                .pressed(&modifiers(&keyboard.modifier_state()))
        {
            self.swipe_mod_released();
        }
        if let Some(Some(action)) = action {
            self.run_action(action);
        }
    }

    /// Runs a configured action against the current compositor state.
    fn run_action(&mut self, action: Action) {
        if let Action::Spawn(command) = &action {
            self.spawn(command);
            return;
        }
        match action {
            Action::FocusNext => return self.focus_next(),
            Action::RestoreMinimized => return self.restore_minimized(),
            Action::Quit => {
                tracing::info!("quitting");
                return self.loop_signal.stop();
            }
            Action::SwitchVt(vt) => {
                if let Some(session) = &mut self.session
                    && let Err(err) = session.change_vt(vt)
                {
                    tracing::warn!(vt, %err, "failed to switch VT");
                }
                return;
            }
            Action::FocusWorkspace(n) => {
                return self.switch_focused_workspace(WorkspaceTarget::Index(n.saturating_sub(1)));
            }
            Action::FocusWorkspaceNext => {
                return self.switch_focused_workspace(WorkspaceTarget::By(1));
            }
            Action::FocusWorkspacePrev => {
                return self.switch_focused_workspace(WorkspaceTarget::By(-1));
            }
            Action::ToggleOverview => return self.toggle_overview(),
            Action::FocusOutputLeft => return self.focus_output(Direction::Left),
            Action::FocusOutputRight => return self.focus_output(Direction::Right),
            _ => {}
        }

        let Some(window) = self.focused_window() else {
            return;
        };
        match action {
            Action::CloseWindow => Orbit::close(&window),
            Action::ToggleMaximize => self.toggle_maximize(&window),
            Action::ToggleFullscreen => self.toggle_fullscreen(&window),
            Action::Minimize => self.minimize(&window),
            Action::TileLeft => self.toggle_tile(&window, Side::Left),
            Action::TileRight => self.toggle_tile(&window, Side::Right),
            Action::MoveLeft => self.move_by(&window, -1, 0),
            Action::MoveRight => self.move_by(&window, 1, 0),
            Action::MoveUp => self.move_by(&window, 0, -1),
            Action::MoveDown => self.move_by(&window, 0, 1),
            Action::GrowWidth => self.resize_by(&window, 1, 0),
            Action::ShrinkWidth => self.resize_by(&window, -1, 0),
            Action::GrowHeight => self.resize_by(&window, 0, 1),
            Action::ShrinkHeight => self.resize_by(&window, 0, -1),
            Action::MoveToWorkspace(n) => {
                self.move_window_to_workspace(&window, WorkspaceTarget::Index(n.saturating_sub(1)));
            }
            Action::MoveToWorkspaceNext => {
                self.move_window_to_workspace(&window, WorkspaceTarget::By(1));
            }
            Action::MoveToWorkspacePrev => {
                self.move_window_to_workspace(&window, WorkspaceTarget::By(-1));
            }
            Action::MoveToOutputLeft => self.move_window_towards(&window, Direction::Left),
            Action::MoveToOutputRight => self.move_window_towards(&window, Direction::Right),
            Action::FocusNext
            | Action::RestoreMinimized
            | Action::Quit
            | Action::SwitchVt(_)
            | Action::Spawn(_)
            | Action::FocusWorkspace(_)
            | Action::FocusWorkspaceNext
            | Action::FocusWorkspacePrev
            | Action::FocusOutputLeft
            | Action::FocusOutputRight
            | Action::ToggleOverview => {}
        }
    }

    /// Converts an absolute device position into compositor coordinates.
    fn on_pointer_motion_absolute<I: InputBackend>(
        &mut self,
        event: I::PointerMotionAbsoluteEvent,
    ) {
        let Some(output_geo) = self
            .active_output()
            .and_then(|output| self.space.output_geometry(&output))
        else {
            return;
        };
        let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
        self.pointer_moved_to(pos, event.time());
    }

    /// Handles relative pointer motion from devices such as a mouse.
    ///
    /// The pointer can cross between touching outputs and remains within the
    /// output area elsewhere.
    /// Handles relative pointer motion and keeps it within the output space.
    fn on_pointer_motion<I: InputBackend>(&mut self, event: I::PointerMotionEvent) {
        let space = &self.space;
        let outputs = || {
            space
                .outputs()
                .filter_map(|output| space.output_geometry(output))
                .map(ToCore::to_core)
        };
        if outputs().next().is_none() {
            return;
        }
        let old = self.pointer.current_location();
        let new = old + event.delta();
        let pos = output::constrain_pointer(old.to_core(), new.to_core(), outputs).to_smithay();
        self.pointer_moved_to(pos, event.time());
    }

    /// Moves the pointer to a new logical position and forwards the motion.
    ///
    /// The old and new cursor locations are redrawn.
    pub fn pointer_moved_to(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        // Redraw the cursor before and after moving it.
        self.queue_redraw_pointer();
        self.move_pointer(pos, time);
        self.queue_redraw_pointer();
    }

    /// Forwards a pointer position to the active focus and grab.
    fn move_pointer(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.pointer.clone();
        if self.session_lock.is_active() {
            // Only lock surfaces get the pointer.
            let under = self.lock_surface_under(pos);
            if !pointer.is_grabbed() {
                self.input.cursor_status = CursorImageStatus::default_named();
            }
            pointer.motion(
                self,
                under,
                &MotionEvent {
                    location: pos,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
            return;
        }
        // In overview mode windows take no pointer input.
        // The compositor handles clicks while panels on top still receive it.
        let overview = self
            .space
            .output_under(pos)
            .next()
            .cloned()
            .is_some_and(|o| self.in_overview(&o));
        let hit = if overview { None } else { self.hit_test(pos) };

        // Panels above windows take the pointer first.
        // Panels below windows receive it only where no window is present.
        let layer_hit = self.upper_layer_surface_under(pos).or_else(|| {
            hit.is_none()
                .then(|| self.lower_layer_surface_under(pos))
                .flatten()
        });
        // Title bar buttons highlight under the pointer unless a drag is active.
        let hover = match (&hit, &layer_hit) {
            (Some((window, Hit::Decoration(deco))), None) if !pointer.is_grabbed() => {
                Some((window, *deco))
            }
            _ => None,
        };
        self.set_decoration_hover(hover);

        if let Some((_, surface, loc)) = layer_hit {
            // Reset the previous client cursor.
            // The panel may set its own cursor after the enter event.
            if !pointer.is_grabbed() && pointer.current_focus().as_ref() != Some(&surface) {
                self.input.cursor_status = CursorImageStatus::default_named();
            }
            pointer.motion(
                self,
                Some((surface, loc)),
                &MotionEvent {
                    location: pos,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
            return;
        }

        // Outside client surfaces the compositor chooses the cursor.
        // Pointer grabs choose their own cursor.
        if !pointer.is_grabbed() {
            match &hit {
                Some((_, Hit::Surface(..))) => {}
                Some((_, Hit::Decoration(DecorationHit::Edge(edges)))) => {
                    self.input.cursor_status = CursorImageStatus::Named(edges.cursor_icon());
                }
                Some((_, Hit::Decoration(_))) | None => {
                    self.input.cursor_status = CursorImageStatus::default_named();
                }
            }
        }

        let under = match hit {
            Some((_, Hit::Surface(surface, loc))) => Some((surface, loc)),
            _ => None,
        };
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// Handles pointer button presses and releases.
    fn on_pointer_button<I: InputBackend>(&mut self, event: I::PointerButtonEvent) {
        let pointer = self.pointer.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let button_state = event.state();

        // A press outside an active grab focuses the
        // window under the cursor, or clears focus over the background.
        // While locked it moves the keyboard to the output lock surface.
        let location = pointer.current_location();
        let locked = self.session_lock.is_active();
        if locked && button_state == ButtonState::Pressed && !pointer.is_grabbed() {
            let output = self.space.output_under(location).next().cloned();
            if let Some(output) = output {
                self.set_focused_output(&output);
            }
            self.focus_lock_surface();
        }

        let layer_hit = (!locked && button_state == ButtonState::Pressed && !pointer.is_grabbed())
            .then(|| self.layer_hit_for_press(location))
            .flatten();
        if let Some(layer_surface) = layer_hit {
            // Clicking a panel leaves window focus alone unless the panel
            // wants the keyboard and no other panel holds it exclusively.
            if layer_surface.can_receive_keyboard_focus()
                && self
                    .exclusive_layer()
                    .is_none_or(|exclusive| &exclusive == layer_surface.wl_surface())
            {
                let surface = layer_surface.wl_surface().clone();
                self.keyboard.clone().set_focus(self, Some(surface), serial);
            }
        } else if !locked
            && button_state == ButtonState::Pressed
            && !pointer.is_grabbed()
            && self.overview_click(location)
        {
            // Overview handled the press.
        } else if !locked && button_state == ButtonState::Pressed && !pointer.is_grabbed() {
            // Clicking an output selects it for keyboard actions even when
            // no window is under the pointer.
            let clicked = self.space.output_under(location).next().cloned();
            if let Some(output) = clicked {
                self.set_focused_output(&output);
            }
            let hit = self.hit_test(location);
            let window = hit.as_ref().map(|(w, _)| w.clone());
            self.focus_window(window.as_ref(), serial);

            let modifiers = self.keyboard.clone().modifier_state();
            let button = event.button_code();
            if let Some((window, hit)) = hit {
                if self.input.mod_key.pressed(&self::modifiers(&modifiers))
                    && (button == BTN_LEFT || button == BTN_RIGHT)
                {
                    // Mod Left drags the window. Mod Right resizes it.
                    let start_data = GrabStartData {
                        focus: None,
                        button,
                        location,
                    };
                    if button == BTN_LEFT {
                        self.start_move(window, start_data, serial);
                    } else if let Some(rect) = self.frame_geometry(&window) {
                        let edges = Edges::nearest(rect, location);
                        self.start_resize(window, start_data, edges, serial);
                    }
                } else if let Hit::Decoration(decoration) = hit
                    && button == BTN_LEFT
                {
                    self.on_decoration_press(window, decoration, location, serial);
                }
            }
        }

        pointer.button(
            self,
            &ButtonEvent {
                button: event.button_code(),
                state: button_state,
                serial,
                time: event.time(),
            },
        );
        pointer.frame(self);
    }

    /// Handles presses on server side decoration controls and edges.
    fn on_decoration_press(
        &mut self,
        window: Window,
        hit: DecorationHit,
        location: Point<f64, Logical>,
        serial: Serial,
    ) {
        let start_data = GrabStartData {
            focus: None,
            button: BTN_LEFT,
            location,
        };
        match hit {
            DecorationHit::Titlebar => {
                let id = window_id(&window);
                let double_click =
                    self.input
                        .last_titlebar_click
                        .take()
                        .is_some_and(|(last, at)| {
                            Some(last) == id && at.elapsed().as_millis() <= DOUBLE_CLICK_MS
                        });
                if double_click {
                    self.toggle_maximize(&window);
                } else {
                    self.input.last_titlebar_click = id.map(|id| (id, std::time::Instant::now()));
                    self.start_move(window, start_data, serial);
                }
            }
            DecorationHit::Close => Orbit::close(&window),
            DecorationHit::Maximize => self.toggle_maximize(&window),
            DecorationHit::Minimize => self.minimize(&window),
            DecorationHit::Edge(edges) => self.start_resize(window, start_data, edges, serial),
        }
    }

    /// Handles pointer scrolling and forwards unhandled axis events.
    fn on_pointer_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        let source = event.source();
        // Smooth scrolling uses Finger for libinput touchpads.
        // The nested backend uses Continuous for the host touchpad.
        // Wheel clicks never start a swipe.
        if matches!(source, AxisSource::Finger | AxisSource::Continuous) {
            let mod_held = self.seat.get_keyboard().is_some_and(|keyboard| {
                self.input
                    .mod_key
                    .pressed(&modifiers(&keyboard.modifier_state()))
            });
            if self.swipe_scroll(
                event.amount(Axis::Horizontal),
                event.amount(Axis::Vertical),
                mod_held,
            ) {
                return;
            }
        }

        let mut frame = AxisFrame::new(event.time()).source(source);
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let amount = event
                .amount(axis)
                .or_else(|| event.amount_v120(axis).map(|v120| v120 * 15.0 / 120.0));
            if let Some(amount) = amount {
                if amount != 0.0 {
                    frame = frame.value(axis, amount);
                    if let Some(v120) = event.amount_v120(axis) {
                        frame = frame.v120(axis, v120 as i32);
                    }
                } else if source == AxisSource::Finger {
                    frame = frame.stop(axis);
                }
            }
        }

        let pointer = self.pointer.clone();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}

/// Converts Smithay modifier state into the binding modifier state.
pub fn modifiers(state: &ModifiersState) -> Modifiers {
    Modifiers {
        ctrl: state.ctrl,
        alt: state.alt,
        shift: state.shift,
        meta: state.logo,
    }
}
