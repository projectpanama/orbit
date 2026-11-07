pub mod config;
pub mod scale;

use std::time::Duration;

use orbit_core::{
    animation::workspace::WorkspaceSlide,
    frame::FrameScheduler,
    ids::{OutputId, WorkspaceId},
    layout::{self, geometry::Point, output::Slot},
};
use smithay::{
    backend::input::InputTime,
    desktop::{Window, layer_map_for_output},
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::backend::GlobalId,
    },
    utils::{Logical, Rectangle, Size},
};

use crate::{
    geometry::{RectExt, ToCore, ToSmithay},
    shell::overview::OverviewState,
    state::Orbit,
};

const GLOBAL_REMOVAL_DELAY: Duration = Duration::from_secs(5);

/// Configuration used when an output is added.
#[derive(Debug, Clone, Copy, Default)]
pub struct OutputSetup {
    /// Preferred logical position for the output.
    pub configured_position: Option<Point>,
    /// Whether the output is an internal display.
    pub internal: bool,
}

/// Runtime state associated with one output.
#[derive(Debug)]
pub struct OutputState {
    /// Stable identifier assigned to the output.
    pub id: OutputId,
    /// Scheduler for frames on this output.
    pub frame: FrameScheduler,
    /// Wayland global owned by this output.
    pub global: GlobalId,
    /// Position requested by the output configuration.
    pub configured_position: Option<Point>,
    /// Whether the output is an internal display.
    pub internal: bool,
    /// Time of the most recently presented frame.
    pub last_presented: Option<Duration>,
    /// Workspace transition currently shown on the output.
    pub workspace_slide: Option<WorkspaceSlide<WorkspaceId>>,
    /// Overview state currently shown on the output.
    pub overview: Option<OverviewState>,
}

/// Saved window information used while outputs are rearranged.
struct WindowHome {
    /// Window whose position is saved.
    window: Window,
    /// Output containing the window.
    output: Output,
    /// Output geometry at the time of saving.
    geometry: Rectangle<i32, Logical>,
}

impl Orbit {
    /// Registers an output and places it in the output layout.
    ///
    /// Existing windows are repositioned when the layout changes.
    pub fn add_output(&mut self, output: &Output, setup: OutputSetup) {
        let global = output.create_global::<Orbit>(&self.display_handle);
        let homes = self.window_homes();

        self.outputs.insert(
            output.clone(),
            OutputState {
                id: self.output_ids.allocate(),
                frame: FrameScheduler::default(),
                global,
                configured_position: setup.configured_position,
                internal: setup.internal,
                last_presented: None,
                workspace_slide: None,
                overview: None,
            },
        );

        self.arrange_outputs();
        self.add_output_workspaces(output);
        self.outputs_changed(homes);

        if let Some(theme) = self.input.cursor_theme.as_mut() {
            theme.preload(crate::input::cursor::cursor_scale(output));
        }
    }

