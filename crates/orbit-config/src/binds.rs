use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use xkbcommon::xkb::{self, Keysym};

/// Built-in key bindings used before user overrides are applied.
const DEFAULT_BINDS: &[(&str, &str)] = &[
    ("Mod+Q", "close-window"),
    ("Mod+M", "toggle-maximize"),
    ("Mod+F", "toggle-fullscreen"),
    ("Mod+Tab", "focus-next"),
    ("Mod+H", "minimize"),
    ("Mod+Shift+H", "restore-minimized"),
    ("Mod+Shift+Left", "tile-left"),
    ("Mod+Shift+Right", "tile-right"),
    ("Mod+Left", "move-left"),
    ("Mod+Right", "move-right"),
    ("Mod+Up", "move-up"),
    ("Mod+Down", "move-down"),
    ("Mod+Ctrl+Right", "grow-width"),
    ("Mod+Ctrl+Left", "shrink-width"),
    ("Mod+Ctrl+Down", "grow-height"),
    ("Mod+Ctrl+Up", "shrink-height"),
    ("Mod+Shift+E", "quit"),
    ("Mod+Page_Down", "focus-workspace-next"),
    ("Mod+Page_Up", "focus-workspace-prev"),
    ("Mod+Shift+Page_Down", "move-to-workspace-next"),
    ("Mod+Shift+Page_Up", "move-to-workspace-prev"),
    ("Mod+Comma", "focus-output-left"),
    ("Mod+Period", "focus-output-right"),
    ("Mod+Shift+Comma", "move-to-output-left"),
    ("Mod+Shift+Period", "move-to-output-right"),
    ("Mod+W", "toggle-overview"),
];

/// Number of default workspace bindings generated for `Mod+1` through `Mod+9`.
const NUMBERED_WORKSPACES: usize = 9;

/// Largest workspace number accepted in a configured action.
const MAX_WORKSPACE: usize = 99;

/// Default terminal command launched by `Mod+Return`.
const DEFAULT_TERMINAL: &str = "foot";

/// The modifiers held when a key is pressed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

/// The modifier held for compositor bindings (`mod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModKey {
    Alt,
    Super,
}

impl ModKey {
    /// Returns whether this configured modifier is currently pressed.
    pub fn pressed(self, modifiers: &Modifiers) -> bool {
        match self {
            ModKey::Alt => modifiers.alt,
            ModKey::Super => modifiers.meta,
        }
    }
}

/// Compositor actions that can be bound to keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CloseWindow,
    ToggleMaximize,
    ToggleFullscreen,
    FocusNext,
    Minimize,
    RestoreMinimized,
    TileLeft,
    TileRight,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    GrowWidth,
    ShrinkWidth,
    GrowHeight,
    ShrinkHeight,
    Quit,
    Spawn(Vec<String>),
    FocusWorkspace(usize),
    FocusWorkspaceNext,
    FocusWorkspacePrev,
    MoveToWorkspace(usize),
    MoveToWorkspaceNext,
    MoveToWorkspacePrev,
    FocusOutputLeft,
    FocusOutputRight,
    MoveToOutputLeft,
    MoveToOutputRight,
    ToggleOverview,
    /// Switch to another virtual terminal. Bound to Ctrl+Alt+F1–F12 on a
    /// TTY session; not configurable.
    SwitchVt(i32),
}

