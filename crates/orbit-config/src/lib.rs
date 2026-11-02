mod animation;
mod gestures;
mod input;
mod output;

use std::collections::HashMap;

use serde::Deserialize;

use animation::RawAnimations;
use gestures::RawGestures;
use input::RawInput;
use output::RawOutput;

pub use animation::AnimationConfig;
pub use gestures::{OverviewSwipeConfig, WorkspaceSwipeConfig};
pub use input::{AccelerationProfile, InputConfig, KeyboardConfig, PointerConfig};
pub use output::{MAX_SCALE, MIN_SCALE, ModeSpec, OutputConfig, Transform};

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
    #[serde(default)]
    startup: RawStartup,
    #[serde(default, rename = "desktop-integration")]
    desktop_integration: RawDesktopIntegration,
    #[serde(default)]
    overview: RawOverview,
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
    pub startup: Vec<Vec<String>>,
    pub desktop_integration: bool,
    pub overview_dim: f64,
    pub input: InputConfig,
    pub workspace_swipe: WorkspaceSwipeConfig,
    pub overview_swipe: OverviewSwipeConfig,
    pub animations: AnimationConfig,
    outputs: HashMap<String, OutputConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            startup: Vec::new(),
            desktop_integration: true,
            overview_dim: DEFAULT_OVERVIEW_DIM,
            input: InputConfig::default(),
            workspace_swipe: WorkspaceSwipeConfig::default(),
            overview_swipe: OverviewSwipeConfig::default(),
            animations: AnimationConfig::default(),
            outputs: HashMap::new(),
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::from_table(text.parse::<toml::Table>().map_err(|err| err.to_string())?)
    }

    pub fn output(&self, connector: &str, description: &str) -> Option<&OutputConfig> {
        self.outputs
            .get(connector)
            .or_else(|| self.outputs.get(description))
    }

    pub(crate) fn from_table(table: toml::Table) -> Result<Self, String> {
        let raw: RawConfig = table
            .try_into()
            .map_err(|err: toml::de::Error| err.to_string())?;

        let mut config = Self {
            desktop_integration: !raw.desktop_integration.off,
            overview_dim: overview_dim(raw.overview),
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
