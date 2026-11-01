mod input;

use serde::Deserialize;

use input::RawInput;

pub use input::{AccelerationProfile, InputConfig, KeyboardConfig, PointerConfig};

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    input: RawInput,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub input: InputConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            input: InputConfig::default(),
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::from_table(text.parse::<toml::Table>().map_err(|err| err.to_string())?)
    }

    pub(crate) fn from_table(table: toml::Table) -> Result<Self, String> {
        let raw: RawConfig = table
            .try_into()
            .map_err(|err: toml::de::Error| err.to_string())?;

        let config = Self {
            input: InputConfig::from_raw(raw.input),
            ..Self::default()
        };

        Ok(config)
    }
}
