use smithay::{
    backend::input::InputTime,
    desktop::Window,
    output::Output,
    utils::{Point, SERIAL_COUNTER},
};

use orbit_core::{
    animation::workspace::WorkspaceSlide,
    ids::{OutputId, OutputKey, WindowId, WorkspaceId},
    layout::output::{self, Direction},
    workspaces::Visibility,
};

use crate::{
    geometry::{RectExt, ToCore, ToSmithay},
    state::Orbit,
    window::{is_minimized, with_window_state},
};

/// Which workspace to go to on an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTarget {
    /// Selects a workspace by zero based position.
    ///
    /// A position beyond the end selects the last empty workspace.
    Index(usize),
    /// Selects a workspace relative to the current one.
    By(isize),
}

impl Orbit {
    /// Registers a new toplevel and assigns it to the focused output.
    ///
    /// Returns true when the window is visible.
    pub fn register_window(&mut self, window: &Window) -> bool {
        let id: WindowId = self.window_ids.allocate();
        with_window_state(window, |state| state.id = Some(id));
        self.windows.insert(id, window.clone());
        if let Some(toplevel) = window.toplevel() {
            self.window_surfaces
                .insert(toplevel.wl_surface().clone(), id);
        }
        let output = self
            .focused_output()
            .and_then(|output| self.output_id(&output));
        self.workspaces.add_window(id, output);
        self.workspaces.is_visible(id)
    }

    /// Removes a destroyed toplevel from window and workspace state.
    pub fn unregister_window(&mut self, window: &Window) {
        if let Some(toplevel) = window.toplevel() {
            self.window_surfaces.remove(toplevel.wl_surface());
        }
        if let Some(id) = window_id(window) {
            self.windows.remove(&id);
            self.workspaces.remove_window(id);
            if self
                .input
                .last_titlebar_click
                .is_some_and(|(clicked, _)| clicked == id)
            {
                self.input.last_titlebar_click = None;
            }
            if self
                .input
                .decoration_hover
                .is_some_and(|(hovered, _)| hovered == id)
            {
                self.input.decoration_hover = None;
            }
        }
        self.desktop.toplevels_dirty = true;
    }

    /// Returns the identifier assigned to an output.
    pub fn output_id(&self, output: &Output) -> Option<OutputId> {
        self.outputs.get(output).map(|state| state.id)
    }

    /// Finds an output by its identifier.
    pub fn output_by_id(&self, id: OutputId) -> Option<Output> {
        self.outputs
            .iter()
            .find(|(_, state)| state.id == id)
            .map(|(output, _)| output.clone())
    }

    /// Returns the output that owns the window workspace.
    ///
    /// This is based on workspace state rather than window geometry.
    pub fn window_output(&self, window: &Window) -> Option<Output> {
        let id = window_id(window)?;
        self.output_by_id(self.workspaces.output_of(id)?)
    }

    /// Returns the output used for keyboard workspace and window actions.
    ///
    /// The focused output is preferred. The output under the pointer is used
    /// when no focused output is recorded.
    pub fn focused_output(&self) -> Option<Output> {
        self.focused_output
            .and_then(|id| self.output_by_id(id))
            .or_else(|| self.active_output())
    }

    /// Sets the output used for keyboard actions.
    pub fn set_focused_output(&mut self, output: &Output) {
        self.focused_output = self.output_id(output);
    }

    /// Adds an output and restores its existing workspace state.
    pub fn add_output_workspaces(&mut self, output: &Output) {
        let Some(id) = self.output_id(output) else {
            return;
        };
        let visibility = self.workspaces.add_output(id, OutputKey(output.name()));
        self.apply_visibility(visibility);
    }

    /// Removes an output and hides its workspaces.
    ///
    /// The output must still be mapped so window positions relative to it can
    /// be saved.
    pub fn remove_output_workspaces(&mut self, output: &Output, target: Option<&Output>) {
        let Some(id) = self.output_id(output) else {
            return;
        };
        let target = target.and_then(|target| self.output_id(target));
        let visibility = self.workspaces.remove_output(id, target);
        self.apply_visibility(visibility);
    }

