const USAGE: &str = "\
Usage: orbit [--nested] [-- <command> [args...]]

Without --nested, orbit takes over the display and input devices; run it
from a TTY (or a display manager) with no other desktop on that seat.

Options:
    --nested           Run as a window inside an existing Wayland or X11 session
    -h, --help         Show this help message

Anything after `--` is spawned as a client once the compositor is up.
";

pub struct Args {
    pub nested: bool,
    pub command: Vec<String>,
}

pub fn parse() -> Result<Args, String> {
    let mut args = Args {
        nested: false,
        command: Vec::new(),
    };

    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--nested" => args.nested = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--" => {
                args.command = iter.collect();
                break;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(args)
}
