//! A minimal desktop-shell client for Orbit's desktop-integration
//! protocols. It talks to Orbit only over Wayland:
//!
//! - monitors from `wl_output`;
//! - workspaces from ext-workspace-v1;
//! - windows from ext-foreign-toplevel-list-v1;
//! - activating, moving, closing, minimizing and maximizing windows, and
//!   reading their state, with `orbit_toplevel_manager_v1` (generated from
//!   `protocols/orbit-toplevel-management-v1.xml`);
//! - the overview, with `orbit_overview_v1` (`protocols/orbit-overview-v1.xml`);
//! - screenshots, with ext-image-capture-source-v1 and
//!   ext-image-copy-capture-v1 into a wl_shm buffer.

use std::{collections::HashMap, io::Read as _, os::fd::AsFd as _, process::ExitCode};

use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    backend::ObjectId,
    event_created_child,
    protocol::{wl_buffer, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool},
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    },
    workspace::v1::client::{
        ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
        ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
        ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
    },
};
use wayland_protocols::ext::{
    image_capture_source::v1::client::{
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};

use self::overview_protocol::{
    orbit_overview_manager_v1::OrbitOverviewManagerV1,
    orbit_overview_v1::{self, OrbitOverviewV1},
};

use orbit_protocols::overview::client as overview_protocol;

use self::generated::{
    orbit_toplevel_manager_v1::{self, OrbitToplevelManagerV1},
    orbit_toplevel_state_v1::{self, OrbitToplevelStateV1},
};

use orbit_protocols::toplevel_management::client as generated;

const USAGE: &str = "\
Usage: orbit-shell-debug <command>

Commands:
    outputs                                  List monitors
    workspaces                               List each monitor's workspaces (* = shown)
    switch <output> <workspace>              Show a workspace on a monitor
    windows                                  List windows
    activate <window-id>                     Show, raise and focus a window
    move <window-id> <output> <workspace>    Move a window to a workspace
    overview <output> open|close|toggle      Animate the overview, print progress and layout
    screenshot <output> <file.ppm>           Capture a monitor (ext-image-copy-capture)
    close <window-id>                        Ask a window to close
    minimize|unminimize <window-id>          Minimize or restore a window
    maximize|unmaximize <window-id>          Maximize or restore a window
    state <window-id>                        Print a window's state

<output> is a monitor name from `outputs`, <workspace> a workspace name
from `workspaces`, and <window-id> an identifier from `windows`.

Connects to $WAYLAND_DISPLAY, which must be Orbit.";

#[derive(Default)]
struct Output {
    name: String,
    description: String,
    size: (i32, i32),
    refresh_mhz: i32,
    scale: i32,
}

#[derive(Default)]
struct Group {
    outputs: Vec<ObjectId>,
    workspaces: Vec<ObjectId>,
}

struct Workspace {
    handle: ExtWorkspaceHandleV1,
    name: String,
    coordinates: Vec<u32>,
    active: bool,
}

struct Toplevel {
    handle: ExtForeignToplevelHandleV1,
    identifier: String,
    title: String,
    app_id: String,
    closed: bool,
}

#[derive(Default)]
struct State {
    outputs: HashMap<ObjectId, Output>,
    output_objects: HashMap<ObjectId, wl_output::WlOutput>,
    seat: Option<wl_seat::WlSeat>,
    workspace_manager: Option<ExtWorkspaceManagerV1>,
    toplevel_list: Option<ExtForeignToplevelListV1>,
    toplevel_manager: Option<OrbitToplevelManagerV1>,
    groups: HashMap<ObjectId, Group>,
    /// In the order they were announced.
    workspaces: Vec<(ObjectId, Workspace)>,
    toplevels: Vec<Toplevel>,
    /// From the last `orbit_toplevel_state_v1.state`.
    toplevel_state: Option<Vec<&'static str>>,
    overview_manager: Option<OrbitOverviewManagerV1>,
    /// Progress values received, in order.
    overview_progress: Vec<f64>,
    /// The windows of the last complete layout: identifier and rect.
    overview_layout: Vec<(String, (i32, i32, i32, i32))>,
    overview_pending: Vec<(String, (i32, i32, i32, i32))>,
    shm: Option<wl_shm::WlShm>,
    output_sources: Option<ExtOutputImageCaptureSourceManagerV1>,
    copy_manager: Option<ExtImageCopyCaptureManagerV1>,
    capture_size: Option<(u32, u32)>,
    capture_done: bool,
    capture_stopped: bool,
    /// `Some(true)` ready, `Some(false)` failed.
    capture_result: Option<bool>,
}

impl State {
    fn wl_output_named(&self, name: &str) -> Result<wl_output::WlOutput, String> {
        self.outputs
            .iter()
            .find(|(_, o)| o.name == name)
            .and_then(|(id, _)| self.output_objects.get(id).cloned())
            .ok_or_else(|| format!("no monitor named `{name}`"))
    }

    fn output_name(&self, id: &ObjectId) -> &str {
        self.outputs.get(id).map_or("?", |o| o.name.as_str())
    }

    fn workspace(&self, id: &ObjectId) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|(w, _)| w == id)
            .map(|(_, w)| w)
    }

    fn workspace_mut(&mut self, id: &ObjectId) -> Option<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|(w, _)| w == id)
            .map(|(_, w)| w)
    }

    /// Each group's output name and its workspaces in order.
    fn groups_by_output(&self) -> Vec<(String, Vec<&Workspace>)> {
        let mut groups: Vec<(String, Vec<&Workspace>)> = self
            .groups
            .values()
            .map(|group| {
                let name = group
                    .outputs
                    .iter()
                    .map(|id| self.output_name(id))
                    .collect::<Vec<_>>()
                    .join(",");
                let mut workspaces: Vec<&Workspace> = group
                    .workspaces
                    .iter()
                    .filter_map(|id| self.workspace(id))
                    .collect();
                workspaces.sort_by(|a, b| a.coordinates.cmp(&b.coordinates));
                (name, workspaces)
            })
            .collect();
        groups.sort_by(|a, b| a.0.cmp(&b.0));
        groups
    }

    fn find_workspace(&self, output: &str, name: &str) -> Result<&Workspace, String> {
        let groups = self.groups_by_output();
        let (_, workspaces) = groups
            .iter()
            .find(|(names, _)| names.split(',').any(|n| n == output))
            .ok_or_else(|| format!("no monitor named `{output}` has workspaces"))?;
        workspaces
            .iter()
            .find(|ws| ws.name == name)
            .copied()
            .ok_or_else(|| format!("`{output}` has no workspace named `{name}`"))
    }

    fn find_toplevel(&self, identifier: &str) -> Result<&Toplevel, String> {
        self.toplevels
            .iter()
            .find(|t| !t.closed && t.identifier == identifier)
            .ok_or_else(|| format!("no window with identifier `{identifier}`"))
    }

    fn toplevel_manager(&self) -> Result<&OrbitToplevelManagerV1, String> {
        self.toplevel_manager
            .as_ref()
            .ok_or_else(|| "orbit_toplevel_manager_v1 is not available".to_string())
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("orbit-shell-debug: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if matches!(args.as_slice(), [] | ["-h" | "--help"]) {
        println!("{USAGE}");
        return Ok(());
    }

    let conn = Connection::connect_to_env().map_err(|err| format!("can't connect: {err}"))?;
    let mut queue: EventQueue<State> = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State::default();
    // Globals, then their initial state.
    roundtrip(&mut queue, &mut state)?;
    roundtrip(&mut queue, &mut state)?;

    match args.as_slice() {
        ["outputs"] => {
            let mut outputs: Vec<&Output> = state.outputs.values().collect();
            outputs.sort_by(|a, b| a.name.cmp(&b.name));
            for o in outputs {
                println!(
                    "{}\t{}x{}@{:.3}Hz\tscale {}\t{}",
                    o.name,
                    o.size.0,
                    o.size.1,
                    f64::from(o.refresh_mhz) / 1000.0,
                    o.scale,
                    o.description
                );
            }
        }
        ["workspaces"] => {
            require(&state.workspace_manager, "ext_workspace_manager_v1")?;
            for (output, workspaces) in state.groups_by_output() {
                let list: Vec<String> = workspaces
                    .iter()
                    .map(|ws| {
                        let marker = if ws.active { "*" } else { "" };
                        format!("{}{marker}", ws.name)
                    })
                    .collect();
                println!("{output}\t{}", list.join(" "));
            }
        }
        ["switch", output, name] => {
            let manager = require(&state.workspace_manager, "ext_workspace_manager_v1")?;
            state.find_workspace(output, name)?.handle.activate();
            manager.commit();
            roundtrip(&mut queue, &mut state)?;
        }
        ["windows"] => {
            require(&state.toplevel_list, "ext_foreign_toplevel_list_v1")?;
            for t in state.toplevels.iter().filter(|t| !t.closed) {
                println!("{}\t{}\t{}", t.identifier, t.app_id, t.title);
            }
        }
        ["activate", identifier] => {
            let seat = require(&state.seat, "wl_seat")?;
            let toplevel = state.find_toplevel(identifier)?;
            state.toplevel_manager()?.activate(&toplevel.handle, seat);
            roundtrip(&mut queue, &mut state)?;
        }
        ["move", identifier, output, name] => {
            let toplevel = state.find_toplevel(identifier)?;
            let workspace = state.find_workspace(output, name)?;
            state
                .toplevel_manager()?
                .move_to_workspace(&toplevel.handle, &workspace.handle);
            roundtrip(&mut queue, &mut state)?;
        }
        [
            action @ ("close" | "minimize" | "unminimize" | "maximize" | "unmaximize"),
            identifier,
        ] => {
            let manager = state.toplevel_manager()?;
            let toplevel = &state.find_toplevel(identifier)?.handle;
            match *action {
                "close" => manager.close(toplevel),
                "minimize" => manager.set_minimized(toplevel),
                "unminimize" => manager.unset_minimized(toplevel),
                "maximize" => manager.set_maximized(toplevel),
                _ => manager.unset_maximized(toplevel),
            }
            roundtrip(&mut queue, &mut state)?;
        }
        ["state", identifier] => {
            let manager = state.toplevel_manager()?;
            let object = manager.get_state(&state.find_toplevel(identifier)?.handle, &qh, ());
            roundtrip(&mut queue, &mut state)?;
            match state.toplevel_state.take() {
                Some(names) if names.is_empty() => println!("normal"),
                Some(names) => println!("{}", names.join(" ")),
                None => println!("closed"),
            }
            object.destroy();
        }
        ["overview", output, action @ ("open" | "close" | "toggle")] => {
            let manager = require(&state.overview_manager, "orbit_overview_manager_v1")?;
            let wl_output = state.wl_output_named(output)?;
            let overview = manager.get_overview(&wl_output, &qh, ());
            roundtrip(&mut queue, &mut state)?;
            let open = match *action {
                "open" => true,
                "close" => false,
                _ => state.overview_progress.last().copied().unwrap_or(0.0) < 0.5,
            };
            if open {
                overview.open();
            } else {
                overview.close();
            }
            // Watch the animation for a moment.
            for _ in 0..40 {
                roundtrip(&mut queue, &mut state)?;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            let progress: Vec<String> = state
                .overview_progress
                .iter()
                .map(|p| format!("{p:.2}"))
                .collect();
            println!("progress\t{}", progress.join(" "));
            for (identifier, (x, y, w, h)) in &state.overview_layout {
                println!("window\t{identifier}\t{x},{y}\t{w}x{h}");
            }
            overview.destroy();
        }
        ["screenshot", output, path] => {
            let sources = require(
                &state.output_sources,
                "ext_output_image_capture_source_manager_v1",
            )?;
            let copy = require(&state.copy_manager, "ext_image_copy_capture_manager_v1")?;
            let shm = require(&state.shm, "wl_shm")?.clone();
            let wl_output = state.wl_output_named(output)?;
            let source = sources.create_source(&wl_output, &qh, ());
            let session = copy.create_session(&source, Options::empty(), &qh, ());
            roundtrip(&mut queue, &mut state)?;
            if state.capture_stopped {
                return Err("the compositor stopped the capture".into());
            }
            let (w, h) = state
                .capture_size
                .ok_or("no buffer size from the compositor")?;
            let (w, h) = (w as i32, h as i32);
            let len = w * h * 4;
            let tmp =
                std::env::temp_dir().join(format!("orbit-shell-debug-{}", std::process::id()));
            let mut file = std::fs::File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&tmp);
            file.set_len(len as u64).map_err(|e| e.to_string())?;
            let pool = shm.create_pool(file.as_fd(), len, &qh, ());
            let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, &qh, ());
            pool.destroy();
            let frame = session.create_frame(&qh, ());
            frame.attach_buffer(&buffer);
            frame.damage_buffer(0, 0, w, h);
            frame.capture();
            while state.capture_result.is_none() {
                queue
                    .blocking_dispatch(&mut state)
                    .map_err(|e| e.to_string())?;
            }
            if state.capture_result != Some(true) {
                return Err("the capture failed".into());
            }
            let mut pixels = Vec::new();
            use std::io::Seek as _;
            file.rewind().map_err(|e| e.to_string())?;
            file.read_to_end(&mut pixels).map_err(|e| e.to_string())?;
            // XRGB8888 is B, G, R, X in memory; PPM wants R, G, B.
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            for px in pixels.as_chunks::<4>().0 {
                ppm.extend_from_slice(&[px[2], px[1], px[0]]);
            }
            std::fs::write(path, ppm).map_err(|e| e.to_string())?;
            println!("{path}\t{w}x{h}");
            frame.destroy();
            session.destroy();
            source.destroy();
        }
        _ => return Err(format!("unknown command\n\n{USAGE}")),
    }
    Ok(())
}

