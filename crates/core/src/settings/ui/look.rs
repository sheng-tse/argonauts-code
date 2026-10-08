//! How numbers look everywhere: the theme, colors, wording, levels, and state glyphs.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// Types written as one string in settings, checked when read.
macro_rules! written_as_text {
    ($($name:ident),* $(,)?) => {$(
        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(generator: &mut SchemaGenerator) -> Schema {
                String::json_schema(generator)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.to_string()
            }
        }
    )*};
}

written_as_text!(Theme, ColorText, Percent, Threshold);

/// `auto`, Claude Code's six themes, or `custom:<name>` for a theme file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Theme {
    /// Asks the terminal for its background and picks light or dark.
    #[default]
    Auto,
    Dark,
    Light,
    DarkDaltonized,
    LightDaltonized,
    DarkAnsi,
    LightAnsi,
    /// `~/.argonauts/themes/<name>.toml`, or the project's own.
    Custom(String),
}

impl Theme {
    /// Every theme but the custom ones, as `ui.theme` names them.
    pub const BUILT_IN: [&'static str; 7] = [
        "auto",
        "dark",
        "light",
        "dark-daltonized",
        "light-daltonized",
        "dark-ansi",
        "light-ansi",
    ];

    /// Whether the theme is light, when its name says. Auto and custom themes leave it to
    /// the terminal or the theme file.
    pub fn is_light(&self) -> Option<bool> {
        match self {
            Theme::Dark | Theme::DarkDaltonized | Theme::DarkAnsi => Some(false),
            Theme::Light | Theme::LightDaltonized | Theme::LightAnsi => Some(true),
            Theme::Auto | Theme::Custom(_) => None,
        }
    }
}

impl fmt::Display for Theme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Theme::Auto => "auto",
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::DarkDaltonized => "dark-daltonized",
            Theme::LightDaltonized => "light-daltonized",
            Theme::DarkAnsi => "dark-ansi",
            Theme::LightAnsi => "light-ansi",
            Theme::Custom(name) => return write!(f, "custom:{name}"),
        };
        f.write_str(name)
    }
}

impl TryFrom<String> for Theme {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let text = value.trim();
        if let Some(name) = text.strip_prefix("custom:") {
            return if is_file_name(name) {
                Ok(Theme::Custom(name.to_owned()))
            } else {
                Err(format!(
                    "custom:{name} needs a theme file's name, such as custom:harbor"
                ))
            };
        }
        let theme = match text.replace('_', "-").as_str() {
            "auto" => Theme::Auto,
            "dark" => Theme::Dark,
            "light" => Theme::Light,
            "dark-daltonized" => Theme::DarkDaltonized,
            "light-daltonized" => Theme::LightDaltonized,
            "dark-ansi" => Theme::DarkAnsi,
            "light-ansi" => Theme::LightAnsi,
            _ => {
                return Err(format!(
                    "use {}, or custom:<name> for a theme file",
                    Theme::BUILT_IN.join(", ")
                ));
            }
        };
        Ok(theme)
    }
}

/// A theme file's name never leaves its folder.
fn is_file_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A color in Claude Code's forms: `#rrggbb`, `#rgb`, `rgb(r,g,b)`, `ansi256(n)`,
/// `ansi:<name>`, or another role's name, such as `warning`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ColorText(String);

impl ColorText {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether it names another role rather than a color.
    pub fn is_role(&self) -> bool {
        is_role_name(&self.0)
    }
}

impl fmt::Display for ColorText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ColorText {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let text = value.trim();
        if is_color(text) || is_role_name(text) {
            Ok(ColorText(text.to_owned()))
        } else {
            Err(format!(
                "{text:?} isn't a color: use #rrggbb, #rgb, rgb(r,g,b), ansi256(n), ansi:<name>, \
                 or a role's name"
            ))
        }
    }
}

impl From<&str> for ColorText {
    /// For built-in defaults, which are always good.
    fn from(text: &str) -> Self {
        ColorText(text.to_owned())
    }
}

const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

fn is_color(text: &str) -> bool {
    if let Some(hex) = text.strip_prefix('#') {
        return matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    if let Some(inner) = text.strip_prefix("rgb(").and_then(|t| t.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        return parts.len() == 3 && parts.iter().all(|part| part.parse::<u8>().is_ok());
    }
    if let Some(n) = text
        .strip_prefix("ansi256(")
        .and_then(|t| t.strip_suffix(')'))
    {
        return n.trim().parse::<u8>().is_ok();
    }
    if let Some(name) = text.strip_prefix("ansi:") {
        // Claude Code writes `redBright`; the role table writes `brightBlack`. Both read.
        let lower = name.to_ascii_lowercase();
        let base = lower
            .strip_prefix("bright")
            .or_else(|| lower.strip_suffix("bright"))
            .unwrap_or(&lower);
        return ANSI_NAMES.contains(&base);
    }
    false
}

/// Role names are lowercase words joined by `-`, such as `state-working`.
fn is_role_name(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_lowercase())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A share written as a percent, such as `"70%"`. Settings never take ratios as floats.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Percent(f64);

impl Percent {
    pub const fn new(percent: f64) -> Self {
        Percent(percent)
    }

    /// The number before the `%`.
    pub fn value(self) -> f64 {
        self.0
    }
}

impl fmt::Display for Percent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.0)
    }
}

