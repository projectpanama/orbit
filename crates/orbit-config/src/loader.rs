use std::{
    collections::HashMap,
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};

use crate::Config;

/// System-wide configuration file used by the default source.
pub const SYSTEM_CONFIG_PATH: &str = "/etc/orbit/config.toml";

/// Built-in configuration written for a first-time user configuration.
pub const DEFAULT_CONFIG: &str = include_str!("../assets/config.toml");

/// Files from which configuration is loaded and their precedence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Explicit files from `--config`, layered in the given order.
    ///
    /// Missing files are warned about and are never created automatically.
    Given(Vec<PathBuf>),
    /// The system file with the user's file layered over it.
    ///
    /// Either file may be missing. `user` is `None` when no home directory is
    /// available.
    Default {
        /// System-wide configuration path.
        system: PathBuf,
        /// Optional per-user configuration path.
        user: Option<PathBuf>,
    },
}

impl ConfigSource {
    /// Builds a source from explicit paths or the default system/user paths.
    pub fn new(given: Vec<PathBuf>) -> Self {
        if !given.is_empty() {
            return Self::Given(given);
        }
        Self::Default {
            system: PathBuf::from(SYSTEM_CONFIG_PATH),
            user: user_config_path(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            ),
        }
    }

    /// Returns source files in layer order.
    ///
    /// Later files override values from earlier files.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Given(paths) => paths.iter().map(PathBuf::as_path).collect(),
            Self::Default { system, user } => std::iter::once(system.as_path())
                .chain(user.as_deref())
                .collect(),
        }
    }

    /// Reports whether the source was supplied explicitly.
    pub fn is_given(&self) -> bool {
        matches!(self, Self::Given(_))
    }

    /// Creates the user's default configuration when no config exists.
    ///
    /// This applies only to the default source, never overwrites an existing
    /// file, and returns the path created when successful.
    pub fn create_user_config(&self) -> std::io::Result<Option<&Path>> {
        let Self::Default {
            system,
            user: Some(user),
        } = self
        else {
            return Ok(None);
        };
        if system.try_exists()? || user.try_exists()? {
            return Ok(None);
        }
        if let Some(dir) = user.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // `create_new` so a file that appeared since the check (another
        // Orbit starting) is left alone.
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(user)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
            Err(err) => return Err(err),
        };
        file.write_all(DEFAULT_CONFIG.as_bytes())?;
        Ok(Some(user))
    }
}

/// Loads the config from a [`ConfigSource`]'s files, and again when they change.
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    /// Files and precedence used for loading.
    source: ConfigSource,
    /// Last valid table and standalone config for each file.
    ///
    /// Invalid edits retain the file's previous valid contribution.
    last_good: HashMap<PathBuf, (toml::Table, Config)>,
}

impl ConfigLoader {
    /// Creates a loader with no previously valid file contents.
    pub fn new(source: ConfigSource) -> Self {
        Self {
            source,
            last_good: HashMap::new(),
        }
    }

    /// Returns the configured source selection.
    pub fn source(&self) -> &ConfigSource {
        &self.source
    }

    /// Loads all source files and merges them in precedence order.
    ///
    /// Missing files are skipped. Invalid files retain their last valid
    /// contents during reload, preventing a typo from removing working
    /// configuration.
    pub fn load(&mut self) -> Config {
        let paths = self.source.paths();
        let explicit = self.source.is_given();
        // The files that make up the config, each in `last_good`.
        let mut layers: Vec<&Path> = Vec::with_capacity(paths.len());
        for path in paths {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    if explicit {
                        tracing::warn!(path = %path.display(), "config file not found, skipping it");
                    } else {
                        tracing::debug!(path = %path.display(), "no config file, using defaults");
                    }
                    self.last_good.remove(path);
                    continue;
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "failed to read config, skipping it");
                    continue;
                }
            };
            // Each file must be valid on its own, so one broken file can't
            // spoil the others.
            let parsed = text
                .parse::<toml::Table>()
                .map_err(|err| err.to_string())
                .and_then(|table| Ok((Config::from_table(table.clone())?, table)));
            match parsed {
                Ok((config, table)) => {
                    tracing::info!(path = %path.display(), "loaded config");
                    self.last_good.insert(path.to_path_buf(), (table, config));
                    layers.push(path);
                }
                Err(err) if self.last_good.contains_key(path) => {
                    tracing::warn!(path = %path.display(), "invalid config, keeping what it said before: {err}");
                    layers.push(path);
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), "invalid config, skipping it: {err}");
                }
            }
        }

        // One file (the usual case) is the whole config, already built.
        if let [path] = layers.as_slice()
            && let Some((_, config)) = self.last_good.get(*path)
        {
            return config.clone();
        }
        let mut merged = toml::Table::new();
        for path in layers {
            if let Some((table, _)) = self.last_good.get(path) {
                merge(&mut merged, table.clone(), false);
            }
        }
        Config::from_table(merged).unwrap_or_else(|err| {
            tracing::warn!("invalid combined config, using defaults: {err}");
            Config::default()
        })
    }
}

