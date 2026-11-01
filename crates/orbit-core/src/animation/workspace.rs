use std::time::Duration;

use super::{Animation, Spring, Timing};

/// How far past the first or last workspace the camera can be dragged, in
/// output widths. Dragging further meets more and more resistance.
const RUBBER_BAND_LIMIT: f64 = 0.25;

/// On release, the camera is projected this far ahead at its current
/// speed to pick where it lands, so a short flick is enough.
const RELEASE_PROJECTION: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy)]
enum Camera {
    Eased(Animation),
    Spring(Spring),
    Held(f64),
}

/// Animates a strip of workspaces while tracking the camera's position.
///
/// A slide can be eased between two workspaces, held while the user drags it,
/// or settled onto a workspace with spring physics.
#[derive(Debug, Clone)]
pub struct WorkspaceSlide<W> {
    strip: Vec<W>,
    camera: Camera,
    home: Option<W>,
}

impl<W: Copy + Eq> WorkspaceSlide<W> {
    /// Creates a slide from `from` to `to`.
    ///
    /// When `forward` is `true`, the destination enters from the right;
    /// otherwise it enters from the left.
    pub fn new(from: W, to: W, forward: bool, now: Duration, timing: Timing) -> Self {
        let (strip, start, end) = if forward {
            (vec![from, to], 0.0, 1.0)
        } else {
            (vec![to, from], 1.0, 0.0)
        };
        Self {
            strip,
            camera: Camera::Eased(Animation::new(start, end, now, timing)),
            home: None,
        }
    }

    /// Starts an interactive drag from the active workspace.
    ///
    /// If `current` is still animating, the camera is positioned to continue
    /// from the workspace currently closest to the viewport center.
    pub fn grab(
        workspaces: Vec<W>,
        active: W,
        current: Option<&Self>,
        now: Duration,
    ) -> Option<Self> {
        let slot_of = |workspace: W| workspaces.iter().position(|&w| w == workspace);
        let mut camera = slot_of(active)? as f64;
        // Continue from the workspace most in view, so nothing jumps.
        if let Some(current) = current
            && let Some((workspace, offset)) = current
                .visible(now)
                .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            && let Some(slot) = slot_of(workspace)
        {
            camera = slot as f64 - offset;
        }
        let last = workspaces.len().saturating_sub(1) as f64;
        Some(Self {
            strip: workspaces,
            camera: Camera::Held(camera.clamp(0.0, last)),
            home: Some(active),
        })
    }

    /// Moves the held camera by a number of workspace slots.
    ///
    /// Non-finite movement is ignored. This method has no effect after the
    /// slide has been released or cancelled.
    pub fn drag(&mut self, slots: f64) {
        if let Camera::Held(raw) = &mut self.camera
            && slots.is_finite()
        {
            *raw += slots;
        }
    }

    /// Releases the drag and springs toward the nearest projected workspace.
    ///
    /// Returns the selected target workspace, or the existing target when the
    /// camera is not currently held.
    pub fn release(&mut self, velocity: f64, now: Duration, timing: Timing) -> Option<W> {
        let Camera::Held(_) = self.camera else {
            return self.target();
        };
        let position = self.position(now);
        let projected = position + velocity * RELEASE_PROJECTION.as_secs_f64();
        let slot = projected.round().clamp(0.0, self.last_slot());
        self.camera = Camera::Spring(Spring::new(position, slot, velocity, now, timing.duration));
        self.target()
    }

    /// Cancels the drag and springs back to the workspace active at grab time.
    pub fn cancel(&mut self, now: Duration, timing: Timing) {
        let Camera::Held(_) = self.camera else {
            return;
        };
        let position = self.position(now);
        let home = self
            .home
            .and_then(|home| self.strip.iter().position(|&w| w == home))
            .map_or_else(|| position.round(), |slot| slot as f64);
        self.camera = Camera::Spring(Spring::new(position, home, 0.0, now, timing.duration));
    }

    /// Returns the workspace targeted by the current animation.
    ///
    /// A held camera has no target and returns `None`.
    pub fn target(&self) -> Option<W> {
        let slot = match self.camera {
            Camera::Eased(animation) => animation.to(),
            Camera::Spring(spring) => spring.to(),
            Camera::Held(_) => return None,
        }
        .round();
        (slot >= 0.0)
            .then(|| self.strip.get(slot as usize).copied())
            .flatten()
    }