    /// Shows another workspace on an output.
    ///
    /// Other outputs keep their current workspace.
    pub fn switch_workspace(&mut self, output: &Output, target: WorkspaceTarget) {
        let Some(id) = self.output_id(output) else {
            return;
        };
        // The keyboard takes over from a swipe on this output.
        if self
            .input
            .workspace_swipe
            .as_ref()
            .is_some_and(|swipe| swipe.output == id)
        {
            self.input.workspace_swipe = None;
        }

        let from = self.workspaces.active(id);
        let from_index = self.workspaces.active_index(id).map(|(index, _)| index);
        let visibility = match target {
            WorkspaceTarget::Index(index) => self.workspaces.switch_to(id, index),
            WorkspaceTarget::By(delta) => self.workspaces.switch_by(id, delta),
        };
        self.animate_switch(output, from, from_index);
        if visibility.is_empty() {
            // The workspace was already active or was an empty workspace.
            // Nothing visible or focused needs updating.
            self.log_workspace(output, id);
            return;
        }
        self.apply_visibility(visibility);

        if self.focused_output().as_ref() == Some(output) {
            self.refocus_output(id);
        }
        // Recalculate pointer focus under the updated scene.
        self.refresh_pointer_focus();
        self.log_workspace(output, id);
        self.queue_redraw(output);
    }

    /// Shows another workspace on the focused output.
    pub fn switch_focused_workspace(&mut self, target: WorkspaceTarget) {
        if let Some(output) = self.focused_output() {
            self.switch_workspace(&output, target);
        }
    }

    /// Moves a window to the neighboring output in the given direction.
    pub fn move_window_towards(&mut self, window: &Window, direction: Direction) {
        if let Some(output) = self
            .window_output(window)
            .and_then(|from| self.neighbor_output(&from, direction))
        {
            self.move_window_to_output(window, &output);
        }
    }

    /// Moves a window to another workspace on its output.
    ///
    /// The output keeps showing its current workspace.
    pub fn move_window_to_workspace(&mut self, window: &Window, target: WorkspaceTarget) {
        let (Some(id), Some(output)) = (window_id(window), self.window_output(window)) else {
            return;
        };
        let Some((_, current)) = self.workspaces.workspace_of(id) else {
            return;
        };
        let index = match target {
            WorkspaceTarget::Index(index) => index,
            WorkspaceTarget::By(delta) => current.saturating_add_signed(delta),
        };
        let visibility = self.workspaces.move_window_to_index(id, index);
        if visibility.is_empty() {
            return;
        }
        self.apply_visibility(visibility);
        if !self.workspaces.is_visible(id)
            && self.focused_window().as_ref() == Some(window)
            && let Some(output_id) = self.output_id(&output)
        {
            self.refocus_output(output_id);
        }
        self.refresh_pointer_focus();
        self.queue_redraw(&output);
    }

    /// Moves a window to a workspace on any output.
    ///
    /// Outputs keep their current workspaces. A visible window keeps its
    /// position relative to the output where it lands.
    pub fn move_window_to_workspace_id(&mut self, window: &Window, workspace: WorkspaceId) {
        let Some(id) = window_id(window) else {
            return;
        };
        let from = self.window_output(window);
        let visibility = self.workspaces.move_window_to_workspace(id, workspace);
        self.apply_visibility(visibility);
        if !self.workspaces.is_visible(id)
            && self.focused_window().as_ref() == Some(window)
            && let Some(output_id) = from.as_ref().and_then(|output| self.output_id(output))
        {
            self.refocus_output(output_id);
        }
        self.refresh_pointer_focus();
        self.queue_redraw_all();
    }

    /// Moves a window to the workspace shown by an output.
    ///
    /// The relative position is preserved and the window is focused there.
    pub fn move_window_to_output(&mut self, window: &Window, output: &Output) {
        let (Some(id), Some(output_id)) = (window_id(window), self.output_id(output)) else {
            return;
        };
        if self.workspaces.output_of(id) == Some(output_id) {
            return;
        }
        if self.workspaces.is_visible(id) {
            self.move_to_output(window, output);
        } else if let Some(target) = self.space.output_geometry(output) {
            // Place a hidden window in the middle of the target output.
            with_window_state(window, |state| {
                let center = target.size.to_point().downscale(2)
                    - window.geometry().size.to_point().downscale(2);
                state.hidden_at = Some((center, 0));
            });
        }
        let visibility = self.workspaces.move_window_to_output(id, output_id);
        self.apply_visibility(visibility);
        self.refit(window);
        self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
        self.queue_redraw_all();
    }