    /// Removes an output and moves its workspaces to a remaining output.
    pub fn remove_output(&mut self, output: &Output) {
        if !self.outputs.contains_key(output) {
            return;
        }
        let homes = self.window_homes();

        // Prefer the output under the pointer unless it is being removed.
        let pointer = self.seat.get_pointer().map(|p| p.current_location());
        let others: Vec<Output> = self
            .space
            .outputs()
            .filter(|o| *o != output)
            .cloned()
            .collect();
        let target = others
            .iter()
            .find(|other| {
                pointer.is_some_and(|pointer| {
                    self.space
                        .output_geometry(other)
                        .is_some_and(|geometry| geometry.to_f64().contains(pointer))
                })
            })
            .or_else(|| others.first())
            .cloned();
        self.remove_output_workspaces(output, target.as_ref());
        let Some(state) = self.outputs.remove(output) else {
            return;
        };
        self.lock_output_removed(output);

        // Layer surfaces belong to this output.
        // Clients can reopen them on another output.
        {
            let mut map = layer_map_for_output(output);
            let layers: Vec<_> = map.layers().cloned().collect();
            for layer in layers {
                layer.layer_surface().send_close();
                map.unmap_layer(&layer);
            }
        }
        self.space.unmap_output(output);

        self.display_handle
            .disable_global::<Orbit>(state.global.clone());
        let global = state.global;
        let timer = Timer::from_duration(GLOBAL_REMOVAL_DELAY);
        if let Err(err) = self.loop_handle.insert_source(timer, move |_, _, orbit| {
            orbit.display_handle.remove_global::<Orbit>(global.clone());
            TimeoutAction::Drop
        }) {
            tracing::warn!(%err, "failed to schedule removing an output global");
        }

        self.arrange_outputs();
        self.keep_pointer_on_outputs();
        self.outputs_changed(homes);

        // The focused window may have left with the removed workspace.
        // Move keyboard focus to the target output.
        if let Some(target) = target.as_ref().and_then(|t| self.output_id(t))
            && self
                .focused_window()
                .is_some_and(|w| self.space.element_location(&w).is_none())
        {
            self.refocus_output(target);
        }
        // A launcher on the removed output may have held keyboard focus.
        self.refresh_layer_focus();
    }

    /// Recomputes output positions and preserves window placement.
    pub fn relayout_outputs(&mut self) {
        let homes = self.window_homes();
        self.arrange_outputs();
        self.outputs_changed(homes);
    }

    /// Returns the output under the pointer or the first available output.
    pub fn active_output(&self) -> Option<Output> {
        let pointer = self.seat.get_pointer()?.current_location();
        self.space
            .output_under(pointer)
            .next()
            .or_else(|| self.space.outputs().next())
            .cloned()
    }

    /// Returns the output containing most of a window frame.
    ///
    /// The active output is used when the frame does not overlap an output.
    pub fn output_under_window(&self, window: &Window) -> Option<Output> {
        self.frame_geometry(window)
            .and_then(|frame| self.output_for_rect(frame))
            .or_else(|| self.active_output())
    }

    /// Returns the output with the largest overlap with a rectangle.
    pub fn output_for_rect(&self, rect: Rectangle<i32, Logical>) -> Option<Output> {
        let overlap = |output: &Output| {
            self.space
                .output_geometry(output)
                .and_then(|geometry| geometry.intersection(rect))
                .map_or(0, |area| i64::from(area.size.w) * i64::from(area.size.h))
        };
        self.space
            .outputs()
            .map(|output| (output, overlap(output)))
            .filter(|(_, area)| *area > 0)
            .max_by_key(|(_, area)| *area)
            .map(|(output, _)| output.clone())
    }

    /// Moves a window to an output while fitting its frame into that output.
    pub fn move_to_output(&mut self, window: &Window, output: &Output) {
        let (Some(frame), Some(location), Some(target)) = (
            self.frame_geometry(window),
            self.space.element_location(window),
            self.space.output_geometry(output),
        ) else {
            return;
        };
        let from = self.output_for_rect(frame);
        if from.as_ref() == Some(output) {
            return;
        }
        let from_geometry = from.and_then(|from| self.space.output_geometry(&from));
        let new_frame_loc = layout::output::fit_rect(
            frame.to_core(),
            from_geometry.map(ToCore::to_core),
            target.to_core(),
        )
        .to_smithay();
        self.space.map_element(
            window.clone(),
            location + (new_frame_loc - frame.loc),
            false,
        );
    }

    /// Moves a window onto an output when its frame is outside all outputs.
    pub fn ensure_on_output(&mut self, window: &Window) {
        let Some(frame) = self.frame_geometry(window) else {
            return;
        };
        if self.output_for_rect(frame).is_some() {
            return;
        }
        if let Some(output) = self.window_output(window).or_else(|| self.active_output()) {
            self.move_to_output(window, &output);
        }
    }

