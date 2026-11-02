use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RawWorkspaceSwipe {
    #[serde(default)]
    off: bool,
    fingers: Option<i64>,
    sensitivity: Option<f64>,
    mod_scroll: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RawOverviewSwipe {
    #[serde(default)]
    off: bool,
    fingers: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RawGestures {
    pub(crate) workspace_swipe: Option<RawWorkspaceSwipe>,
    pub(crate) overview_swipe: Option<RawOverviewSwipe>,
}

/// Minimum accepted workspace swipe sensitivity.
const MIN_SENSITIVITY: f64 = 0.1;
/// Maximum accepted workspace swipe sensitivity.
const MAX_SENSITIVITY: f64 = 10.0;

/// Configuration for switching workspaces with a horizontal swipe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkspaceSwipeConfig {
    /// Whether workspace swiping is enabled.
    pub enabled: bool,
    /// Touchpad fingers that swipe between workspaces.
    pub fingers: u32,
    /// Multiplier for the distance moved by a given finger movement.
    pub sensitivity: f64,
    /// Whether modifier-assisted horizontal scrolling also swipes.
    ///
    /// This can support nested sessions where the host does not forward
    /// touchpad swipe gestures.
    pub mod_scroll: bool,
}

impl Default for WorkspaceSwipeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fingers: 3,
            sensitivity: 1.0,
            mod_scroll: true,
        }
    }
}

impl WorkspaceSwipeConfig {
    /// Converts raw settings, warning and defaulting invalid values.
    pub(crate) fn from_raw(raw: Option<RawWorkspaceSwipe>) -> Self {
        let mut config = Self::default();
        let Some(raw) = raw else {
            return config;
        };
        config.enabled = !raw.off;
        match raw.fingers {
            None => {}
            Some(fingers @ (3 | 4)) => config.fingers = fingers as u32,
            Some(fingers) => {
                tracing::warn!("ignoring workspace-swipe fingers {fingers}: not 3 or 4");
            }
        }
        match raw.sensitivity {
            None => {}
            Some(value) if (MIN_SENSITIVITY..=MAX_SENSITIVITY).contains(&value) => {
                config.sensitivity = value;
            }
            Some(value) => tracing::warn!(
                "ignoring workspace-swipe sensitivity {value}: not \
                 {MIN_SENSITIVITY}..={MAX_SENSITIVITY}"
            ),
        }
        if let Some(mod_scroll) = raw.mod_scroll {
            config.mod_scroll = mod_scroll;
        }
        config
    }
}

/// Configuration for the vertical touchpad swipe that scrubs the overview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewSwipeConfig {
    /// Whether overview swiping is enabled.
    pub enabled: bool,
    /// Number of fingers required for the gesture.
    pub fingers: u32,
}

impl Default for OverviewSwipeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fingers: 4,
        }
    }
}

impl OverviewSwipeConfig {
    /// Converts raw settings, warning and defaulting invalid finger counts.
    pub(crate) fn from_raw(raw: Option<RawOverviewSwipe>) -> Self {
        let mut config = Self::default();
        let Some(raw) = raw else {
            return config;
        };
        config.enabled = !raw.off;
        match raw.fingers {
            None => {}
            Some(fingers @ (3 | 4)) => config.fingers = fingers as u32,
            Some(fingers) => {
                tracing::warn!("ignoring overview-swipe fingers {fingers}: not 3 or 4")
            }
        }
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn parses_workspace_swipe() {
        assert_eq!(
            Config::parse("").unwrap().workspace_swipe,
            WorkspaceSwipeConfig::default()
        );

        let config = Config::parse(
            r#"
            [gestures.workspace-swipe]
            fingers = 4
            sensitivity = 1.5
            mod-scroll = false
            "#,
        )
        .unwrap();
        assert_eq!(
            config.workspace_swipe,
            WorkspaceSwipeConfig {
                enabled: true,
                fingers: 4,
                sensitivity: 1.5,
                mod_scroll: false,
            }
        );

        let off = Config::parse("[gestures.workspace-swipe]\noff = true").unwrap();
        assert!(!off.workspace_swipe.enabled);

        let bad = Config::parse(
            r#"
            [gestures.workspace-swipe]
            fingers = 2
            sensitivity = 0
            "#,
        )
        .unwrap();
        assert_eq!(bad.workspace_swipe, WorkspaceSwipeConfig::default());
        assert!(Config::parse("[gestures.pinch]\nfingers = 2").is_err());
    }
}
