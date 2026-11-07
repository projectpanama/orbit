# Orbit

Orbit is a Wayland compositor for Linux, written in Rust using Smithay. It is
being developed by Melvin Otieno as part of **Project Panama**, an operating
system project whose planned home is [projectpanama.org](https://projectpanama.org).

Orbit combines floating windows with edge snapping, independent workspaces on
each monitor, and an animated window overview. Desktop components communicate
with it through Wayland protocols, including two Orbit-specific protocols for
window management and overview integration.

**Status:** unreleased and under active development. Configuration and protocol
interfaces may change before the first release.

## Features

- **Two ways to run:** a native DRM/KMS session with libseat and libinput, or a
  nested window inside an existing Wayland or X11 desktop.
- **Window management:** move, resize, maximize, fullscreen, minimize, and tile
  windows to either half of a monitor. Dragging near window and monitor edges
  snaps the window; dropping at the top maximizes it, and dropping at either
  side tiles it, with a preview of the destination.
- **Per-monitor workspaces:** each output has its own active workspace, focus
  history, and trailing empty workspace. Workspaces survive monitor hotplug.
- **Window overview:** live windows scale into a grid over a dimmed background.
  Open it with `Mod+W`, select a window with a click, or leave with Escape or a
  click on the background.
- **Touchpad gestures:** horizontal three-finger workspace swipes and vertical
  four-finger overview swipes by default. Workspace switching also supports
  `Mod` plus smooth horizontal scrolling.
- **Configurable appearance and input:** server-side title bars, button layouts,
  colors, fonts, output scaling and placement, keyboard layouts, pointer settings,
  and animation timings.
- **Live configuration:** layered TOML files, automatic reload on save, and
  explicit reload through SIGHUP.
- **Desktop integration:** layer-shell panels and backgrounds, clipboard and
  primary selection, pointer drag and drop, session locking, idle notifications,
  input methods, output management, and output/window capture.
- **Rendering:** OpenGL ES, DMA-BUF client buffers, output damage tracking, and
  per-output frame scheduling driven by redraw requests and presentation.

## Build

Build on Linux with:

- Rust and Cargo. [rust-toolchain.toml](rust-toolchain.toml) pins Rust `1.99.1`
  and includes `rustfmt` and Clippy; rustup uses this toolchain automatically.
- A C compiler, linker, and `pkg-config`.
- Development libraries for **libudev, libseat, libinput, GBM, and libxkbcommon**.
  The pinned Smithay dependency uses libinput APIs through version 1.19.
- Graphics drivers providing EGL and OpenGL ES. Nested mode also needs the
  host desktop's Wayland or X11 runtime libraries.
- **Pixman** development libraries to build and run the compositor tests.

Package names vary by distribution. Both runtime backends are compiled into the
same binary, so a nested-only build still needs the native backend's libraries.
The first build fetches Cargo dependencies and the pinned Smithay Git revision.

From the repository root:

```sh
cargo build --locked --release --workspace
```

This produces `target/release/orbit` and `target/release/orbit-shell-debug`.
For development, omit `--release` and use the binaries under `target/debug`.

The default terminal binding launches `foot`; install it or configure a different
command. Title-bar font lookup uses `fc-match` from Fontconfig unless `font` is
set to a font file path. Native cursors use the installed Xcursor themes and
honor `XCURSOR_THEME` and `XCURSOR_SIZE`.

## Run

### Inside an existing desktop

```sh
./target/release/orbit --nested -- foot
```

Orbit opens a resizable window, initially 1280 × 800 logical pixels, representing
an output named `winit`. It uses **Alt** as `Mod` in nested mode. The host controls
the window's size; resize the host window to change Orbit's output dimensions.

For a development build:

```sh
cargo run --locked --bin orbit -- --nested -- foot
```

### As a native session

Run from a TTY with no other desktop occupying the seat:

```sh
./target/release/orbit -- foot
```

Native mode acquires the seat through libseat, selects the seat's primary GPU
(or the first available GPU), and discovers its connected monitors. Your login
session must provide access to the seat through a working libseat backend, such
as logind or seatd. **Super** is the default `Mod` key in this mode.

Orbit also needs a valid `XDG_RUNTIME_DIR` for its Wayland socket. If starting a
session without an existing D-Bus session bus, it can be launched with:

```sh
dbus-run-session ./target/release/orbit -- foot
```

Anything after `--` is a program and its arguments, launched once Orbit is
listening. Commands in `[startup].spawn` are launched first. These commands are
executed directly; use an explicit shell such as `sh -c` when you need shell
syntax.

Spawned clients receive Orbit's `WAYLAND_DISPLAY`. In native mode they also
receive `XDG_SESSION_TYPE=wayland` and, if unset, `XDG_CURRENT_DESKTOP=Orbit`.
Orbit attempts to update the D-Bus/systemd activation environment in native mode.
In nested mode, a terminal launched by Orbit is a convenient place to start
clients and the debug tool against the correct compositor.

### Command-line options

```text
orbit [--nested] [--config <path>]... [-- <command> [args...]]
orbit --check-config [--config <path>]...
```

| Option | Meaning |
| --- | --- |
| `--nested` | Run inside an existing Wayland or X11 session. |
| `--config <path>` | Use an explicit configuration file; repeat to layer files in order. |
| `--check-config` | Read configuration files, report problems, and exit without starting a compositor. |
| `-h`, `--help` | Show usage and default bindings. |
| `-v`, `--version` | Print the compositor package version. |

## Controls

`Mod` means **Super** in native mode and **Alt** in nested mode, unless overridden
by the `mod` configuration setting.

| Binding | Action |
| --- | --- |
| `Mod+Return` | Launch `foot`. |
| `Mod+Q` | Ask the focused window to close. |
| `Mod+Tab` | Cycle through visible windows on the focused monitor. |
| `Mod+M` | Toggle maximize. |
| `Mod+F` | Toggle fullscreen. |
| `Mod+H` / `Mod+Shift+H` | Minimize / restore the most recently minimized window. |
| `Mod+Shift+Left` / `Mod+Shift+Right` | Toggle left / right half tiling. |
| `Mod+Arrow` | Move the focused window. |
| `Mod+Ctrl+Arrow` | Resize the focused window. |
| `Mod+1` … `Mod+9` | Select a workspace on the focused monitor. |
| `Mod+Page_Up` / `Mod+Page_Down` | Select the previous / next workspace. |
| `Mod+Shift+1` … `Mod+Shift+9` | Move the focused window to a workspace. |
| `Mod+Shift+Page_Up` / `Mod+Shift+Page_Down` | Move the window to the previous / next workspace. |
| `Mod+Comma` / `Mod+Period` | Focus the monitor to the left / right. |
| `Mod+Shift+Comma` / `Mod+Shift+Period` | Move the window to the monitor on the left / right. |
| `Mod+W` | Toggle overview on the monitor under the pointer. |
| `Mod+Shift+E` | Quit Orbit. |
| `Ctrl+Alt+F1` … `Ctrl+Alt+F12` | Switch virtual terminal in native mode. |
| `Mod` + left-button drag | Move a window. |
| `Mod` + right-button drag | Resize a window. |

Clicking a window raises and focuses it. Server-side title bars support dragging,
double-click to maximize, and minimize/maximize/close buttons. Their borders
provide resize handles.

Each monitor maintains its own dynamic workspace list. Using the last empty
workspace creates another empty workspace at the end; unused empty workspaces
are removed as the list is normalized. Selecting a number beyond the current
list selects its last empty workspace. Switching one monitor never switches the
others. Moving a window to another workspace leaves the active workspace alone.
On monitor removal, nonempty workspaces migrate to another output when possible
and return when the original connector reconnects, within the same Orbit session.

Three-finger horizontal swipes switch workspaces. Four-finger swipes upward open
overview and downward close it. Gesture finger counts can be changed to three or
four; use different counts for workspace and overview gestures. In nested mode,
gesture availability depends on what the host forwards; `Mod` plus smooth
horizontal scrolling provides another way to swipe workspaces.

## Configuration

The default configuration layers these files, in order:

1. `/etc/orbit/config.toml`
2. `$XDG_CONFIG_HOME/orbit/config.toml`, or `~/.config/orbit/config.toml` when
   `XDG_CONFIG_HOME` is unset or not absolute.

On startup, if neither file exists, Orbit writes a commented default user
configuration. Existing files are never overwritten. Missing default files are
optional, and built-in defaults fill in unspecified settings.

Explicit `--config` arguments replace the default file selection:

```sh
./target/release/orbit --nested \
  --config ./base.toml --config ./local.toml -- foot
```

Later values override earlier ones. Tables merge recursively; arrays and scalar
values replace earlier values. Key bindings merge by trigger, with each later
binding replacing the earlier action as a whole.

See the [commented default configuration](crates/orbit-config/assets/config.toml)
for the available sections and their defaults. For example:

```toml
mod = "auto" # "auto", "super", or "alt"

[startup]
spawn = [["foot"]]

[binds]
"Mod+Return" = { spawn = ["foot"] }
"Mod+Space" = "toggle-overview"
"Mod+W" = "none" # Remove a default binding.
"Mod+Shift+2" = { move-to-workspace = 2 }

[input.keyboard]
layout = "us"
options = "caps:escape"
repeat-rate = 25
repeat-delay = 600

[input.touchpad]
natural-scroll = true
tap-to-click = true
disable-while-typing = true

[outputs."DP-1"]
mode = "2560x1440@143.912"
scale = 1.5
position = { x = 0, y = 0 }
transform = "normal"

[decorations]
height = 28
font = "sans-serif"
font-size = 13
font-weight = 500
buttons = { left = [], right = ["minimize", "maximize", "close"] }

[decorations.colors]
active = "#33384a"
inactive = "#24262e"

[animations.workspace-switch]
duration-ms = 250
curve = "ease-out-cubic"

[overview]
dim = 0.3

[gestures.workspace-swipe]
fingers = 3
sensitivity = 1.0
mod-scroll = true

[gestures.overview-swipe]
fingers = 4
```

Output sections match either a connector such as `DP-1` or a monitor's
`"make model serial"` description; the connector entry takes precedence. Positions
use logical pixels. Without a position, outputs are arranged side by side;
overlapping configured positions fall back to automatic placement. Native mode
automatically chooses a display mode and a density-based scale unless overridden.
Explicit scales range from `0.25` to `8.0` and are rounded to increments of 1/120;
`scale = "auto"` selects automatic scaling. Use `[outputs."winit"]` to set the
nested output's scale.

Bindings accept `Mod`, `Shift`, `Ctrl`, `Alt`, and `Super` modifiers and XKB key
names. Their values can be named actions, spawn commands, numbered workspace
actions, or `"none"`. Bindings are disabled while locked unless explicitly marked
`allow-when-locked = true`, for example:

```toml
[binds]
"XF86AudioRaiseVolume" = { spawn = ["wpctl", "set-volume", "@DEFAULT_AUDIO_SINK@", "5%+"], allow-when-locked = true }
```

`wpctl` in this example is an external command. Animation settings support
`linear`, `ease-out-quad`, `ease-out-cubic`, and `ease-out-expo` curves. Set
`[animations] off = true` to disable animations, or disable an individual
animation with its own `off = true`.

### Validate and reload

```sh
./target/release/orbit --check-config
./target/release/orbit --check-config --config ./local.toml
```

The checker exits nonzero for unreadable or unusable files. Invalid individual
values may instead produce warnings and fall back to defaults, so read its
output even when the exit status is zero.

Orbit watches the selected files' directories and reloads after edits settle.
Send SIGHUP to a running Orbit process to request a reload explicitly. Invalid
edits retain the affected file's last valid configuration. Most settings apply
live; startup commands run only on startup, and changing `[desktop-integration]`
requires a restart.

## Desktop and protocol integration

Orbit provides the compositor side of desktop services. Panels, backgrounds,
notifications, launchers, lock screens, and input-method clients run separately
and can be started through `[startup].spawn`.

| Area | Protocols and behavior |
| --- | --- |
| Application windows | `xdg-shell`, `xdg-decoration`, and `xdg-activation`. |
| Panels and backgrounds | `wlr-layer-shell`, including exclusive zones, popups, and keyboard focus. |
| Workspaces | `ext-workspace-v1`: per-output groups, workspace state, and activation. |
| Window discovery | `ext-foreign-toplevel-list-v1`: identifiers, titles, and application IDs. |
| Window actions | [`orbit-toplevel-management-v1`](protocols/orbit-toplevel-management-v1.xml): activation, workspace moves, close, minimize, maximize, and state reporting. |
| Overview | [`orbit-overview-v1`](protocols/orbit-overview-v1.xml): per-output open/close requests, animation progress, and window layout. |
| Monitor configuration | `wlr-output-management` interface version 4: modes, position, scale, transform, and enable/disable. |
| Capture | `ext-image-capture-source-v1` and `ext-image-copy-capture-v1` for output and individual-window capture into shared memory. |
| Session and input | `ext-session-lock-v1`, `ext-idle-notify-v1`, idle inhibition, `text-input-v3`, and `input-method-v2`. |
| Buffers and presentation | Shared memory, Linux DMA-BUF, fractional scale, viewporter, single-pixel buffers, presentation feedback, and cursor shapes. |
| Sandboxed connections | `wp-security-context-v1` marks clients whose access to privileged globals is restricted. |

Both Orbit-specific protocols currently use **interface version 1**. Rust client
and server bindings are generated from the XML by
[`orbit-protocols`](crates/orbit-protocols), using its `client` and `server`
features. A shell can draw overview labels and workspace controls on layer
surfaces while following the progress and layout supplied by Orbit.

Clients connected through a Wayland security context cannot see privileged
globals such as layer shell, session lock, input method, desktop management,
output management, or capture. Ordinary clients connected directly to Orbit's
socket are trusted for these purposes; there is no per-application permission
prompt.

Setting `[desktop-integration] off = true` disables workspace, foreign-toplevel,
Orbit management/overview, output-management, and capture globals after restart.
It does not disable layer shell or session locking.

Output changes made through output management are runtime overrides, not writes
to the TOML files. Editing the `[outputs]` configuration reapplies it and clears
those overrides. Save persistent monitor settings in the configuration file.

## Shell debug tool

`orbit-shell-debug` exercises desktop integration entirely over Wayland. Run it
from a terminal inside Orbit, or set `WAYLAND_DISPLAY` to Orbit's socket when
launching it from elsewhere.

```sh
./target/release/orbit-shell-debug outputs
./target/release/orbit-shell-debug workspaces
./target/release/orbit-shell-debug windows
./target/release/orbit-shell-debug overview winit open
./target/release/orbit-shell-debug screenshot winit screenshot.ppm
```

Replace `winit` with a name from `outputs` in native mode. Screenshots are PPM
images and do not include the pointer. Additional commands are:

```text
switch <output> <workspace>
activate <window-id>
move <window-id> <output> <workspace>
close <window-id>
minimize <window-id>
unminimize <window-id>
maximize <window-id>
unmaximize <window-id>
state <window-id>
overview <output> open|close|toggle
```

Workspace names come from `workspaces`; window identifiers come from `windows`.
Use `orbit-shell-debug --help` for the command reference. Commands that need
desktop-integration globals require that integration to be enabled and the
client to be trusted.

## Development

| Path | Responsibility |
| --- | --- |
| [`src/backend`](src/backend) | Native DRM/GBM/libseat/libinput, monitor discovery, and nested winit backend. |
| [`src/state.rs`](src/state.rs) | Compositor state, Wayland socket, event dispatch, and client spawning. |
| [`src/render.rs`](src/render.rs) | Scene composition, damage-related refresh, animations, and presentation feedback. |
| [`src/window`](src/window) and [`src/window.rs`](src/window.rs) | Layout changes, move/resize grabs, and server-side decorations. |
| [`src/workspaces.rs`](src/workspaces.rs) and [`src/outputs`](src/outputs) | Workspace visibility, output layout, configuration, and scaling. |
| [`src/input`](src/input) and [`src/input.rs`](src/input.rs) | Keyboard actions, pointer handling, gestures, cursor themes, and input methods. |
| [`src/shell`](src/shell) and [`src/protocols`](src/protocols) | Layer shell, locking, idle, capture, overview, and desktop protocols. |
| [`crates/orbit-core`](crates/orbit-core) | Dependency-free workspace, geometry, snapping, animation, overview, and frame-scheduling logic. |
| [`crates/orbit-config`](crates/orbit-config) | TOML parsing, defaults, validation, layering, and bindings. |
| [`crates/orbit-protocols`](crates/orbit-protocols) and [`protocols`](protocols) | Shared Rust bindings and custom Wayland protocol definitions. |
| [`tools/orbit-shell-debug`](tools/orbit-shell-debug) | Example/debug desktop client. |
| [`src/tests`](src/tests) | Headless compositor tests driven by real Wayland clients over socket pairs. |

Useful checks from the repository root:

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
```

The headless test backend uses virtual outputs and optionally Pixman; it does
not need a running display server or ownership of a physical seat. Tests cover
window geometry, output hotplug, workspace behavior, client isolation, reloads,
render scheduling, capture, session services, and desktop protocols.

Ignored benchmarks measure idle frames, changing windows, and redraw scheduling:

```sh
cargo test --locked --release --bin orbit tests::bench -- --ignored --nocapture
```

Tracy instrumentation is enabled with the `profile-with-tracy` feature:

```sh
cargo run --locked --release --bin orbit --features profile-with-tracy -- --nested -- foot
```

Logs go to journald when `JOURNAL_STREAM` is present and journald is available;
otherwise they go to stderr. Debug builds include more detailed logs than
release builds. Set `RUST_BACKTRACE=1` when investigating a panic.

## Current limitations

- Native mode drives one GPU. Connectors on additional GPUs are not managed.
- There is no Xwayland integration for X11 applications. Running Orbit nested
  on an X11 host still serves Wayland applications inside Orbit.
- Touchscreen and tablet input are not implemented. Pointer drag and drop does
  not render a drag icon.
- Capture currently uses ARGB8888/XRGB8888 shared-memory buffers with a pixel
  readback; DMA-BUF capture buffers and cursor capture are not implemented.
- Adaptive sync is not implemented. In nested mode the host controls output
  size, and the nested output cannot be rotated or disabled through output
  management.
- The headless backend is available only to tests, not as a command-line mode.

## License

The compositor is licensed under **GNU GPLv3**; see [LICENSE](LICENSE).
The two custom protocol XML files in [protocols](protocols) carry their own
**MIT license** notices, allowing clients and shells to reuse those definitions
under those terms. Dependencies retain their respective licenses.