impl Action {
    /// Parses a named action from the configuration format.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "close-window" => Self::CloseWindow,
            "toggle-maximize" => Self::ToggleMaximize,
            "toggle-fullscreen" => Self::ToggleFullscreen,
            "focus-next" => Self::FocusNext,
            "minimize" => Self::Minimize,
            "restore-minimized" => Self::RestoreMinimized,
            "tile-left" => Self::TileLeft,
            "tile-right" => Self::TileRight,
            "move-left" => Self::MoveLeft,
            "move-right" => Self::MoveRight,
            "move-up" => Self::MoveUp,
            "move-down" => Self::MoveDown,
            "grow-width" => Self::GrowWidth,
            "shrink-width" => Self::ShrinkWidth,
            "grow-height" => Self::GrowHeight,
            "shrink-height" => Self::ShrinkHeight,
            "quit" => Self::Quit,
            "focus-workspace-next" => Self::FocusWorkspaceNext,
            "focus-workspace-prev" => Self::FocusWorkspacePrev,
            "move-to-workspace-next" => Self::MoveToWorkspaceNext,
            "move-to-workspace-prev" => Self::MoveToWorkspacePrev,
            "focus-output-left" => Self::FocusOutputLeft,
            "focus-output-right" => Self::FocusOutputRight,
            "move-to-output-left" => Self::MoveToOutputLeft,
            "move-to-output-right" => Self::MoveToOutputRight,
            "toggle-overview" => Self::ToggleOverview,
            _ => return None,
        })
    }
}

/// Modifier requirements for a binding.
///
/// `main` represents the configurable `Mod` key and is resolved against
/// [`ModKey`] when the binding is matched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
struct Mods {
    main: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
}

impl Mods {
    /// Reports whether these requirements match the currently pressed keys.
    fn matches(self, mod_key: ModKey, pressed: &Modifiers) -> bool {
        let alt = self.alt || (self.main && mod_key == ModKey::Alt);
        let meta = self.meta || (self.main && mod_key == ModKey::Super);
        pressed.ctrl == self.ctrl
            && pressed.alt == alt
            && pressed.shift == self.shift
            && pressed.meta == meta
    }
}

/// A normalized key and modifier combination used as a binding key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Trigger {
    /// Required modifier combination.
    mods: Mods,
    /// XKB keysym for the trigger key.
    keysym: Keysym,
}

/// Parses a binding such as `Mod+Shift+H`.
///
/// The `super` and `meta` names refer to the Super key.
pub(crate) fn parse_trigger(spec: &str) -> Result<Trigger, String> {
    let mut mods = Mods::default();
    let mut parts: Vec<&str> = spec.split('+').map(str::trim).collect();
    let key = parts.pop().filter(|k| !k.is_empty()).ok_or("missing key")?;
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "mod" => mods.main = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            "super" | "meta" => mods.meta = true,
            other => return Err(format!("unknown modifier `{other}`")),
        }
    }
    let keysym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
    if keysym == Keysym::NoSymbol {
        return Err(format!("unknown key `{key}`"));
    }
    Ok(Trigger {
        mods,
        keysym: normalize(keysym),
    })
}

/// Raw action syntax accepted in the configuration file.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawAction {
    /// Named action, or `"none"` to remove a binding.
    Name(String),
    /// Command and arguments to spawn.
    Spawn {
        /// Program and arguments to execute.
        spawn: Vec<String>,
        /// Whether the binding remains active while locked.
        #[serde(default, rename = "allow-when-locked")]
        allow_when_locked: bool,
    },
    /// Focuses a numbered workspace.
    FocusWorkspace {
        /// One-based workspace number.
        #[serde(rename = "focus-workspace")]
        focus_workspace: i64,
        /// Whether the binding remains active while locked.
        #[serde(default, rename = "allow-when-locked")]
        allow_when_locked: bool,
    },
    /// Moves the focused window to a numbered workspace.
    MoveToWorkspace {
        /// One-based workspace number.
        #[serde(rename = "move-to-workspace")]
        move_to_workspace: i64,
        /// Whether the binding remains active while locked.
        #[serde(default, rename = "allow-when-locked")]
        allow_when_locked: bool,
    },
    /// Named action with optional lock-screen permission.
    Named {
        /// Action name.
        action: String,
        /// Whether the binding remains active while locked.
        #[serde(default, rename = "allow-when-locked")]
        allow_when_locked: bool,
    },
}

/// Effective bindings after defaults and user configuration are merged.
#[derive(Debug, Clone)]
pub(crate) struct Binds {
    /// Actions indexed by normalized key trigger.
    pub(crate) actions: HashMap<Trigger, Action>,
    /// Triggers allowed to run while the session is locked.
    locked: HashSet<Trigger>,
}