fn roundtrip(queue: &mut EventQueue<State>, state: &mut State) -> Result<(), String> {
    queue
        .roundtrip(state)
        .map(drop)
        .map_err(|err| format!("connection error: {err}"))
}

fn require<'a, T>(global: &'a Option<T>, name: &str) -> Result<&'a T, String> {
    global
        .as_ref()
        .ok_or_else(|| format!("the compositor doesn't offer {name} (or not to this client)"))
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_output" if version >= 4 => {
                let output: wl_output::WlOutput = registry.bind(name, 4, qh, ());
                state.outputs.insert(output.id(), Output::default());
                state.output_objects.insert(output.id(), output);
            }
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind(name, 1, qh, ()));
            }
            "ext_workspace_manager_v1" => {
                state.workspace_manager = Some(registry.bind(name, 1, qh, ()));
            }
            "ext_foreign_toplevel_list_v1" => {
                state.toplevel_list = Some(registry.bind(name, 1, qh, ()));
            }
            "orbit_toplevel_manager_v1" => {
                state.toplevel_manager = Some(registry.bind(name, 1, qh, ()));
            }
            "orbit_overview_manager_v1" => {
                state.overview_manager = Some(registry.bind(name, 1, qh, ()));
            }
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "ext_output_image_capture_source_manager_v1" => {
                state.output_sources = Some(registry.bind(name, 1, qh, ()));
            }
            "ext_image_copy_capture_manager_v1" => {
                state.copy_manager = Some(registry.bind(name, 1, qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.get_mut(&output.id()) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => o.name = name,
            wl_output::Event::Description { description } => o.description = description,
            wl_output::Event::Scale { factor } => o.scale = factor,
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                refresh,
            } if flags.contains(wl_output::Mode::Current) => {
                o.size = (width, height);
                o.refresh_mhz = refresh;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } => {
                state.groups.insert(workspace_group.id(), Group::default());
            }
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                state.workspaces.push((
                    workspace.id(),
                    Workspace {
                        handle: workspace,
                        name: String::new(),
                        coordinates: Vec::new(),
                        active: false,
                    },
                ));
            }
            _ => {}
        }
    }

    event_created_child!(State, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        group: &ExtWorkspaceGroupHandleV1,
        event: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_workspace_group_handle_v1::Event::Removed = event {
            state.groups.remove(&group.id());
            return;
        }
        let Some(g) = state.groups.get_mut(&group.id()) else {
            return;
        };
        match event {
            ext_workspace_group_handle_v1::Event::OutputEnter { output } => {
                g.outputs.push(output.id())
            }
            ext_workspace_group_handle_v1::Event::OutputLeave { output } => {
                g.outputs.retain(|o| *o != output.id())
            }
            ext_workspace_group_handle_v1::Event::WorkspaceEnter { workspace } => {
                g.workspaces.push(workspace.id())
            }
            ext_workspace_group_handle_v1::Event::WorkspaceLeave { workspace } => {
                g.workspaces.retain(|w| *w != workspace.id())
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        workspace: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_workspace_handle_v1::Event::Removed = event {
            state.workspaces.retain(|(id, _)| *id != workspace.id());
            return;
        }
        let Some(ws) = state.workspace_mut(&workspace.id()) else {
            return;
        };
        match event {
            ext_workspace_handle_v1::Event::Name { name } => ws.name = name,
            ext_workspace_handle_v1::Event::Coordinates { coordinates } => {
                ws.coordinates = coordinates
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&c| u32::from_ne_bytes(c))
                    .collect();
            }
            ext_workspace_handle_v1::Event::State { state } => {
                ws.active = matches!(state, WEnum::Value(s)
                    if s.contains(ext_workspace_handle_v1::State::Active));
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(Toplevel {
                handle: toplevel,
                identifier: String::new(),
                title: String::new(),
                app_id: String::new(),
                closed: false,
            });
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(t) = state.toplevels.iter_mut().find(|t| t.handle == *handle) else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                t.identifier = identifier
            }
            ext_foreign_toplevel_handle_v1::Event::Title { title } => t.title = title,
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => t.app_id = app_id,
            ext_foreign_toplevel_handle_v1::Event::Closed => t.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<OrbitToplevelManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &OrbitToplevelManagerV1,
        _: orbit_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<OrbitToplevelStateV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &OrbitToplevelStateV1,
        event: orbit_toplevel_state_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let orbit_toplevel_state_v1::Event::State { state: bytes } = event else {
            return;
        };
        let names = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| match u32::from_ne_bytes(b) {
                0 => "activated",
                1 => "minimized",
                2 => "maximized",
                3 => "fullscreen",
                _ => "unknown",
            })
            .collect();
        state.toplevel_state = Some(names);
    }
}

