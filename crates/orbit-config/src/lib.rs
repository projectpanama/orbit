mod animation;
mod binds;
mod decorations;
mod gestures;
mod input;
mod loader;
mod output;

use std::collections::HashMap;

use serde::Deserialize;

use animation::RawAnimations;
use binds::{Binds, RawAction};
use decorations::RawDecorations;
use gestures::RawGestures;
use input::RawInput;
use output::RawOutput;

pub use animation::AnimationConfig;
pub use binds::{Action, ModKey, Modifiers};
pub use decorations::{Button, DecorationColors, DecorationConfig, Rgba};
pub use gestures::{OverviewSwipeConfig, WorkspaceSwipeConfig};
pub use input::{AccelerationProfile, InputConfig, KeyboardConfig, PointerConfig};
pub use loader::{ConfigLoader, ConfigSource, DEFAULT_CONFIG, SYSTEM_CONFIG_PATH, check};
pub use output::{MAX_SCALE, MIN_SCALE, ModeSpec, OutputConfig, Transform};
pub use xkbcommon::xkb::Keysym;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawOverview {
    dim: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDesktopIntegration {
    #[serde(default)]
    off: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawStartup {
    #[serde(default)]
    spawn: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(rename = "mod")]
    mod_key: Option<String>,
    #[serde(default)]
    startup: RawStartup,
    #[serde(default, rename = "desktop-integration")]
    desktop_integration: RawDesktopIntegration,
    #[serde(default)]
    binds: HashMap<String, RawAction>,
    #[serde(default)]
    overview: RawOverview,
    #[serde(default)]
    decorations: RawDecorations,
    #[serde(default)]
    input: RawInput,
    #[serde(default)]
    outputs: HashMap<String, RawOutput>,
    #[serde(default)]
    gestures: RawGestures,
    #[serde(default)]
    animations: RawAnimations,
}

/// How dark the background gets in the overview.
pub const DEFAULT_OVERVIEW_DIM: f64 = 0.3;

#[derive(Debug, Clone)]
pub struct Config {
    pub mod_key: Option<ModKey>,
    pub startup: Vec<Vec<String>>,
    pub desktop_integration: bool,
    pub overview_dim: f64,
    pub decorations: DecorationConfig,
    pub input: InputConfig,
    pub workspace_swipe: WorkspaceSwipeConfig,
    pub overview_swipe: OverviewSwipeConfig,
    pub animations: AnimationConfig,
    binds: Binds,
    outputs: HashMap<String, OutputConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mod_key: None,
            startup: Vec::new(),
            desktop_integration: true,
            overview_dim: DEFAULT_OVERVIEW_DIM,
            decorations: DecorationConfig::default(),
            input: InputConfig::default(),
            workspace_swipe: WorkspaceSwipeConfig::default(),
            overview_swipe: OverviewSwipeConfig::default(),
            animations: AnimationConfig::default(),
            binds: Binds::default(),
            outputs: HashMap::new(),
        }
    }
}

