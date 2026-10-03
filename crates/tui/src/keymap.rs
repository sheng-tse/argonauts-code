//! Every key is rebindable under `ui.keymap`, by action name. Layers are checked in order:
//! overlay, card, popup, composer, then global. A key no bound action takes goes to the
//! focused part's own keys, such as a card's option letters or the composer's editing keys.

use std::collections::BTreeMap;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Overlay,
    Card,
    Popup,
    Composer,
    Global,
}

impl Layer {
    pub const ORDER: [Layer; 5] = [
        Layer::Overlay,
        Layer::Card,
        Layer::Popup,
        Layer::Composer,
        Layer::Global,
    ];
}

/// What a key does, by the name `ui.keymap` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Submit,
    Newline,
    Interrupt,
    ClearOrExit,
    Exit,
    CycleApprovalMode,
    Transcript,
    WorkflowView,
    Editor,
    CrewView,
    AgentPanel,
    HistorySearch,
    Shortcuts,
    TakeBack,
    /// Gives a waiting card the keys, on an empty prompt.
    FocusCard,
}

impl Action {
    pub const ALL: [Action; 15] = [
        Action::Submit,
        Action::Newline,
        Action::Interrupt,
        Action::ClearOrExit,
        Action::Exit,
        Action::CycleApprovalMode,
        Action::Transcript,
        Action::WorkflowView,
        Action::Editor,
        Action::CrewView,
        Action::AgentPanel,
        Action::HistorySearch,
        Action::Shortcuts,
        Action::TakeBack,
        Action::FocusCard,
    ];

    /// The key under `ui.keymap`.
    pub fn name(self) -> &'static str {
        match self {
            Action::Submit => "submit",
            Action::Newline => "newline",
            Action::Interrupt => "interrupt",
            Action::ClearOrExit => "clear_or_exit",
            Action::Exit => "exit",
            Action::CycleApprovalMode => "cycle_approval_mode",
            Action::Transcript => "transcript",
            Action::WorkflowView => "workflow_view",
            Action::Editor => "editor",
            Action::CrewView => "crew_view",
            Action::AgentPanel => "agent_panel",
            Action::HistorySearch => "history_search",
            Action::Shortcuts => "shortcuts",
            Action::TakeBack => "take_back",
            Action::FocusCard => "focus_card",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().replace('-', "_");
        Self::ALL.into_iter().find(|action| action.name() == name)
    }