    /// Assigns a visible window to the output containing most of its frame.
    ///
    /// This is used after a window was moved by hand.
    pub fn sync_window_output(&mut self, window: &Window) {
        let output = self
            .frame_geometry(window)
            .and_then(|frame| self.output_for_rect(frame));
        if let Some(output) = output {
            self.assign_window_to_output(window, &output);
        }
    }

    /// Assigns a visible window to the workspace shown by an output.
    ///
    /// The geometry is left unchanged. This is used while dragging a window.
    pub fn assign_window_to_output(&mut self, window: &Window, output: &Output) {
        let (Some(id), Some(output_id)) = (window_id(window), self.output_id(output)) else {
            return;
        };
        if !self.workspaces.is_visible(id) || self.workspaces.output_of(id) == Some(output_id) {
            return;
        }
        let visibility = self.workspaces.move_window_to_output(id, output_id);
        self.apply_visibility(visibility);
        if self.focused_window().as_ref() == Some(window) {
            self.focused_output = Some(output_id);
        }
    }

    /// Focuses the neighboring output in the given direction.
    ///
    /// Its focus target is selected and the pointer moves there so keyboard
    /// and pointer focus remain aligned.
    pub fn focus_output(&mut self, direction: Direction) {
        let Some(output) = self
            .focused_output()
            .and_then(|from| self.neighbor_output(&from, direction))
        else {
            return;
        };
        let Some(id) = self.output_id(&output) else {
            return;
        };
        self.focused_output = Some(id);
        self.refocus_output(id);
        if let Some(geometry) = self.space.output_geometry(&output) {
            let center = geometry.center();
            self.pointer_moved_to(center, InputTime::now());
        }
        self.queue_redraw_all();
    }

    /// Returns the output neighboring `from` in the given direction.
    pub fn neighbor_output(&self, from: &Output, direction: Direction) -> Option<Output> {
        let outputs: Vec<(Output, _)> = self
            .space
            .outputs()
            .filter_map(|output| Some((output.clone(), self.space.output_geometry(output)?)))
            .collect();
        let from = outputs.iter().position(|(output, _)| output == from)?;
        let rects: Vec<_> = outputs.iter().map(|(_, rect)| rect.to_core()).collect();
        let index = output::neighbor(&rects, from, direction)?;
        Some(outputs[index].0.clone())
    }

    /// Focuses the preferred window for an output or clears focus.
    ///
    /// This is used after the focused window disappears or the user selects
    /// another workspace or output.
    pub fn refocus_output(&mut self, output: OutputId) {
        let target = self.workspace_focus_target(output);
        self.focus_window(target.as_ref(), SERIAL_COUNTER.next_serial());
    }

    /// Returns the preferred focus window for an output.
    ///
    /// The previously focused window is preferred. The newest non minimized
    /// window is used when no previous focus exists.
    pub fn workspace_focus_target(&self, output: OutputId) -> Option<Window> {
        let id = self.workspaces.focus_target(output, |id| {
            self.windows
                .get(&id)
                .is_some_and(|window| !is_minimized(window))
        })?;
        self.windows.get(&id).cloned()
    }

    /// Synchronizes mapped windows with logical workspace visibility.
    ///
    /// Workspace slides are drawn on top of this state. The scene always
    /// matches the logical workspace state.
    pub fn apply_visibility(&mut self, visibility: Visibility<WindowId>) {
        // Hide from bottom to top so stamps preserve stacking order.
        let mut hide: Vec<Window> = self
            .space
            .elements()
            .filter(|window| window_id(window).is_some_and(|id| visibility.hide.contains(&id)))
            .cloned()
            .collect();
        for window in hide.drain(..) {
            self.hide_window(&window);
        }

        let mut show: Vec<Window> = visibility
            .show
            .iter()
            .filter_map(|id| self.windows.get(id))
            .filter(|window| !is_minimized(window))
            .cloned()
            .collect();
        show.sort_by_key(|window| {
            with_window_state(window, |state| state.hidden_at.map(|(_, stamp)| stamp))
        });
        for window in show {
            self.show_window(&window);
        }
    }