impl Dispatch<OrbitOverviewV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &OrbitOverviewV1,
        event: orbit_overview_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            orbit_overview_v1::Event::Progress { value } => state.overview_progress.push(value),
            orbit_overview_v1::Event::Window {
                toplevel,
                x,
                y,
                width,
                height,
            } => {
                let identifier = state
                    .toplevels
                    .iter()
                    .find(|t| t.handle == toplevel)
                    .map_or_else(|| "?".to_string(), |t| t.identifier.clone());
                state
                    .overview_pending
                    .push((identifier, (x, y, width, height)));
            }
            orbit_overview_v1::Event::Done => {
                state.overview_layout = std::mem::take(&mut state.overview_pending);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event as E;
        match event {
            E::BufferSize { width, height } => state.capture_size = Some((width, height)),
            E::Done => state.capture_done = true,
            E::Stopped => state.capture_stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => state.capture_result = Some(true),
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => {
                state.capture_result = Some(false);
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ignore OrbitOverviewManagerV1);
wayland_client::delegate_noop!(State: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(State: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(State: ignore ExtOutputImageCaptureSourceManagerV1);
wayland_client::delegate_noop!(State: ignore ExtImageCaptureSourceV1);
wayland_client::delegate_noop!(State: ignore ExtImageCopyCaptureManagerV1);