    /// Records each visible window position before outputs are rearranged.
    fn window_homes(&self) -> Vec<WindowHome> {
        self.space
            .elements()
            .filter_map(|window| {
                let frame = self.frame_geometry(window)?;
                let output = self.output_for_rect(frame)?;
                let geometry = self.space.output_geometry(&output)?;
                Some(WindowHome {
                    window: window.clone(),
                    output,
                    geometry,
                })
            })
            .collect()
    }

    /// Assigns logical positions to all outputs.
    fn arrange_outputs(&mut self) {
        let mut outputs: Vec<(&Output, &OutputState)> = self.outputs.iter().collect();

        // Keep a stable order so the same monitors use the same positions.
        outputs.sort_by(|(a, a_state), (b, b_state)| {
            b_state
                .internal
                .cmp(&a_state.internal)
                .then_with(|| a.name().cmp(&b.name()))
        });

        let slots: Vec<Slot> = outputs
            .iter()
            .map(|(output, state)| Slot {
                size: logical_size(output).to_core(),
                configured: state.configured_position,
            })
            .collect();

        let outputs: Vec<Output> = outputs.into_iter().map(|(o, _)| o.clone()).collect();

        // Arrange outputs into slots and update their scene positions.
        for (output, arranged) in outputs.iter().zip(layout::output::arrange(&slots)) {
            let position = arranged.position.to_smithay();
            if arranged.overlapped {
                tracing::warn!(
                    output = output.name(),
                    "configured output position overlaps another output, placing it automatically"
                );
            }
            if output.current_location() != position {
                output.change_current_state(None, None, None, Some(position));
            }
            self.space.map_output(output, position);
        }
    }

    /// Repositions windows after output geometry or membership changes.
    fn outputs_changed(&mut self, homes: Vec<WindowHome>) {
        for home in homes {
            // Windows whose workspace went to another output were already put there.
            if self.window_output(&home.window).as_ref() != Some(&home.output) {
                continue;
            }
            let (Some(frame), Some(location)) = (
                self.frame_geometry(&home.window),
                self.space.element_location(&home.window),
            ) else {
                continue;
            };
            let new_frame_loc = match self.space.output_geometry(&home.output) {
                Some(geometry) if geometry.loc == home.geometry.loc => continue,
                // Move windows with an output that changed position.
                Some(geometry) => frame.loc + (geometry.loc - home.geometry.loc),
                None => {
                    let Some(target) = self
                        .active_output()
                        .and_then(|output| self.space.output_geometry(&output))
                    else {
                        continue;
                    };
                    layout::output::fit_rect(
                        frame.to_core(),
                        Some(home.geometry.to_core()),
                        target.to_core(),
                    )
                    .to_smithay()
                }
            };
            self.space.map_element(
                home.window.clone(),
                location + (new_frame_loc - frame.loc),
                false,
            );
        }
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in &windows {
            self.ensure_on_output(window);
        }
        self.arrange_after_output_change();
        self.output_management.dirty = true;
        self.queue_redraw_all();
    }

    /// Keeps the pointer on an output after the output layout changes.
    fn keep_pointer_on_outputs(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let location = pointer.current_location();
        if self.space.output_under(location).next().is_some() {
            return;
        }
        let Some(geometry) = self
            .space
            .outputs()
            .next()
            .and_then(|output| self.space.output_geometry(output))
        else {
            return;
        };
        let center = geometry.center();
        self.pointer_moved_to(center, InputTime::now());
    }
}

/// Returns the logical size of an output using its mode and scale.
fn logical_size(output: &Output) -> Size<i32, Logical> {
    output.current_mode().map_or_else(Size::default, |mode| {
        output
            .current_transform()
            .transform_size(mode.size)
            .to_f64()
            .to_logical(output.current_scale().fractional_scale())
            .to_i32_ceil()
    })
}