    /// Starts or redirects the visual slide between workspaces.
    ///
    /// The logical switch has already happened. This only changes how it looks.
    fn animate_switch(
        &mut self,
        output: &Output,
        from: Option<WorkspaceId>,
        from_index: Option<usize>,
    ) {
        let timing = self.config.animations.workspace_switch;
        let now = self.next_frame_time(output);
        let Some(state) = self.outputs.get_mut(output) else {
            return;
        };
        let to = self.workspaces.active(state.id);
        let to_index = self
            .workspaces
            .active_index(state.id)
            .map(|(index, _)| index);
        let (Some(from), Some(to), Some(from_index), Some(to_index)) =
            (from, to, from_index, to_index)
        else {
            return;
        };
        if from == to {
            return;
        }
        // Keep an existing slide that already targets the new workspace.
        if state
            .workspace_slide
            .as_ref()
            .is_some_and(|slide| slide.target() == Some(to))
        {
            return;
        }
        if timing.is_off() {
            state.workspace_slide = None;
            return;
        }
        // The source index predates the switch. An empty source can disappear
        // and move later indexes down. Greater than or equal still means
        // forward in that case.
        let forward = to_index >= from_index;
        match &mut state.workspace_slide {
            Some(slide) => slide.retarget(to, forward, now, timing),
            None => {
                state.workspace_slide = Some(WorkspaceSlide::new(from, to, forward, now, timing))
            }
        }
        self.queue_redraw(output);
    }

    /// Unmaps a window and saves its position relative to its output.
    fn hide_window(&mut self, window: &Window) {
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        // Save the position relative to the output where it was visible.
        let origin = self
            .frame_geometry(window)
            .and_then(|frame| self.output_for_rect(frame))
            .or_else(|| self.window_output(window))
            .and_then(|output| self.space.output_geometry(&output))
            .map(|geometry| geometry.loc)
            .unwrap_or_default();
        self.hide_stamp += 1;
        let stamp = self.hide_stamp;
        with_window_state(window, |state| {
            state.hidden_at = Some((location - origin, stamp))
        });
        self.space.unmap_elem(window);
    }

    /// Maps a window that became visible and restores its saved position.
    fn show_window(&mut self, window: &Window) {
        if self.space.element_location(window).is_some() {
            return;
        }
        let Some(geometry) = self
            .window_output(window)
            .and_then(|output| self.space.output_geometry(&output))
        else {
            return;
        };
        let relative = with_window_state(window, |state| state.hidden_at.take())
            .map(|(relative, _)| relative)
            .unwrap_or_else(|| crate::window::content_loc(window, Point::from((0, 0))));
        self.space
            .map_element(window.clone(), geometry.loc + relative, false);
        self.refit(window);
    }

    /// Keeps a shown window inside its workspace output and arranges it.
    ///
    /// The output may be smaller than or different from the previous output.
    fn refit(&mut self, window: &Window) {
        let Some(target) = self
            .window_output(window)
            .and_then(|output| self.space.output_geometry(&output))
        else {
            return;
        };
        if let (Some(frame), Some(location)) = (
            self.frame_geometry(window),
            self.space.element_location(window),
        ) && !target.contains_rect(frame)
        {
            let fitted = output::fit_rect(frame.to_core(), None, target.to_core()).to_smithay();
            self.space
                .map_element(window.clone(), location + (fitted - frame.loc), false);
        }
        self.arrange_and_configure(window);
    }

    /// Re-evaluates pointer focus after the scene changes.
    fn refresh_pointer_focus(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.is_grabbed() {
            return;
        }
        let location = pointer.current_location();
        self.pointer_moved_to(location, InputTime::now());
    }

    /// Logs the workspace currently shown by an output.
    fn log_workspace(&self, output: &Output, id: OutputId) {
        if let Some((index, count)) = self.workspaces.active_index(id) {
            tracing::info!(
                output = %output.name(),
                workspace = index + 1,
                of = count,
                "workspace shown"
            );
        }
    }
}

/// Returns the identifier assigned to a window.
pub fn window_id(window: &Window) -> Option<WindowId> {
    with_window_state(window, |state| state.id)
}