/// Reads and validates one configuration file without layering.
///
/// Invalid values produce warnings and a configuration; unreadable or
/// syntactically invalid files return an error.
pub fn check(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    Config::parse(&text)
}

/// Layers `over` onto `base` using recursive table merging.
///
/// Scalar values replace earlier values. Bindings merge by trigger, so a
/// later `"none"` or spawn action replaces the earlier binding as a whole.
pub(crate) fn merge(base: &mut toml::Table, over: toml::Table, in_binds: bool) {
    for (key, value) in over {
        let is_binds = !in_binds && key == "binds";
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(over)) if !in_binds => {
                merge(base, over, is_binds);
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Resolves the per-user configuration path from XDG environment variables.
///
/// Uses `$XDG_CONFIG_HOME/orbit/config.toml` when it is absolute, otherwise
/// falls back to `$HOME/.config/orbit/config.toml`.
fn user_config_path(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("orbit").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use orbit_core::animation::Easing;

    use super::*;
    use crate::{Action, ModKey, binds::parse_trigger};

    fn write(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orbit-config-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn layered_files_merge_in_order() {
        let dir = temp_dir("layers");
        let base = write(
            &dir,
            "base.toml",
            r#"
            mod = "alt"
            [startup]
            spawn = [["panel"]]
            [binds]
            "Mod+Space" = { spawn = ["launcher"] }
            "Mod+X" = { spawn = ["x"] }
            [animations]
            slowdown = 2.0
            [animations.window-open]
            duration-ms = 300
            "#,
        );
        let user = write(
            &dir,
            "user.toml",
            r#"
            [binds]
            "Mod+X" = "none"
            "Mod+Space" = { focus-workspace = 2 }
            [animations.window-open]
            curve = "linear"
            "#,
        );
        let config = Config::load_layered(&[base, dir.join("missing.toml"), user]);
        assert_eq!(config.mod_key, Some(ModKey::Alt));
        assert_eq!(config.startup, vec![vec!["panel".to_string()]]);
        let space = parse_trigger("Mod+Space").unwrap();
        assert_eq!(
            config.binds.actions.get(&space),
            Some(&Action::FocusWorkspace(2))
        );
        assert!(
            !config
                .binds
                .actions
                .contains_key(&parse_trigger("Mod+X").unwrap())
        );
        // Nested tables merge: the slowdown and the duration survive.
        assert_eq!(
            config.animations.window_open.duration,
            Duration::from_millis(600)
        );
        assert_eq!(config.animations.window_open.easing, Easing::Linear);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn check_reports_unusable_files_only() {
        let dir = temp_dir("check");
        let good = write(&dir, "good.toml", "[animations]\nslowdown = 50\n");
        let bad = write(&dir, "bad.toml", "[animations]\nspeed = 2\n");
        // An out-of-range value is a warning: the file still works.
        assert!(check(&good).is_ok());
        assert!(check(&bad).unwrap_err().contains("speed"));
        assert!(check(&dir.join("missing.toml")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_broken_layer_is_skipped() {
        let dir = temp_dir("broken");
        let good = write(&dir, "good.toml", "mod = \"alt\"");
        let bad = write(
            &dir,
            "bad.toml",
            "mod = \"hyper\"\n[startup]\nspawn = [[\"x\"]]",
        );
        let config = Config::load_layered(&[good, bad]);
        assert_eq!(config.mod_key, Some(ModKey::Alt));
        assert!(config.startup.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn user_path_follows_xdg() {
        let path = |xdg: Option<&str>, home: Option<&str>| {
            user_config_path(xdg.map(OsString::from), home.map(OsString::from))
        };
        assert_eq!(
            path(Some("/x"), Some("/home/u")),
            Some(PathBuf::from("/x/orbit/config.toml"))
        );
        // A relative XDG_CONFIG_HOME is ignored, as the spec says.
        assert_eq!(
            path(Some("rel"), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/orbit/config.toml"))
        );
        assert_eq!(path(None, None), None);
    }

    #[test]
    fn default_source_layers_user_over_system() {
        let dir = temp_dir("source");
        let system = write(
            &dir,
            "system.toml",
            "mod = \"alt\"\n[startup]\nspawn = [[\"panel\"]]",
        );
        let user = write(&dir, "user.toml", "mod = \"super\"");
        let source = ConfigSource::Default {
            system: system.clone(),
            user: Some(user.clone()),
        };
        assert_eq!(source.paths(), vec![system.as_path(), user.as_path()]);
        assert!(!source.is_given());
        let config = ConfigLoader::new(source).load();
        assert_eq!(config.mod_key, Some(ModKey::Super));
        assert_eq!(config.startup, vec![vec!["panel".to_string()]]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn user_config_is_created_only_on_first_start() {
        let dir = temp_dir("create");
        let user = dir.join("new").join("orbit").join("config.toml");
        let source = ConfigSource::Default {
            system: dir.join("no-system.toml"),
            user: Some(user.clone()),
        };
        assert_eq!(source.create_user_config().unwrap(), Some(user.as_path()));
        assert_eq!(std::fs::read_to_string(&user).unwrap(), DEFAULT_CONFIG);
        // Never replaced.
        std::fs::write(&user, "mod = \"alt\"").unwrap();
        assert_eq!(source.create_user_config().unwrap(), None);
        assert_eq!(std::fs::read_to_string(&user).unwrap(), "mod = \"alt\"");

        // Not when a system config exists, nor for --config files.
        let system = write(&dir, "system.toml", "");
        let other = dir.join("other.toml");
        let with_system = ConfigSource::Default {
            system,
            user: Some(other.clone()),
        };
        assert_eq!(with_system.create_user_config().unwrap(), None);
        let given = ConfigSource::Given(vec![other.clone()]);
        assert_eq!(given.create_user_config().unwrap(), None);
        assert!(!other.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The template sets nothing as written, and every commented setting
    /// in it is valid and matches the built-in default.
    #[test]
    fn default_config_matches_the_defaults() {
        assert!(Config::parse(DEFAULT_CONFIG).is_ok());
        let uncommented: String = DEFAULT_CONFIG
            .lines()
            .filter_map(|line| {
                let setting = line.strip_prefix("# ")?;
                setting.parse::<toml::Table>().ok()?;
                Some(format!("{setting}\n"))
            })
            .collect();
        let table: toml::Table = uncommented.parse().unwrap();
        // Every section made it through, not just some.
        for key in [
            "mod",
            "startup",
            "desktop-integration",
            "binds",
            "outputs",
            "input",
            "animations",
            "overview",
            "gestures",
            "decorations",
        ] {
            assert!(table.contains_key(key), "`{key}` missing");
        }
        let mut config = Config::from_table(table).unwrap();
        let default = Config::default();
        // An example, not a default.
        let volume = parse_trigger("XF86AudioRaiseVolume").unwrap();
        assert!(config.binds.actions.remove(&volume).is_some());
        assert_eq!(config.mod_key, default.mod_key);
        assert_eq!(config.desktop_integration, default.desktop_integration);
        assert_eq!(config.input, default.input);
        assert_eq!(config.animations, default.animations);
        assert_eq!(config.workspace_swipe, default.workspace_swipe);
        assert_eq!(config.overview_swipe, default.overview_swipe);
        assert_eq!(config.overview_dim, default.overview_dim);
        assert_eq!(config.decorations, default.decorations);
        assert_eq!(config.binds.actions, default.binds.actions);
    }
}