    /// Retargets the slide while preserving the camera's current position.
    ///
    /// Workspaces that are no longer visible are discarded so repeated
    /// interruptions do not grow the internal strip.
    pub fn retarget(&mut self, target: W, forward: bool, now: Duration, timing: Timing) {
        let mut camera = self.position(now);

        // Forget workspaces out of view, so interruptions never pile up.
        let mut kept = Vec::with_capacity(3);
        let mut first = None;
        for (slot, &workspace) in self.strip.iter().enumerate() {
            if (slot as f64 - camera).abs() < 1.0 {
                first.get_or_insert(slot);
                kept.push(workspace);
            }
        }
        match first {
            Some(first) => camera -= first as f64,
            // Unreachable while the camera stays within the strip, which
            // eased motion between slots does. Restart from the target.
            None => {
                kept.push(target);
                camera = 0.0;
            }
        }

        let slot = match kept.iter().position(|&workspace| workspace == target) {
            Some(slot) => slot,
            None if forward => {
                kept.push(target);
                kept.len() - 1
            }
            None => {
                kept.insert(0, target);
                camera += 1.0;
                0
            }
        };
        self.strip = kept;
        self.camera = Camera::Eased(Animation::new(camera, slot as f64, now, timing));
        self.home = None;
    }

    /// Returns visible workspaces and their signed offsets from the camera.
    ///
    /// Only workspaces less than one slot from the camera are returned.
    pub fn visible(&self, now: Duration) -> impl Iterator<Item = (W, f64)> + '_ {
        let camera = self.position(now);
        self.strip
            .iter()
            .enumerate()
            .map(move |(slot, &workspace)| (workspace, slot as f64 - camera))
            .filter(|(_, offset)| offset.abs() < 1.0)
    }

    /// Reports whether the camera is currently moving.
    pub fn is_moving(&self, now: Duration) -> bool {
        match self.camera {
            Camera::Eased(animation) => !animation.is_done(now),
            Camera::Spring(spring) => !spring.is_done(now),
            Camera::Held(_) => false,
        }
    }

    /// Reports whether a non-interactive slide has completed.
    ///
    /// A slide remains unfinished while the camera is held by an active drag.
    pub fn is_finished(&self, now: Duration) -> bool {
        match self.camera {
            Camera::Held(_) => false,
            _ => !self.is_moving(now),
        }
    }

    fn position(&self, now: Duration) -> f64 {
        match self.camera {
            Camera::Eased(animation) => animation.value(now),
            Camera::Spring(spring) => spring.value(now),
            Camera::Held(raw) => rubber_band(raw, self.last_slot()),
        }
    }

    fn last_slot(&self) -> f64 {
        self.strip.len().saturating_sub(1) as f64
    }
}