    /// One line for the help view.
    pub fn describe(self) -> &'static str {
        match self {
            Action::Submit => "send the prompt",
            Action::Newline => "new line",
            Action::Interrupt => "set a card aside, stop a chat turn, or pause the run",
            Action::ClearOrExit => "clear the prompt, twice to exit",
            Action::Exit => "exit, twice on an empty prompt",
            Action::CycleApprovalMode => "next approval mode",
            Action::Transcript => "detailed transcript",
            Action::WorkflowView => "workflow view",
            Action::Editor => "edit the prompt in $EDITOR",
            Action::CrewView => "crew view, on an empty prompt",
            Action::AgentPanel => "agent panel, on an empty prompt",
            Action::HistorySearch => "search prompt history",
            Action::Shortcuts => "shortcuts, on an empty prompt",
            Action::TakeBack => "take queued messages back",
            Action::FocusCard => "answer a waiting card",
        }
    }

    /// The `?` panel's words after the key, short as Claude Code writes them: `to <verb>` or
    /// `for <noun>`. The panel leaves out Enter, whose job is plain, and `?`, which opened it.
    fn hint(self) -> Option<&'static str> {
        match self {
            Action::Submit | Action::Shortcuts => None,
            Action::Newline => Some("for newline"),
            Action::Interrupt => Some("to interrupt"),
            Action::ClearOrExit => Some("to clear input"),
            Action::Exit => Some("to exit"),
            Action::CycleApprovalMode => Some("to cycle modes"),
            Action::Transcript => Some("for the transcript"),
            Action::WorkflowView => Some("for workflows"),
            Action::Editor => Some("to edit in $EDITOR"),
            Action::CrewView => Some("for the crew"),
            Action::AgentPanel => Some("for agents"),
            Action::HistorySearch => Some("for history"),
            Action::TakeBack => Some("to edit queued"),
            Action::FocusCard => Some("to answer a card"),
        }
    }

    /// Where an override for this action applies. The transcript key also closes it.
    fn layers(self) -> &'static [Layer] {
        match self {
            Action::Submit | Action::Newline | Action::HistorySearch => &[Layer::Composer],
            Action::Shortcuts => &[Layer::Popup],
            Action::Transcript => &[Layer::Overlay, Layer::Global],
            _ => &[Layer::Global],
        }
    }

    fn defaults(self) -> &'static [&'static str] {
        match self {
            Action::Submit => &["enter"],
            Action::Newline => &["shift+enter", "alt+enter", "ctrl+j"],
            Action::Interrupt => &["esc"],
            Action::ClearOrExit => &["ctrl+c"],
            Action::Exit => &["ctrl+d"],
            Action::CycleApprovalMode => &["shift+tab"],
            Action::Transcript => &["ctrl+o"],
            Action::WorkflowView => &["ctrl+t"],
            Action::Editor => &["ctrl+g"],
            Action::CrewView => &["left"],
            Action::AgentPanel => &["down"],
            Action::HistorySearch => &["ctrl+r"],
            Action::Shortcuts => &["?"],
            Action::TakeBack => &["up"],
            Action::FocusCard => &["tab", "up"],
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Keys are kept in their written form, such as `ctrl+c` or `shift+tab`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub bindings: BTreeMap<(Layer, Action), Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeymapError {
    #[error("unknown action `{0}` in ui.keymap")]
    UnknownAction(String),
    #[error("can't read the key `{0}` in ui.keymap")]
    BadKey(String),
}

impl Default for Keymap {
    fn default() -> Self {
        let mut bindings = BTreeMap::new();
        for action in Action::ALL {
            let keys: Vec<String> = action.defaults().iter().map(|&k| k.to_owned()).collect();
            for &layer in action.layers() {
                bindings.insert((layer, action), keys.clone());
            }
        }
        Self { bindings }
    }
}

impl Keymap {
    /// The defaults, with `ui.keymap` overrides on top. The first bad entry is an error.
    pub fn from_settings(overrides: &BTreeMap<String, String>) -> Result<Self, KeymapError> {
        let (keymap, mut errors) = Self::with_overrides(overrides);
        if errors.is_empty() {
            Ok(keymap)
        } else {
            Err(errors.remove(0))
        }
    }

    /// Applies every override that reads, and returns the ones that don't, so one typo in the
    /// settings doesn't cost every other rebinding.
    pub fn with_overrides(overrides: &BTreeMap<String, String>) -> (Self, Vec<KeymapError>) {
        let mut keymap = Self::default();
        let mut errors = Vec::new();
        for (name, keys) in overrides {
            let Some(action) = Action::from_name(name) else {
                errors.push(KeymapError::UnknownAction(name.clone()));
                continue;
            };
            match parse_keys(keys) {
                Ok(keys) => keymap.rebind(action, keys),
                Err(error) => errors.push(error),
            }
        }
        (keymap, errors)
    }

    /// `action` takes these keys in its layers, and no other action there keeps them.
    pub fn rebind(&mut self, action: Action, keys: Vec<KeySpec>) {
        let written: Vec<String> = keys.iter().map(ToString::to_string).collect();
        for &layer in action.layers() {
            for ((other_layer, other), bound) in &mut self.bindings {
                if *other_layer == layer && *other != action {
                    bound.retain(|key| !written.contains(key));
                }
            }
            self.bindings.insert((layer, action), written.clone());
        }
    }

    /// The first action bound to `key` in `layer`.
    pub fn action_for(&self, layer: Layer, key: &KeyEvent) -> Option<Action> {
        self.actions_for(layer, key).into_iter().next()
    }

    /// Every action bound to `key` in `layer`, in [`Action::ALL`] order, for keys that mean
    /// different things by context, such as ↑.
    pub fn actions_for(&self, layer: Layer, key: &KeyEvent) -> Vec<Action> {
        let pressed = KeySpec::from_event(key).to_string();
        self.bindings
            .iter()
            .filter(|((bound_layer, _), keys)| *bound_layer == layer && keys.contains(&pressed))
            .map(|((_, action), _)| *action)
            .collect()
    }

    /// Whether `key` is bound to `action` in any of its layers.
    pub fn is(&self, action: Action, key: &KeyEvent) -> bool {
        let pressed = KeySpec::from_event(key).to_string();
        action.layers().iter().any(|&layer| {
            self.bindings
                .get(&(layer, action))
                .is_some_and(|keys| keys.contains(&pressed))
        })
    }

    /// The keys bound to `action`, as written, such as `ctrl+c`.
    pub fn keys(&self, action: Action) -> Vec<String> {
        let layer = action.layers()[0];
        self.bindings
            .get(&(layer, action))
            .cloned()
            .unwrap_or_default()
    }

    /// The `?` panel's hints: `/` and `@`, then each bound action the panel lists with its first
    /// key written with spaces, such as `ctrl + o for the transcript`. Short hints keep the
    /// panel to three columns at 80 cells, as Claude Code's is.
    pub fn shortcut_hints(&self) -> Vec<String> {
        let typed = [
            "! for shell mode",
            "/ for commands",
            "@ for agents and cards",
        ]
        .map(str::to_owned);
        let bound = Action::ALL.into_iter().filter_map(|action| {
            let hint = action.hint()?;
            let key = self.keys(action).into_iter().next()?;
            Some(format!("{} {hint}", spaced(&key)))
        });
        typed.into_iter().chain(bound).collect()
    }

    /// `(keys, action)` rows for the help view and the `?` panel.
    pub fn rows(&self) -> Vec<(String, Action)> {
        Action::ALL
            .into_iter()
            .filter_map(|action| {
                let keys = self.keys(action);
                (!keys.is_empty()).then(|| (keys.join(" / "), action))
            })
            .collect()
    }
}

/// One key with its modifiers, in a form two spellings of the same key share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeySpec {
    /// Shift is part of a typed character, so `?` is the same key with or without it, and
    /// Shift+Tab arrives as BackTab.
    pub fn from_event(key: &KeyEvent) -> Self {
        let wanted = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT;
        let mut modifiers = key.modifiers & wanted;
        let code = match key.code {
            KeyCode::BackTab => {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            KeyCode::Char(c) if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                if c.is_uppercase() {
                    modifiers |= KeyModifiers::SHIFT;
                }
                KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
            }
            KeyCode::Char(c) => {
                let shifted = modifiers.contains(KeyModifiers::SHIFT);
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::Char(if shifted {
                    c.to_uppercase().next().unwrap_or(c)
                } else {
                    c
                })
            }
            code => code,
        };
        Self { code, modifiers }
    }

    pub fn parse(text: &str) -> Result<Self, KeymapError> {
        let bad = || KeymapError::BadKey(text.to_owned());
        let lower = text.trim().to_lowercase();
        if lower.is_empty() {
            return Err(bad());
        }
        // `+` alone, or at the end after another `+`, is the plus key itself.
        let (prefix, last) = match lower.strip_suffix("++") {
            Some(prefix) => (prefix, "+"),
            None if lower == "+" => ("", "+"),
            None => match lower.rsplit_once('+') {
                Some((prefix, last)) => (prefix, last),
                None => ("", lower.as_str()),
            },
        };
        let mut modifiers = KeyModifiers::NONE;
        for part in prefix.split('+').filter(|part| !part.is_empty()) {
            modifiers |= match part.trim() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" | "option" | "meta" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                _ => return Err(bad()),
            };
        }
        let code = match last.trim() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            "space" => KeyCode::Char(' '),
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            other => {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    (Some('f'), Some(_)) => other[1..]
                        .parse::<u8>()
                        .ok()
                        .filter(|n| (1..=24).contains(n))
                        .map(KeyCode::F)
                        .ok_or_else(bad)?,
                    _ => return Err(bad()),
                }
            }
        };
        Ok(Self::from_event(&KeyEvent::new(code, modifiers)))
    }
}