impl Config {
    /// Parses a TOML string into a [`Config`].
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::from_table(text.parse::<toml::Table>().map_err(|err| err.to_string())?)
    }

    /// Settings for the monitor on `connector` (e.g. `HDMI-A-1`) described
    /// as `description` ("make model serial").
    pub fn output(&self, connector: &str, description: &str) -> Option<&OutputConfig> {
        self.outputs
            .get(connector)
            .or_else(|| self.outputs.get(description))
    }

    /// Whether `[outputs]` sets up monitors differently from `other`'s.
    pub fn outputs_differ(&self, other: &Config) -> bool {
        self.outputs != other.outputs
    }

    /// The bound action and whether it may run while the sssion is locked.
    pub fn bind_for(
        &self,
        mod_key: ModKey,
        pressed: &Modifiers,
        keysym: Keysym,
    ) -> Option<(Action, bool)> {
        self.binds.find(mod_key, pressed, keysym)
    }

    #[cfg(test)]
    pub fn load_layered(paths: &[std::path::PathBuf]) -> Self {
        ConfigLoader::new(ConfigSource::Given(paths.to_vec())).load()
    }

    #[cfg(test)]
    pub(crate) fn action_for(
        &self,
        mod_key: ModKey,
        pressed: &Modifiers,
        keysym: Keysym,
    ) -> Option<Action> {
        self.bind_for(mod_key, pressed, keysym)
            .map(|(action, _)| action)
    }

    pub(crate) fn from_table(table: toml::Table) -> Result<Self, String> {
        let raw: RawConfig = table
            .try_into()
            .map_err(|err: toml::de::Error| err.to_string())?;

        let mod_key = match raw
            .mod_key
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("auto") => None,
            Some("alt") => Some(ModKey::Alt),
            Some("super") | Some("meta") => Some(ModKey::Super),
            Some(other) => return Err(format!("unknown mod `{other}`")),
        };

        let mut config = Self {
            mod_key,
            desktop_integration: !raw.desktop_integration.off,
            overview_dim: overview_dim(raw.overview),
            decorations: DecorationConfig::from_raw(raw.decorations),
            input: InputConfig::from_raw(raw.input),
            workspace_swipe: WorkspaceSwipeConfig::from_raw(raw.gestures.workspace_swipe),
            overview_swipe: OverviewSwipeConfig::from_raw(raw.gestures.overview_swipe),
            animations: AnimationConfig::from_raw(raw.animations),
            ..Self::default()
        };

        for command in raw.startup.spawn {
            if command.is_empty() {
                tracing::warn!("ignoring empty startup command");
            } else {
                config.startup.push(command);
            }
        }

        config.binds.apply(raw.binds);

        for (name, raw_output) in raw.outputs {
            match OutputConfig::from_raw(raw_output) {
                Ok(output) => {
                    config.outputs.insert(name, output);
                }
                Err(err) => tracing::warn!("ignoring output `{name}`: {err}"),
            }
        }

        Ok(config)
    }
}

fn overview_dim(raw: RawOverview) -> f64 {
    match raw.dim {
        None => DEFAULT_OVERVIEW_DIM,
        Some(d) if (0.0..=1.0).contains(&d) => d,
        Some(d) => {
            tracing::warn!("ignoring overview dim {d}: not 0..=1");
            DEFAULT_OVERVIEW_DIM
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn parses_startup() {
        let config = Config::parse(
            r##"
            [startup]
            spawn = [["waybar"], [], ["swaybg", "-c", "#202020"]]
            "##,
        )
        .unwrap();
        assert_eq!(
            config.startup,
            vec![
                vec!["waybar".to_string()],
                vec!["swaybg".into(), "-c".into(), "#202020".into()],
            ]
        );
        assert!(Config::default().startup.is_empty());
        assert!(Config::default().desktop_integration);
        assert!(
            !Config::parse("[desktop-integration]\noff = true")
                .unwrap()
                .desktop_integration
        );
        assert!(Config::parse("[startup]\nexec = [[\"waybar\"]]").is_err());
    }

    #[test]
    fn parses_overview_settings() {
        let config = Config::parse(
            r#"
            [overview]
            dim = 0.5
            [animations.overview]
            duration-ms = 200
            [gestures.overview-swipe]
            fingers = 3
            "#,
        )
        .unwrap();
        assert_eq!(config.overview_dim, 0.5);
        assert_eq!(
            config.animations.overview.duration,
            Duration::from_millis(200)
        );
        assert_eq!(config.overview_swipe.fingers, 3);
        let bad = Config::parse("[overview]\ndim = 4.0\n[gestures.overview-swipe]\nfingers = 9\n")
            .unwrap();
        assert_eq!(bad.overview_dim, DEFAULT_OVERVIEW_DIM);
        assert_eq!(bad.overview_swipe.fingers, 4);
        let off =
            Config::parse("[gestures.overview-swipe]\noff = true\n[animations]\noff = true\n")
                .unwrap();
        assert!(!off.overview_swipe.enabled);
        assert!(off.animations.overview.is_off());
    }

    #[test]
    fn invalid_config_is_an_error() {
        assert!(Config::parse("mod = \"hyper\"").is_err());
        assert!(Config::parse("binds = 3").is_err());
    }
}