impl TryFrom<String> for Percent {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let text = value.trim();
        text.strip_suffix('%')
            .and_then(|number| number.trim().parse::<f64>().ok())
            .filter(|percent| percent.is_finite() && *percent >= 0.0)
            .map(Percent)
            .ok_or_else(|| format!("{text:?} isn't a percent: write it as \"80%\""))
    }
}

/// Where a level starts: a percent, or `soft_stop` for `limits.soft_stop`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Threshold {
    Percent(Percent),
    /// Where Argonauts starts asking for more budget.
    SoftStop,
}

impl Threshold {
    pub const fn percent(percent: f64) -> Self {
        Threshold::Percent(Percent::new(percent))
    }
}

impl fmt::Display for Threshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Threshold::Percent(percent) => percent.fmt(f),
            Threshold::SoftStop => f.write_str("soft_stop"),
        }
    }
}

impl TryFrom<String> for Threshold {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim() == "soft_stop" {
            return Ok(Threshold::SoftStop);
        }
        Percent::try_from(value)
            .map(Threshold::Percent)
            .map_err(|_| "write a percent such as \"80%\", or soft_stop".to_owned())
    }
}

/// One wording for each kind of number.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct FormatSettings {
    /// Token counts: short (212k, 1.2M), lower (Claude Code's 1.2m), or full (212,400)
    pub tokens: TokenStyle,
    /// Plan usage says how much is used or how much is left, everywhere it shows
    #[serde(rename = "plan_usage")]
    pub quota: QuotaWording,
    /// Clock times: 12h (4:40pm) or 24h (16:40)
    pub clock: ClockStyle,
    /// When plan usage resets: a clock time, a countdown, or both
    pub reset: ResetStyle,
    /// Durations in rows and views: compact (4m12s) or spaced (4m 12s)
    pub duration: DurationStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenStyle {
    #[default]
    Short,
    Lower,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWording {
    #[default]
    Used,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub enum ClockStyle {
    #[default]
    #[serde(rename = "12h")]
    TwelveHour,
    #[serde(rename = "24h")]
    TwentyFourHour,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResetStyle {
    #[default]
    Clock,
    Countdown,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DurationStyle {
    #[default]
    Compact,
    Spaced,
}

/// What every meter, badge, and colored number inherits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LevelSettings {
    /// Where a value turns amber, as a percent of its limit
    pub warn: Percent,
    /// Where a value turns red, as a percent of its limit
    pub danger: Percent,
    /// How far under its line a value must fall before its level drops back
    pub hysteresis: Percent,
    /// The crossings that ring the bell or send a notification
    pub notify: Vec<Crossing>,
}

impl Default for LevelSettings {
    fn default() -> Self {
        Self {
            warn: Percent::new(70.0),
            danger: Percent::new(90.0),
            hysteresis: Percent::new(2.0),
            notify: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Crossing {
    Warn,
    Danger,
}

/// A state, which looks the same everywhere it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Working,
    /// Waiting on other work.
    Waiting,
    /// Waiting on a timer, such as a quota's reset.
    Timer,
    NeedsYou,
    /// Done, merged, or passed.
    Done,
    /// Failed, crashed, or out.
    Failed,
    /// Stopped or paused.
    Stopped,
}

impl State {
    pub const ALL: [State; 7] = [
        State::Working,
        State::Waiting,
        State::Timer,
        State::NeedsYou,
        State::Done,
        State::Failed,
        State::Stopped,
    ];

    pub fn glyph(self) -> &'static str {
        match self {
            State::Working => "◐",
            State::Waiting => "○",
            State::Timer => "✢",
            State::NeedsYou => "◆",
            State::Done => "✔",
            State::Failed => "✗",
            State::Stopped => "■",
        }
    }

    /// The color role it draws in, which points at another role so one change moves them all.
    pub fn role(self) -> &'static str {
        match self {
            State::Working => "state-working",
            State::Waiting => "state-waiting",
            State::Timer => "state-timer",
            State::NeedsYou => "state-needs-you",
            State::Done => "state-done",
            State::Failed => "state-failed",
            State::Stopped => "state-stopped",
        }
    }

    fn style(self) -> StateStyle {
        StateStyle {
            glyph: Some(self.glyph().to_owned()),
            color: Some(ColorText::from(self.role())),
        }
    }
}

/// One glyph and one color for each state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct StatesSettings {
    /// Running, building, or checking its gates
    pub working: StateStyle,
    /// Waiting on other work
    pub waiting: StateStyle,
    /// Waiting on a timer, such as a quota's reset
    pub timer: StateStyle,
    /// Waiting on you
    pub needs_you: StateStyle,
    /// Done, merged, or passed
    pub done: StateStyle,
    /// Failed, crashed, or out
    pub failed: StateStyle,
    /// Stopped or paused
    pub stopped: StateStyle,
}

impl Default for StatesSettings {
    fn default() -> Self {
        Self {
            working: State::Working.style(),
            waiting: State::Waiting.style(),
            timer: State::Timer.style(),
            needs_you: State::NeedsYou.style(),
            done: State::Done.style(),
            failed: State::Failed.style(),
            stopped: State::Stopped.style(),
        }
    }
}

impl StatesSettings {
    /// The glyph and color `state` draws with, its built-in ones where a table leaves them out.
    pub fn look(&self, state: State) -> (String, ColorText) {
        let own = match state {
            State::Working => &self.working,
            State::Waiting => &self.waiting,
            State::Timer => &self.timer,
            State::NeedsYou => &self.needs_you,
            State::Done => &self.done,
            State::Failed => &self.failed,
            State::Stopped => &self.stopped,
        };
        let glyph = own
            .glyph
            .clone()
            .filter(|glyph| !glyph.trim().is_empty())
            .unwrap_or_else(|| state.glyph().to_owned());
        let color = own
            .color
            .clone()
            .unwrap_or_else(|| ColorText::from(state.role()));
        (glyph, color)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct StateStyle {
    /// The glyph this state shows
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    /// The role or color this state draws in
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorText>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read<T: TryFrom<String, Error = String>>(text: &str) -> Result<T, String> {
        T::try_from(text.to_owned())
    }

    #[test]
    fn themes_read_by_name_and_custom_ones_by_file() {
        for name in Theme::BUILT_IN {
            let theme: Theme = read(name).unwrap();
            assert_eq!(theme.to_string(), name);
        }
        assert_eq!(read::<Theme>("dark_ansi"), Ok(Theme::DarkAnsi));
        assert_eq!(
            read::<Theme>("custom:harbor"),
            Ok(Theme::Custom("harbor".into()))
        );
        assert!(read::<Theme>("custom:../etc").is_err());
        assert!(read::<Theme>("solarized").is_err());
        assert_eq!(Theme::LightAnsi.is_light(), Some(true));
        assert_eq!(Theme::Custom("x".into()).is_light(), None);
    }

    #[test]
    fn colors_take_claude_codes_forms_and_role_names() {
        for good in [
            "#b1b9f9",
            "#fff",
            "rgb(1, 2, 255)",
            "ansi256(205)",
            "ansi:brightBlack",
            "ansi:yellowBright",
            "ansi:red",
            "state-working",
            "accent",
        ] {
            assert!(read::<ColorText>(good).is_ok(), "{good}");
        }
        for bad in [
            "#12345",
            "rgb(1,2)",
            "rgb(1,2,300)",
            "ansi256(256)",
            "ansi:pink",
            "Warning",
            "",
        ] {
            assert!(read::<ColorText>(bad).is_err(), "{bad}");
        }
        assert!(ColorText::from("card").is_role());
        assert!(!ColorText::from("#fff").is_role());
    }

    #[test]
    fn percents_are_written_with_a_sign_and_round_trip() {
        let percent: Percent = read(" 80% ").unwrap();
        assert_eq!(percent.value(), 80.0);
        assert_eq!(percent.to_string(), "80%");
        assert_eq!(read::<Percent>("2.5%").unwrap().to_string(), "2.5%");
        for bad in ["0.8", "80", "-5%", "lots%"] {
            assert!(read::<Percent>(bad).is_err(), "{bad}");
        }
        assert_eq!(read::<Threshold>("soft_stop"), Ok(Threshold::SoftStop));
        assert_eq!(read::<Threshold>("100%"), Ok(Threshold::percent(100.0)));
        assert!(read::<Threshold>("soon").is_err());
    }

    #[test]
    fn a_state_table_left_partial_keeps_the_states_own_look() {
        let mut states = StatesSettings::default();
        assert_eq!(
            states.look(State::NeedsYou),
            ("◆".to_owned(), ColorText::from("state-needs-you"))
        );
        states.done = StateStyle {
            glyph: Some("+".into()),
            color: None,
        };
        assert_eq!(
            states.look(State::Done),
            ("+".to_owned(), ColorText::from("state-done"))
        );
    }
}