impl Default for Binds {
    fn default() -> Self {
        let mut binds: HashMap<Trigger, Action> = DEFAULT_BINDS
            .iter()
            .map(|(spec, name)| {
                let trigger = parse_trigger(spec).expect("invalid default binding");
                (
                    trigger,
                    Action::from_name(name).expect("invalid default action"),
                )
            })
            .collect();
        binds.insert(
            parse_trigger("Mod+Return").expect("invalid default binding"),
            Action::Spawn(vec![DEFAULT_TERMINAL.to_string()]),
        );
        for n in 1..=NUMBERED_WORKSPACES {
            binds.insert(
                parse_trigger(&format!("Mod+{n}")).expect("invalid default binding"),
                Action::FocusWorkspace(n),
            );
            binds.insert(
                parse_trigger(&format!("Mod+Shift+{n}")).expect("invalid default binding"),
                Action::MoveToWorkspace(n),
            );
        }
        Self {
            actions: binds,
            locked: HashSet::new(),
        }
    }
}

impl Binds {
    /// Merges user bindings over the defaults.
    ///
    /// Invalid individual bindings are logged and skipped without discarding
    /// the rest of the configuration.
    pub(crate) fn apply(&mut self, raw: HashMap<String, RawAction>) {
        for (spec, raw_action) in raw {
            let trigger = match parse_trigger(&spec) {
                Ok(trigger) => trigger,
                Err(err) => {
                    tracing::warn!("ignoring binding `{spec}`: {err}");
                    continue;
                }
            };
            let (action, when_locked) = match raw_action {
                RawAction::Name(name) if name == "none" => {
                    self.actions.remove(&trigger);
                    self.locked.remove(&trigger);
                    continue;
                }
                RawAction::Name(name) => (Action::from_name(&name).ok_or(name), false),
                RawAction::Named {
                    action,
                    allow_when_locked,
                } => (Action::from_name(&action).ok_or(action), allow_when_locked),
                RawAction::Spawn { spawn, .. } if spawn.is_empty() => {
                    tracing::warn!("ignoring binding `{spec}`: empty spawn command");
                    continue;
                }
                RawAction::Spawn {
                    spawn,
                    allow_when_locked,
                } => (Ok(Action::Spawn(spawn)), allow_when_locked),
                RawAction::FocusWorkspace {
                    focus_workspace: n, ..
                }
                | RawAction::MoveToWorkspace {
                    move_to_workspace: n,
                    ..
                } if !(1..=MAX_WORKSPACE as i64).contains(&n) => {
                    tracing::warn!(
                        "ignoring binding `{spec}`: workspace {n} is not 1..={MAX_WORKSPACE}"
                    );
                    continue;
                }
                RawAction::FocusWorkspace {
                    focus_workspace,
                    allow_when_locked,
                } => (
                    Ok(Action::FocusWorkspace(focus_workspace as usize)),
                    allow_when_locked,
                ),
                RawAction::MoveToWorkspace {
                    move_to_workspace,
                    allow_when_locked,
                } => (
                    Ok(Action::MoveToWorkspace(move_to_workspace as usize)),
                    allow_when_locked,
                ),
            };
            match action {
                Ok(action) => {
                    self.actions.insert(trigger, action);
                    if when_locked {
                        self.locked.insert(trigger);
                    } else {
                        self.locked.remove(&trigger);
                    }
                }
                Err(name) => {
                    tracing::warn!("ignoring binding `{spec}`: unknown action `{name}`");
                }
            }
        }
    }

    /// Returns the matching action and whether it is allowed while locked.
    pub(crate) fn find(
        &self,
        mod_key: ModKey,
        pressed: &Modifiers,
        keysym: Keysym,
    ) -> Option<(Action, bool)> {
        let keysym = normalize(keysym);
        self.actions
            .iter()
            .find(|(trigger, _)| trigger.keysym == keysym && trigger.mods.matches(mod_key, pressed))
            .map(|(trigger, action)| (action.clone(), self.locked.contains(trigger)))
    }
}