fn rubber_band(raw: f64, last: f64) -> f64 {
    let resist = |past: f64| RUBBER_BAND_LIMIT * past / (past + RUBBER_BAND_LIMIT);
    if raw < 0.0 {
        -resist(-raw)
    } else if raw > last {
        last + resist(raw - last)
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::Easing;

    const TIMING: Timing = Timing {
        duration: Duration::from_millis(250),
        easing: Easing::EaseOutCubic,
    };

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn offsets(slide: &WorkspaceSlide<u32>, now: Duration) -> Vec<(u32, f64)> {
        slide.visible(now).collect()
    }

    fn offset_of(slide: &WorkspaceSlide<u32>, workspace: u32, now: Duration) -> Option<f64> {
        slide
            .visible(now)
            .find(|&(w, _)| w == workspace)
            .map(|(_, offset)| offset)
    }

    /// Frame times of a display refreshing at `hz` over `span`.
    fn frames(hz: u64, span: Duration) -> impl Iterator<Item = Duration> {
        let end = span.as_nanos() as u64;
        (0..)
            .map(move |frame| Duration::from_nanos(frame * 1_000_000_000 / hz))
            .take_while(move |time| time.as_nanos() as u64 <= end)
    }

    #[test]
    fn forward_slides_in_from_the_right() {
        let slide = WorkspaceSlide::new(1, 2, true, ms(0), TIMING);
        assert_eq!(offsets(&slide, ms(0)), vec![(1, 0.0)]);
        let middle = offsets(&slide, ms(100));
        assert_eq!(middle.len(), 2);
        assert!(middle[0].1 < 0.0 && middle[1].1 > 0.0);
        assert_eq!(offsets(&slide, ms(250)), vec![(2, 0.0)]);
        assert!(slide.is_finished(ms(250)));
        assert_eq!(slide.target(), Some(2));
    }

    #[test]
    fn backward_slides_in_from_the_left() {
        let slide = WorkspaceSlide::new(2, 1, false, ms(0), TIMING);
        assert_eq!(offsets(&slide, ms(0)), vec![(2, 0.0)]);
        let left = offset_of(&slide, 1, ms(50)).unwrap();
        assert!(left < 0.0);
        assert_eq!(offsets(&slide, ms(300)), vec![(1, 0.0)]);
        assert_eq!(slide.target(), Some(1));
    }

    #[test]
    fn same_motion_at_any_refresh_rate() {
        let slide = WorkspaceSlide::new(1, 2, true, ms(0), TIMING);
        let span = ms(400);
        let mut done_at = Vec::new();
        for hz in [60, 144, 240] {
            let mut last = f64::NEG_INFINITY;
            let mut done = None;
            for time in frames(hz, span) {
                let camera = slide.position(time);
                assert!(camera >= last, "{hz} Hz went backwards at {time:?}");
                last = camera;
                if done.is_none() && slide.is_finished(time) {
                    done = Some(time);
                }
            }
            assert_eq!(last, 1.0, "{hz} Hz didn't finish");
            done_at.push(done.unwrap());
        }
        // Each finishes on its first frame at or after 250 ms.
        for time in done_at {
            assert!(time >= ms(250) && time < ms(250) + ms(17));
        }
        // Frames that coincide (every 1/12 s) show exactly the same thing.
        for n in 0..=4u64 {
            let at_60 = Duration::from_nanos(5 * n * 1_000_000_000 / 60);
            let at_144 = Duration::from_nanos(12 * n * 1_000_000_000 / 144);
            assert_eq!(at_60, at_144);
            assert_eq!(offsets(&slide, at_60), offsets(&slide, at_144));
        }
    }

    #[test]
    fn interrupting_keeps_everything_in_place() {
        let mut slide = WorkspaceSlide::new(1, 2, true, ms(0), TIMING);
        let before = offsets(&slide, ms(80));
        slide.retarget(3, true, ms(80), TIMING);
        assert_eq!(slide.target(), Some(3));
        let after = offsets(&slide, ms(80));
        for (workspace, offset) in before {
            let now = after.iter().find(|&&(w, _)| w == workspace).unwrap().1;
            assert!((now - offset).abs() < 1e-9, "workspace {workspace} jumped");
        }
        assert_eq!(offsets(&slide, ms(80 + 250)), vec![(3, 0.0)]);
    }

    #[test]
    fn reversing_glides_back() {
        let mut slide = WorkspaceSlide::new(1, 2, true, ms(0), TIMING);
        let one = offset_of(&slide, 1, ms(60)).unwrap();
        slide.retarget(1, false, ms(60), TIMING);
        assert_eq!(offset_of(&slide, 1, ms(60)), Some(one));
        assert_eq!(slide.target(), Some(1));
        let later = offset_of(&slide, 1, ms(120)).unwrap();
        assert!(later.abs() < one.abs());
        assert_eq!(offsets(&slide, ms(60 + 250)), vec![(1, 0.0)]);
    }

    #[test]
    fn backwards_interruption_inserts_on_the_left() {
        let mut slide = WorkspaceSlide::new(5, 6, true, ms(0), TIMING);
        let six = offset_of(&slide, 6, ms(100)).unwrap();
        slide.retarget(1, false, ms(100), TIMING);
        assert_eq!(offset_of(&slide, 6, ms(100)), Some(six));
        assert!(offset_of(&slide, 1, ms(100)).is_none_or(|offset| offset < 0.0));
        assert_eq!(offsets(&slide, ms(400)), vec![(1, 0.0)]);
    }

    #[test]
    fn repeated_interruptions_stay_small() {
        let mut slide = WorkspaceSlide::new(0, 1, true, ms(0), TIMING);
        for step in 1..50u32 {
            let now = ms(u64::from(step) * 20);
            slide.retarget(step + 1, true, now, TIMING);
            assert!(
                slide.strip.len() <= 3,
                "strip grew to {}",
                slide.strip.len()
            );
            assert!(offsets(&slide, now).len() <= 2);
        }
    }

    #[test]
    fn off_switches_at_once() {
        let slide = WorkspaceSlide::new(1, 2, true, ms(10), Timing::OFF);
        assert!(slide.is_finished(ms(10)));
        assert_eq!(offsets(&slide, ms(10)), vec![(2, 0.0)]);
    }

    fn grabbed(now: Duration) -> WorkspaceSlide<u32> {
        WorkspaceSlide::grab(vec![10, 11, 12, 13], 11, None, now).unwrap()
    }

    #[test]
    fn grab_holds_the_active_workspace() {
        let slide = grabbed(ms(0));
        assert_eq!(offsets(&slide, ms(0)), vec![(11, 0.0)]);
        assert!(!slide.is_moving(ms(0)));
        assert!(!slide.is_finished(ms(1000)));
        assert_eq!(slide.target(), None);
        assert!(WorkspaceSlide::grab(vec![1, 2], 3, None, ms(0)).is_none());
    }

    #[test]
    fn grab_catches_a_running_slide() {
        let running = WorkspaceSlide::new(11, 12, true, ms(0), TIMING);
        let before = offsets(&running, ms(60));
        let slide = WorkspaceSlide::grab(vec![10, 11, 12, 13], 12, Some(&running), ms(60)).unwrap();
        let after = offsets(&slide, ms(60));
        for (workspace, offset) in before {
            let now = after.iter().find(|&&(w, _)| w == workspace).unwrap().1;
            assert!((now - offset).abs() < 1e-9, "workspace {workspace} jumped");
        }
    }

    #[test]
    fn drag_follows_the_fingers() {
        let mut slide = grabbed(ms(0));
        slide.drag(0.25);
        assert_eq!(offsets(&slide, ms(0)), vec![(11, -0.25), (12, 0.75)]);
        slide.drag(-0.5);
        assert_eq!(offsets(&slide, ms(0)), vec![(10, -0.75), (11, 0.25)]);
        // It stays put while the fingers rest.
        assert_eq!(offsets(&slide, ms(500)), offsets(&slide, ms(0)));
        // Dragging across several workspaces.
        slide.drag(2.25);
        assert_eq!(offsets(&slide, ms(0)), vec![(13, 0.0)]);
    }

    #[test]
    fn rubber_bands_past_the_ends() {
        let mut slide = grabbed(ms(0));
        slide.drag(-3.0);
        let first = offset_of(&slide, 10, ms(0)).unwrap();
        assert!(first > 0.0 && first < RUBBER_BAND_LIMIT);
        slide.drag(-100.0);
        let further = offset_of(&slide, 10, ms(0)).unwrap();
        assert!(further > first && further < RUBBER_BAND_LIMIT);
        slide.drag(200.0);
        let last = offset_of(&slide, 13, ms(0)).unwrap();
        assert!(last < 0.0 && last > -RUBBER_BAND_LIMIT);
        // Released out there, it springs back to the end.
        assert_eq!(slide.release(0.0, ms(0), TIMING), Some(13));
        assert_eq!(offsets(&slide, ms(1000)), vec![(13, 0.0)]);
    }

    #[test]
    fn release_past_halfway_switches() {
        let mut slide = grabbed(ms(0));
        slide.drag(0.6);
        assert_eq!(slide.release(0.0, ms(0), TIMING), Some(12));
        assert!(slide.is_moving(ms(10)));
        // Continues from where the fingers left it.
        assert!((offset_of(&slide, 12, ms(0)).unwrap() - 0.4).abs() < 1e-9);
        assert!(slide.is_finished(ms(1000)));
        assert_eq!(offsets(&slide, ms(1000)), vec![(12, 0.0)]);
    }

    #[test]
    fn a_flick_switches_and_a_slow_drag_cancels() {
        let mut flick = grabbed(ms(0));
        flick.drag(-0.2);
        assert_eq!(flick.release(-3.0, ms(0), TIMING), Some(10));

        let mut slow = grabbed(ms(0));
        slow.drag(-0.3);
        assert_eq!(slow.release(-0.2, ms(0), TIMING), Some(11));
        assert_eq!(offsets(&slow, ms(1000)), vec![(11, 0.0)]);
    }

    #[test]
    fn cancel_returns_home() {
        let mut slide = grabbed(ms(0));
        slide.drag(0.9);
        slide.cancel(ms(0), TIMING);
        assert_eq!(slide.target(), Some(11));
        assert!((offset_of(&slide, 12, ms(0)).unwrap() - 0.1).abs() < 1e-9);
        assert_eq!(offsets(&slide, ms(1000)), vec![(11, 0.0)]);
    }

    #[test]
    fn retarget_during_a_spring_is_continuous() {
        let mut slide = grabbed(ms(0));
        slide.drag(0.7);
        slide.release(2.0, ms(0), TIMING);
        let before = offsets(&slide, ms(40));
        slide.retarget(13, true, ms(40), TIMING);
        let after = offsets(&slide, ms(40));
        for (workspace, offset) in before {
            let now = after.iter().find(|&&(w, _)| w == workspace).unwrap().1;
            assert!((now - offset).abs() < 1e-9, "workspace {workspace} jumped");
        }
        assert_eq!(offsets(&slide, ms(40 + 250)), vec![(13, 0.0)]);
    }

    #[test]
    fn release_with_motion_off_lands_at_once() {
        let mut slide = grabbed(ms(0));
        slide.drag(0.8);
        assert_eq!(slide.release(0.0, ms(5), Timing::OFF), Some(12));
        assert!(slide.is_finished(ms(5)));
        assert_eq!(offsets(&slide, ms(5)), vec![(12, 0.0)]);
    }
}
