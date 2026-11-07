//! Frame benchmarks: what one frame costs Orbit, as numbers.
//!
//! Ignored by default, because they only mean something in a release
//! build:
//!
//! ```sh
//! cargo test --release bench -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Each prints its results; `docs/milestone-15.md` records them. A change
//! to a per-frame path should leave these numbers no worse. On a CPU with
//! performance and efficiency cores, pin the run to one core (`taskset -c`
//! on the test binary) before comparing times: the core type alone moves
//! them by about 1.7×.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{Bind, Offscreen, damage::OutputDamageTracker, pixman::PixmanRenderer},
    },
    output::Output,
    reexports::pixman::Image,
};

use super::{Fixture, map_window};
use crate::render::OrbitRenderElement;

/// Counts the allocations made on each thread, so a benchmark sees its
/// own and not those of tests running beside it.
struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: forwards every call to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: the caller upholds `alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

/// Frames measured per run, after as many to warm up.
const FRAMES: usize = 300;

/// The cost of one frame on one output.
#[derive(Default, Clone, Copy)]
struct Sample {
    /// Building the element list (`render_elements`).
    elements: Duration,
    /// Building it, damage tracking and drawing, and `post_render`.
    total: Duration,
    allocations: u64,
    /// Those made by Orbit's own code: all of them but the damage
    /// tracker's and the renderer's (Smithay's).
    orbit_allocations: u64,
}

/// Opens `count` windows, alternating between outputs A and B, and waits
/// for their open animations to finish.
fn fixture_with_windows(count: usize) -> (Fixture, super::TestClient, Vec<super::TestWindow>) {
    let mut fixture = Fixture::with_outputs(&["A", "B"]);
    let outputs = [fixture.output("A"), fixture.output("B")];
    let mut client = fixture.client();
    let mut windows = Vec::with_capacity(count);
    for i in 0..count {
        fixture.state.set_focused_output(&outputs[i % 2]);
        windows.push(map_window(&mut fixture, &mut client));
    }
    fixture.advance(Duration::from_secs(1));
    (fixture, client, windows)
}

/// Draws one frame of `output` the way a real backend does, into an
/// offscreen image on the CPU renderer.
struct Drawer {
    renderer: PixmanRenderer,
    elements: Vec<OrbitRenderElement<PixmanRenderer>>,
    targets: Vec<(Output, Image<'static, 'static>, OutputDamageTracker)>,
}

impl Drawer {
    fn new(outputs: &[Output]) -> Self {
        let mut renderer = PixmanRenderer::new().expect("pixman renderer");
        let targets = outputs
            .iter()
            .map(|output| {
                let mode = output.current_mode().expect("a mode");
                let image = renderer
                    .create_buffer(Fourcc::Abgr8888, (mode.size.w, mode.size.h).into())
                    .expect("offscreen image");
                (
                    output.clone(),
                    image,
                    OutputDamageTracker::from_output(output),
                )
            })
            .collect();
        Self {
            renderer,
            elements: Vec::new(),
            targets,
        }
    }

    fn frame(&mut self, fixture: &mut Fixture, index: usize) -> Sample {
        let (output, image, damage) = &mut self.targets[index];
        let state = &mut fixture.state;
        let allocations_before = allocations();
        let start = Instant::now();

        state.frames.frame_time = state.next_frame_time(output);
        state.tick_animations(output);
        state.render_elements(&mut self.renderer, output, &mut self.elements);
        let elements = start.elapsed();
        let drawing_from = allocations();
        let states = {
            let mut framebuffer = self.renderer.bind(image).expect("bind");
            damage
                .render_output(
                    &mut self.renderer,
                    &mut framebuffer,
                    1,
                    &self.elements,
                    [0.0, 0.0, 0.0, 1.0],
                )
                .expect("render")
                .states
        };
        let drawing = allocations() - drawing_from;
        self.elements.clear();
        state.post_render(output, Some(&states));

        let all = allocations() - allocations_before;
        Sample {
            elements,
            total: start.elapsed(),
            allocations: all,
            orbit_allocations: all - drawing,
        }
    }
}

struct Summary {
    elements: Duration,
    total_median: Duration,
    total_p99: Duration,
    allocations: f64,
    orbit_allocations: f64,
}

fn summarize(samples: &[Sample]) -> Summary {
    let mut totals: Vec<Duration> = samples.iter().map(|s| s.total).collect();
    totals.sort();
    let mut elements: Vec<Duration> = samples.iter().map(|s| s.elements).collect();
    elements.sort();
    Summary {
        elements: elements[elements.len() / 2],
        total_median: totals[totals.len() / 2],
        total_p99: totals[totals.len() * 99 / 100],
        allocations: samples.iter().map(|s| s.allocations).sum::<u64>() as f64
            / samples.len() as f64,
        orbit_allocations: samples.iter().map(|s| s.orbit_allocations).sum::<u64>() as f64
            / samples.len() as f64,
    }
}

fn report(name: &str, windows: usize, summary: &Summary) {
    println!(
        "{name:<8} {windows:>3} windows: elements {:>9.1?}, frame {:>9.1?} (p99 {:>9.1?}), \
         {:>7.1} allocations/frame ({:.1} in Orbit)",
        summary.elements,
        summary.total_median,
        summary.total_p99,
        summary.allocations,
        summary.orbit_allocations,
    );
}

/// Frames where nothing changed, with 1, 20 and 100 windows on two
/// outputs. Both outputs are drawn each round, as an animation or the
/// cursor would make them.
#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn bench_idle_frames() {
    for count in [1, 20, 100] {
        let (mut fixture, _client, _windows) = fixture_with_windows(count);
        let outputs = [fixture.output("A"), fixture.output("B")];
        let mut drawer = Drawer::new(&outputs);
        let mut samples = Vec::with_capacity(FRAMES * 2);
        for round in 0..FRAMES * 2 {
            for index in 0..outputs.len() {
                let sample = drawer.frame(&mut fixture, index);
                if round >= FRAMES {
                    samples.push(sample);
                }
            }
        }
        report("idle", count, &summarize(&samples));
    }
}

/// Frames where one window on output A drew something new, as a playing
/// video would.
#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn bench_one_window_changing() {
    for count in [1, 20, 100] {
        let (mut fixture, mut client, windows) = fixture_with_windows(count);
        let outputs = [fixture.output("A"), fixture.output("B")];
        let mut drawer = Drawer::new(&outputs);
        let mut samples = Vec::with_capacity(FRAMES);
        for round in 0..FRAMES * 2 {
            client.attach(&windows[0], (640, 480));
            fixture.roundtrip(&mut client);
            let sample = drawer.frame(&mut fixture, 0);
            if round >= FRAMES {
                samples.push(sample);
            }
        }
        report("changing", count, &summarize(&samples));
    }
}

/// How often each output is redrawn while a window on output A commits at
/// 60 Hz for a second, and nothing happens on output B.
#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn bench_redraws_while_one_output_plays_video() {
    let (mut fixture, mut client, windows) = fixture_with_windows(2);
    let before = (fixture.renders("A"), fixture.renders("B"));
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        client.attach(&windows[0], (640, 480));
        fixture.roundtrip(&mut client);
        fixture.advance(Duration::from_micros(16_667));
    }
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "video on A: output A redrawn {:.0}/s, output B redrawn {:.0}/s",
        f64::from(fixture.renders("A") - before.0) / seconds,
        f64::from(fixture.renders("B") - before.1) / seconds,
    );
}

/// Not a benchmark: a frame where nothing changed allocates nothing in
/// Orbit's code, however many windows there are. (Smithay's damage
/// tracker and renderer allocate a few times per frame, independently of
/// the window count; they aren't counted.)
#[test]
fn idle_frames_allocate_nothing_in_orbit() {
    let (mut fixture, _client, _windows) = fixture_with_windows(6);
    let outputs = [fixture.output("A"), fixture.output("B")];
    let mut drawer = Drawer::new(&outputs);
    // The first frames create the buffers that are then reused.
    for _ in 0..3 {
        for index in 0..outputs.len() {
            drawer.frame(&mut fixture, index);
        }
    }
    for index in 0..outputs.len() {
        let sample = drawer.frame(&mut fixture, index);
        assert_eq!(sample.orbit_allocations, 0, "output {index}");
    }
}