/// Normalizes ASCII letters so bindings match regardless of key case.
fn normalize(keysym: Keysym) -> Keysym {
    let raw = keysym.raw();
    if (Keysym::A.raw()..=Keysym::Z.raw()).contains(&raw) {
        Keysym::new(raw + (Keysym::a.raw() - Keysym::A.raw()))
    } else {
        keysym
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, loader::merge};

    fn mods(alt: bool, shift: bool, ctrl: bool) -> Modifiers {
        Modifiers {
            alt,
            shift,
            ctrl,
            ..Default::default()
        }
    }

    #[test]
    fn defaults_resolve_mod() {
        let config = Config::default();
        assert_eq!(
            config.action_for(ModKey::Alt, &mods(true, false, false), Keysym::q),
            Some(Action::CloseWindow)
        );
        // Super isn't held, so Super-as-Mod doesn't match.
        assert_eq!(
            config.action_for(ModKey::Super, &mods(true, false, false), Keysym::q),
            None
        );
        assert_eq!(
            config.action_for(ModKey::Alt, &mods(true, true, false), Keysym::h),
            Some(Action::RestoreMinimized)
        );
        assert_eq!(
            config.action_for(ModKey::Alt, &mods(true, false, true), Keysym::Right),
            Some(Action::GrowWidth)
        );
        assert_eq!(
            config.action_for(ModKey::Alt, &mods(true, true, false), Keysym::e),
            Some(Action::Quit)
        );
    }

    #[test]
    fn tab_bindings_can_be_rebound_to_spawn() {
        // A shell's layered config takes over Mod+Tab and Mod+Shift+Tab.
        let mut base: toml::Table = r#"
            [binds]
            "Mod+H" = "minimize"
        "#
        .parse()
        .unwrap();
        let shell: toml::Table = r#"
            [binds]
            "Mod+Tab" = { spawn = ["shell", "switcher", "next"] }
            "Mod+Shift+Tab" = { spawn = ["shell", "switcher", "previous"] }
        "#
        .parse()
        .unwrap();
        merge(&mut base, shell, false);
        let config = Config::from_table(base).unwrap();
        let meta = Modifiers {
            meta: true,
            ..Default::default()
        };
        let logo_shift = Modifiers {
            shift: true,
            ..meta
        };
        assert_eq!(
            config.action_for(ModKey::Super, &meta, Keysym::Tab),
            Some(Action::Spawn(vec![
                "shell".into(),
                "switcher".into(),
                "next".into()
            ]))
        );
        assert_eq!(
            config.action_for(ModKey::Super, &logo_shift, Keysym::Tab),
            Some(Action::Spawn(vec![
                "shell".into(),
                "switcher".into(),
                "previous".into()
            ]))
        );
        // Without the override it focuses the next window, as before.
        let defaults = Config::default();
        assert_eq!(
            defaults.action_for(ModKey::Super, &meta, Keysym::Tab),
            Some(Action::FocusNext)
        );
        assert_eq!(
            defaults.action_for(ModKey::Super, &meta, Keysym::w),
            Some(Action::ToggleOverview)
        );
    }

    #[test]
    fn user_binds_merge_over_defaults() {
        let config = Config::parse(
            r#"
            mod = "super"
            [binds]
            "Mod+Q" = "none"
            "Mod+Shift+Q" = "close-window"
            "Mod+T" = { spawn = ["kitty", "--single-instance"] }
            "Mod+Bogus" = "close-window"
            "Mod+X" = "not-an-action"
            "#,
        )
        .unwrap();
        let meta = Modifiers {
            meta: true,
            ..Default::default()
        };
        let logo_shift = Modifiers {
            meta: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(config.mod_key, Some(ModKey::Super));
        assert_eq!(config.action_for(ModKey::Super, &meta, Keysym::q), None);
        assert_eq!(
            config.action_for(ModKey::Super, &logo_shift, Keysym::q),
            Some(Action::CloseWindow)
        );
        assert_eq!(
            config.action_for(ModKey::Super, &meta, Keysym::t),
            Some(Action::Spawn(vec![
                "kitty".into(),
                "--single-instance".into()
            ]))
        );
        // Untouched defaults survive.
        assert_eq!(
            config.action_for(ModKey::Super, &meta, Keysym::m),
            Some(Action::ToggleMaximize)
        );
    }

    #[test]
    fn media_keys_and_bindings_allowed_when_locked() {
        let config = Config::parse(
            r#"
            [binds]
            "XF86AudioRaiseVolume" = { spawn = ["vol", "up"], allow-when-locked = true }
            "XF86AudioMute" = { spawn = ["vol", "mute"] }
            "XF86MonBrightnessUp" = { action = "focus-next", allow-when-locked = true }
            "Mod+Shift+Q" = { action = "close-window" }
            "Mod+Q" = { action = "no-such-action" }
            "#,
        )
        .expect("valid config");
        let none = Modifiers::default();
        assert_eq!(
            config.bind_for(ModKey::Super, &none, Keysym::XF86_AudioRaiseVolume),
            Some((Action::Spawn(vec!["vol".into(), "up".into()]), true))
        );
        assert_eq!(
            config.bind_for(ModKey::Super, &none, Keysym::XF86_AudioMute),
            Some((Action::Spawn(vec!["vol".into(), "mute".into()]), false))
        );
        assert_eq!(
            config.bind_for(ModKey::Super, &none, Keysym::XF86_MonBrightnessUp),
            Some((Action::FocusNext, true))
        );
        let super_shift = Modifiers {
            meta: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            config.action_for(ModKey::Super, &super_shift, Keysym::q),
            Some(Action::CloseWindow)
        );
        // A bad action is skipped and the default stays.
        let meta = Modifiers {
            meta: true,
            ..Default::default()
        };
        assert_eq!(
            config.action_for(ModKey::Super, &meta, Keysym::q),
            Some(Action::CloseWindow)
        );
        // Media keys need no modifier, and others don't trigger them.
        for name in [
            "XF86AudioLowerVolume",
            "XF86AudioPlay",
            "XF86AudioNext",
            "XF86AudioPrev",
            "XF86MonBrightnessDown",
        ] {
            let trigger = parse_trigger(name).expect(name);
            assert_eq!(trigger.mods, Mods::default(), "{name}");
        }
        assert_eq!(
            config.bind_for(ModKey::Super, &meta, Keysym::XF86_AudioRaiseVolume),
            None
        );
    }

    #[test]
    fn workspace_bindings() {
        let config = Config::default();
        let alt = mods(true, false, false);
        let alt_shift = mods(true, true, false);
        assert_eq!(
            config.action_for(ModKey::Alt, &alt, Keysym::_3),
            Some(Action::FocusWorkspace(3))
        );
        assert_eq!(
            config.action_for(ModKey::Alt, &alt_shift, Keysym::_9),
            Some(Action::MoveToWorkspace(9))
        );
        assert_eq!(
            config.action_for(ModKey::Alt, &alt, Keysym::Page_Down),
            Some(Action::FocusWorkspaceNext)
        );
        assert_eq!(
            config.action_for(ModKey::Alt, &alt_shift, Keysym::period),
            Some(Action::MoveToOutputRight)
        );

        let config = Config::parse(
            r#"
            [binds]
            "Mod+F1" = { focus-workspace = 12 }
            "Mod+Shift+F1" = { move-to-workspace = 12 }
            "Mod+F2" = { focus-workspace = 0 }
            "Mod+1" = "none"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.action_for(ModKey::Alt, &alt, Keysym::F1),
            Some(Action::FocusWorkspace(12))
        );
        assert_eq!(
            config.action_for(ModKey::Alt, &alt_shift, Keysym::F1),
            Some(Action::MoveToWorkspace(12))
        );
        assert_eq!(config.action_for(ModKey::Alt, &alt, Keysym::F2), None);
        assert_eq!(config.action_for(ModKey::Alt, &alt, Keysym::_1), None);
    }
}
