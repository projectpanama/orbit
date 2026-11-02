use ab_glyph::{FontArc, FontVec, VariableFont};
use orbit_config::DecorationConfig;
use smithay::reexports::calloop::{LoopHandle, channel};

use crate::state::Orbit;

/// Result of loading a title-bar font.
pub struct LoadedFont {
    /// Identifier of the request that produced this result.
    pub(super) request: u64,
    /// Loaded font or `None` when lookup, reading, or decoding failed.
    pub(super) font: Option<FontArc>,
}

/// Loads title bar fonts on a worker thread.
pub struct FontLoader {
    /// Channel used by worker threads to deliver completed font loads.
    pub(super) sender: channel::Sender<LoadedFont>,
    /// Monotonically increasing request identifier.
    ///
    /// Consumers use this value to ignore results from loads superseded by a
    /// later configuration request.
    pub(super) latest: u64,
}

impl FontLoader {
    /// Creates a font loader and registers its result channel with `handle`.
    ///
    /// Returns an error if the channel cannot be inserted into the event loop.
    pub fn new(handle: &LoopHandle<'static, Orbit>) -> anyhow::Result<Self> {
        let (sender, receiver) = channel::channel();
        handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg(loaded) = event {
                    state.title_font_loaded(loaded);
                }
            })
            .map_err(|err| err.error)?;
        Ok(Self { sender, latest: 0 })
    }

    /// Starts loading the font named by `config`.
    ///
    /// A font specification containing `/` is treated as a file path.
    /// Otherwise it is resolved as a fontconfig pattern through `fc-match`.
    ///
    /// The request supersedes earlier requests, whose results are ignored by
    /// the consumer. No worker is started when titles are disabled.
    pub fn request(&mut self, config: &DecorationConfig) {
        self.latest += 1;
        if !config.title {
            return;
        }
        let request = self.latest;
        let sender = self.sender.clone();
        let (spec, weight) = (config.font.clone(), config.font_weight);
        let spawned = std::thread::Builder::new()
            .name("orbit-font".into())
            .spawn(move || {
                let font = load_font(&spec, weight);
                if font.is_none() {
                    tracing::warn!(font = %spec, "no font for title bars; titles won't be shown");
                }
                // Fails only if Orbit is exiting.
                let _ = sender.send(LoadedFont { request, font });
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "can't start loading the title bar font; titles won't be shown");
        }
    }
}

/// Resolves, reads, and decodes a font specification.
///
/// Fontconfig supplies both the selected file and face index. For variable
/// fonts, `weight` is applied to the `wght` axis after decoding.
fn load_font(spec: &str, weight: Option<f64>) -> Option<FontArc> {
    let (path, index) = if spec.contains('/') {
        (std::path::PathBuf::from(spec), 0)
    } else {
        let mut pattern = spec.to_string();
        if let Some(weight) = weight
            && !pattern.contains("weight")
        {
            pattern.push_str(&format!(":weight={}", fc_weight(weight)));
        }
        let out = std::process::Command::new("fc-match")
            .args(["-f", "%{file}\n%{index}", &pattern])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let mut lines = text.lines();
        let file = lines.next().filter(|f| !f.is_empty())?;
        // The high bits name a variable font's instance; the face is the low 16.
        let index = lines
            .next()
            .and_then(|i| i.parse::<u32>().ok())
            .map_or(0, |i| i & 0xffff);
        (std::path::PathBuf::from(file), index)
    };
    let data = std::fs::read(&path)
        .map_err(|err| tracing::warn!(path = %path.display(), %err, "can't read title bar font"))
        .ok()?;
    let mut font = FontVec::try_from_vec_and_index(data, index)
        .map_err(|err| tracing::warn!(path = %path.display(), %err, "can't load title bar font"))
        .ok()?;
    if let Some(weight) = weight {
        // False (and harmless) when the font has no weight axis.
        font.set_variation(b"wght", weight as f32);
    }
    tracing::debug!(path = %path.display(), "title bar font");
    Some(FontArc::new(font))
}

/// Converts a CSS font weight to the nearest fontconfig weight value.
///
/// Fontconfig uses a different, non-linear scale from CSS. Values outside
/// the usual CSS range are still mapped to the nearest supported entry.
fn fc_weight(css: f64) -> i32 {
    const MAP: [(f64, i32); 9] = [
        (100.0, 0),
        (200.0, 40),
        (300.0, 50),
        (400.0, 80),
        (500.0, 100),
        (600.0, 180),
        (700.0, 200),
        (800.0, 205),
        (900.0, 210),
    ];
    MAP.iter()
        .min_by(|a, b| (a.0 - css).abs().total_cmp(&(b.0 - css).abs()))
        .map_or(80, |&(_, fc)| fc)
}