impl fmt::Display for KeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (modifier, name) in [
            (KeyModifiers::CONTROL, "ctrl+"),
            (KeyModifiers::ALT, "alt+"),
            (KeyModifiers::SHIFT, "shift+"),
        ] {
            if self.modifiers.contains(modifier) {
                f.write_str(name)?;
            }
        }
        match self.code {
            KeyCode::Enter => f.write_str("enter"),
            KeyCode::Esc => f.write_str("esc"),
            KeyCode::Tab | KeyCode::BackTab => f.write_str("tab"),
            KeyCode::Char(' ') => f.write_str("space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Up => f.write_str("up"),
            KeyCode::Down => f.write_str("down"),
            KeyCode::Left => f.write_str("left"),
            KeyCode::Right => f.write_str("right"),
            KeyCode::Home => f.write_str("home"),
            KeyCode::End => f.write_str("end"),
            KeyCode::PageUp => f.write_str("pageup"),
            KeyCode::PageDown => f.write_str("pagedown"),
            KeyCode::Backspace => f.write_str("backspace"),
            KeyCode::Delete => f.write_str("delete"),
            KeyCode::Insert => f.write_str("insert"),
            KeyCode::F(n) => write!(f, "f{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// A written key with spaces around each `+` that joins its parts, so `ctrl++` reads
/// `ctrl + +`.
fn spaced(key: &str) -> String {
    let mut parts = Vec::new();
    let mut rest = key;
    while let Some((modifier, after)) = rest
        .split_once('+')
        .filter(|(modifier, after)| !modifier.is_empty() && !after.is_empty())
    {
        parts.push(modifier);
        rest = after;
    }
    parts.push(rest);
    parts.join(" + ")
}

/// One key, or several separated by commas, such as `ctrl+c, ctrl+q`.
fn parse_keys(text: &str) -> Result<Vec<KeySpec>, KeymapError> {
    let keys = text
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .map(KeySpec::parse)
        .collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() {
        return Err(KeymapError::BadKey(text.to_owned()));
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn keymap_rebinding() {
        let keymap = Keymap::default();
        let left = key(KeyCode::Left, KeyModifiers::NONE);
        let ctrl_left = key(KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(
            keymap.action_for(Layer::Global, &left),
            Some(Action::CrewView)
        );
        assert_eq!(keymap.action_for(Layer::Global, &ctrl_left), None);

        let overrides = BTreeMap::from([
            ("crew_view".to_owned(), "ctrl+left".to_owned()),
            // Steals ↓ from the agent panel.
            ("take-back".to_owned(), "Down".to_owned()),
            ("clear_or_exit".to_owned(), "ctrl+c, ctrl+q".to_owned()),
        ]);
        let keymap = Keymap::from_settings(&overrides).unwrap();
        assert_eq!(keymap.action_for(Layer::Global, &left), None);
        assert_eq!(
            keymap.action_for(Layer::Global, &ctrl_left),
            Some(Action::CrewView)
        );
        let down = key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(keymap.actions_for(Layer::Global, &down), [Action::TakeBack]);
        assert!(keymap.keys(Action::AgentPanel).is_empty());
        // ↑ still gives a card the keys.
        let up = key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(keymap.actions_for(Layer::Global, &up), [Action::FocusCard]);
        let ctrl_q = key(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert!(keymap.is(Action::ClearOrExit, &ctrl_q));
        // Layers keep their own keys: the composer's Enter isn't a global key.
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(keymap.action_for(Layer::Global, &enter), None);
        assert_eq!(
            keymap.action_for(Layer::Composer, &enter),
            Some(Action::Submit)
        );
        assert!(
            keymap
                .rows()
                .contains(&("ctrl+c / ctrl+q".to_owned(), Action::ClearOrExit))
        );
        assert!(
            !keymap
                .rows()
                .iter()
                .any(|(_, action)| *action == Action::AgentPanel)
        );

        let typo = BTreeMap::from([
            ("crew_veiw".to_owned(), "ctrl+left".to_owned()),
            ("editor".to_owned(), "ctrl+e".to_owned()),
            ("exit".to_owned(), "hyper+x".to_owned()),
        ]);
        assert_eq!(
            Keymap::from_settings(&typo),
            Err(KeymapError::UnknownAction("crew_veiw".into()))
        );
        let (keymap, errors) = Keymap::with_overrides(&typo);
        assert_eq!(
            errors,
            [
                KeymapError::UnknownAction("crew_veiw".into()),
                KeymapError::BadKey("hyper+x".into()),
            ]
        );
        assert!(keymap.is(
            Action::Editor,
            &key(KeyCode::Char('e'), KeyModifiers::CONTROL)
        ));
        assert_eq!(keymap.keys(Action::Exit), ["ctrl+d"]);
    }

    #[test]
    fn spellings_of_one_key_match() {
        let shift_tab = key(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(KeySpec::from_event(&shift_tab).to_string(), "shift+tab");
        for (text, event) in [
            ("shift+tab", shift_tab),
            ("BackTab", shift_tab),
            ("?", key(KeyCode::Char('?'), KeyModifiers::SHIFT)),
            ("ctrl+C", key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            (
                "ctrl+shift+c",
                key(
                    KeyCode::Char('C'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
            ),
            ("alt+enter", key(KeyCode::Enter, KeyModifiers::ALT)),
            ("ctrl++", key(KeyCode::Char('+'), KeyModifiers::CONTROL)),
            ("space", key(KeyCode::Char(' '), KeyModifiers::NONE)),
            ("shift+a", key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            ("f5", key(KeyCode::F(5), KeyModifiers::NONE)),
        ] {
            assert_eq!(
                KeySpec::parse(text).unwrap(),
                KeySpec::from_event(&event),
                "{text}"
            );
        }
        for bad in ["", "ctrl+", "hyper+x", "f99", "enterr"] {
            assert!(KeySpec::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn shortcut_hints_are_short_key_to_or_for_phrases() {
        let hints = Keymap::default().shortcut_hints();
        for hint in [
            "! for shell mode",
            "esc to interrupt",
            "shift + enter for newline",
            "shift + tab to cycle modes",
            "ctrl + o for the transcript",
            "ctrl + g to edit in $EDITOR",
            "tab to answer a card",
        ] {
            assert!(hints.iter().any(|h| h == hint), "{hint}: {hints:?}");
        }
        for hint in &hints {
            assert!(hint.contains(" to ") || hint.contains(" for "), "{hint}");
            assert!(crate::text::str_width(hint) <= 27, "{hint}");
        }
        assert!(
            !hints
                .iter()
                .any(|h| h.starts_with("enter ") || h.starts_with("? "))
        );

        let overrides = BTreeMap::from([("editor".to_owned(), "ctrl++, ctrl+e".to_owned())]);
        let hints = Keymap::from_settings(&overrides).unwrap().shortcut_hints();
        assert!(
            hints.iter().any(|h| h == "ctrl + + to edit in $EDITOR"),
            "{hints:?}"
        );
    }

    #[test]
    fn every_action_has_a_name_and_a_default() {
        let keymap = Keymap::default();
        for action in Action::ALL {
            assert_eq!(Action::from_name(action.name()), Some(action));
            assert!(!keymap.keys(action).is_empty(), "{action}");
            for key in keymap.keys(action) {
                let spec = KeySpec::parse(&key).unwrap();
                assert_eq!(spec.to_string(), key);
            }
        }
    }
}
