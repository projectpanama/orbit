use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: orbit [--nested] [--config <path>]... [-- <command> [args...]]
       orbit --check-config [--config <path>]...

Without --nested, orbit takes over the display and input devices; run it
from a TTY (or a display manager) with no other desktop on that seat.

Options:
    --nested           Run as a window inside an existing Wayland or X11 session
    --check-config     Check the configuration files and exit. Invalid values are
                       reported and the status is non-zero if a file can't be used
    --config <path>    Load the given configuration file. Multiple files can be
                       specified, and they are loaded in order. Later files override
                       earlier ones
    -h, --help         Show this help message
    -v, --version      Show the version of Orbit

Anything after `--` is spawned as a client once the compositor is up.

Default bindings (Mod is Super, or Alt when nested; configurable):
    Mod+Q                 Close the focused window
    Mod+M                 Toggle maximize
    Mod+F                 Toggle fullscreen
    Mod+H / Mod+Shift+H   Minimize / restore the last minimized window
    Mod+Tab               Focus the next window
    Mod+Shift+Left/Right  Tile to the left/right half
    Mod+Arrows            Move the focused window
    Mod+Ctrl+Arrows       Resize the focused window
    Mod+1..9              Show workspace 1-9 on the focused monitor
    Mod+Page_Up/Down      Previous/next workspace
    Mod+Shift+1..9        Move the focused window to workspace 1-9
    Mod+Shift+Page_Up/Dn  Move the focused window to the prev/next workspace
    Mod+Comma/Period      Focus the monitor to the left/right
    Mod+Shift+Comma/Prd   Move the focused window to the monitor left/right
    Mod+Return            Open a terminal (foot)
    Mod+Shift+E           Quit Orbit
    Ctrl+Alt+F1..F12      Switch virtual terminal (native session only)
    Mod+Left drag         Move a window
    Mod+Right drag        Resize a window
";

pub struct Args {
    pub nested: bool,
    pub check_config: bool,
    pub configs: Vec<PathBuf>,
    pub command: Vec<String>,
}

pub fn parse() -> Result<Args, String> {
    parse_from(std::env::args().skip(1))
}

fn parse_from(mut iter: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args {
        nested: false,
        check_config: false,
        configs: Vec::new(),
        command: Vec::new(),
    };

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--nested" => args.nested = true,
            "--check-config" => args.check_config = true,
            "--config" => match iter.next() {
                Some(path) => args.configs.push(PathBuf::from(path)),
                None => return Err("--config needs a path".into()),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-v" | "--version" => {
                println!("Orbit {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--" => {
                args.command = iter.collect();
                break;
            }
            other => return Err(format!("Unknown argument: {other}")),
        }
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_from(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn configs_in_order() {
        let args = parse(&[
            "--config", "a.toml", "--nested", "--config", "b.toml", "--", "foot", "--config",
        ])
        .unwrap();

        assert!(args.nested);
        assert_eq!(
            args.configs,
            vec![PathBuf::from("a.toml"), PathBuf::from("b.toml")]
        );
        assert_eq!(args.command, vec!["foot".to_string(), "--config".into()]);
        assert!(parse(&["--config"]).is_err());
        assert!(!args.check_config);
        assert!(parse(&["--check-config"]).unwrap().check_config);
    }
}
