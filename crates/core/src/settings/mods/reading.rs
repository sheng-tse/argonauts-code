//! What a mod draws: a few words, how it's going, and its parts. A built-in source fills one,
//! and a command prints one as JSON:
//!
//! ```json
//! {"summary": "7 of 9 passed", "level": "danger",
//!  "parts": [{"label": "passed", "value": 7, "color": "green"},
//!            {"label": "running", "value": 1, "color": "amber"},
//!            {"label": "failed", "value": 1, "color": "red"}]}
//! ```

use serde::{Deserialize, Serialize};

use super::ModBody;

/// The context mod's sample before a session counts its context: Claude Code's categories with
/// a fresh conversation's numbers in a window of a million tokens, and the free space, which
/// leaves out the 33k held back for compacting.
const CONTEXT_SAMPLE: [(&str, u64); 7] = [
    ("System prompt", 4_380),
    ("System tools", 19_380),
    ("MCP tools", 705),
    ("MCP server instructions", 960),
    ("Memory files", 503),
    ("Skills", 6_360),
    ("Messages", 10),
];
const CONTEXT_SAMPLE_WINDOW: u64 = 1_000_000;
const CONTEXT_SAMPLE_HELD: u64 = 33_000;

/// The most parts one reading keeps. A card has no room for more.
pub const MOST_PARTS: usize = 64;

/// The largest value a part may have: a thousand million million, beyond any count a card
/// draws, and small enough that the parts of one card always add up.
const MOST_VALUE: f64 = 1e15;
/// The most characters a label, a note or the summary keeps.
const MOST_CHARS: usize = 120;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModReading {
    /// The header's words. The first one is drawn bold, as the headline number.
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub level: ModLevel,
    /// The badge's words. Unset, a reading that isn't ok names the part that made it so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    #[serde(default)]
    pub parts: Vec<ModPart>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModPart {
    pub label: String,
    #[serde(default)]
    pub value: f64,
    /// What `value` is measured against, which a meter needs. Unset, the meter measures
    /// against the largest part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ModColor>,
    /// How the value reads, such as `$1.86` or `840k`, where the plain number won't do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Dim words after the number, such as `resets 4:40pm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The CLI the part stands for, by its id such as `claude-code`: the part then draws in
    /// the CLI's hue with its logo before its label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
}

impl ModPart {
    pub fn new(label: &str, value: f64) -> Self {
        Self {
            label: label.to_owned(),
            value,
            ..Self::default()
        }
    }

    pub fn colored(mut self, color: ModColor) -> Self {
        self.color = Some(color);
        self
    }

    pub fn of(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }

    pub fn reading(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn noted(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn on_cli(mut self, cli: impl Into<String>) -> Self {
        self.cli = Some(cli.into());
        self
    }

    /// The value as the card writes it: its own text, else the number without a needless
    /// fraction.
    pub fn amount(&self) -> String {
        match &self.text {
            Some(text) => text.clone(),
            None => plain_number(self.value),
        }
    }
}

/// `7` for 7.0, `0.5` for 0.5, `1.2k` for 1234, `3.4M` for 3,400,000, `1M` for a million.
pub fn plain_number(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1_000_000.0 {
        let millions = format!("{:.1}", value / 1_000_000.0);
        format!("{}M", millions.strip_suffix(".0").unwrap_or(&millions))
    } else if abs >= 10_000.0 {
        format!("{:.0}k", value / 1_000.0)
    } else if abs >= 1_000.0 {
        format!("{:.1}k", value / 1_000.0)
    } else if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// How a reading is going, which colors its badge.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ModLevel {
    #[default]
    Ok,
    Warn,
    Danger,
}

/// The colors a part can ask for. Each is a color of the theme, so a mod looks right in the
/// dark and the light one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModColor {
    Green,
    Amber,
    Red,
    Blue,
    Purple,
    Cyan,
    Orange,
    Pink,
    Gold,
    Grey,
}

impl ModColor {
    pub const ALL: [ModColor; 10] = [
        ModColor::Green,
        ModColor::Amber,
        ModColor::Red,
        ModColor::Blue,
        ModColor::Purple,
        ModColor::Cyan,
        ModColor::Orange,
        ModColor::Pink,
        ModColor::Gold,
        ModColor::Grey,
    ];

    pub fn word(self) -> &'static str {
        match self {
            ModColor::Green => "green",
            ModColor::Amber => "amber",
            ModColor::Red => "red",
            ModColor::Blue => "blue",
            ModColor::Purple => "purple",
            ModColor::Cyan => "cyan",
            ModColor::Orange => "orange",
            ModColor::Pink => "pink",
            ModColor::Gold => "gold",
            ModColor::Grey => "grey",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.word() == word)
    }

    /// The level a part of this color stands for: red is danger, amber a warning.
    fn level(self) -> ModLevel {
        match self {
            ModColor::Red => ModLevel::Danger,
            ModColor::Amber => ModLevel::Warn,
            _ => ModLevel::Ok,
        }
    }
}

impl ModReading {
    /// What a command printed. JSON is read as a reading. Anything else is taken as words, its
    /// first line the summary, so `git branch --show-current` works as it is.
    pub fn from_output(output: &str) -> Result<Self, String> {
        let text = output.trim();
        if text.is_empty() {
            return Err("it printed nothing".to_owned());
        }
        if !text.starts_with('{') {
            let line = text.lines().next().unwrap_or_default();
            return Ok(Self {
                summary: clip(line),
                ..Self::default()
            });
        }
        let mut reading: ModReading =
            serde_json::from_str(text).map_err(|err| format!("its JSON doesn't read: {err}"))?;
        reading.tidy()?;
        Ok(reading)
    }

    /// Cuts what's too long and refuses what can't be drawn.
    fn tidy(&mut self) -> Result<(), String> {
        self.summary = clip(&self.summary);
        if let Some(badge) = &mut self.badge {
            *badge = clip(badge);
        }
        self.parts.truncate(MOST_PARTS);
        for part in &mut self.parts {
            if !part.value.is_finite() || part.max.is_some_and(|max| !max.is_finite()) {
                return Err(format!("{}'s value isn't a number", part.label));
            }
            if part.value < 0.0 {
                return Err(format!("{}'s value is below zero", part.label));
            }
            // Far past anything a card can show, and past what its drawing can add up.
            if part.value > MOST_VALUE || part.max.is_some_and(|max| max > MOST_VALUE) {
                return Err(format!("{}'s value is too large to draw", part.label));
            }
            part.label = clip(&part.label);
            part.text = part.text.as_deref().map(clip);
            part.note = part.note.as_deref().map(clip);
        }
        Ok(())
    }

    pub fn total(&self) -> f64 {
        self.parts.iter().map(|part| part.value).sum()
    }

    /// The badge's words: its own, else for a reading that isn't ok the part of that level's
    /// color, such as `1 failed`.
    pub fn badge_words(&self) -> Option<String> {
        if let Some(badge) = &self.badge {
            return (!badge.is_empty()).then(|| badge.clone());
        }
        if self.level == ModLevel::Ok {
            return None;
        }
        self.parts
            .iter()
            .find(|part| {
                part.value > 0.0 && part.color.is_some_and(|color| color.level() == self.level)
            })
            .map(|part| format!("{} {}", part.amount(), part.label))
    }

    /// Numbers to preview a mod with before it has any of its own: shaped like its built-in
    /// source where it reads one, so the preview looks like what will come, and like its body
    /// otherwise.
    pub fn sample_for(file: &super::ModFile) -> Self {
        use super::{BuiltinSource, ModSource};
        let part = |label: &str, value: f64, text: &str| ModPart::new(label, value).reading(text);
        match &file.data {
            ModSource::Builtin {
                source: BuiltinSource::Context,
            } => Self {
                summary: "32.3k of 1M · compacts at 967k".to_owned(),
                parts: context_sample(),
                ..Self::default()
            },
            ModSource::Builtin {
                source: BuiltinSource::PlanUsage,
            } => Self {
                summary: "56% Claude Code 7d".to_owned(),
                parts: vec![
                    ModPart::new("Claude Code 5h", 1.0)
                        .of(100.0)
                        .noted("resets in 4h 40m")
                        .on_cli("claude-code"),
                    ModPart::new("Claude Code 7d", 56.0)
                        .of(100.0)
                        .noted("resets in 3d")
                        .on_cli("claude-code"),
                    ModPart::new("Codex 7d", 11.0)
                        .of(100.0)
                        .noted("resets in 5d")
                        .on_cli("codex"),
                ],
                ..Self::default()
            },
            ModSource::Builtin {
                source: BuiltinSource::Spend,
            } => Self {
                summary: "~$1.86 today".to_owned(),
                parts: vec![
                    part("Claude Code", 1.31, "~$1.31").on_cli("claude-code"),
                    part("Codex", 0.45, "$0.45").on_cli("codex"),
                    part("agy", 0.10, "~$0.10").on_cli("antigravity-cli"),
                ],
                ..Self::default()
            },
            ModSource::Builtin {
                source: BuiltinSource::Tokens,
            } => Self {
                summary: "1.4M tokens today".to_owned(),
                parts: vec![
                    part("from cache", 950_000.0, "950k").colored(ModColor::Blue),
                    part("new input", 380_000.0, "380k").colored(ModColor::Purple),
                    part("output", 70_000.0, "70k").colored(ModColor::Cyan),
                ],
                ..Self::default()
            },
            _ => Self::sample(file.body),
        }
    }

    /// Numbers to draw a body with, for a mod whose own will come from a command.
    pub fn sample(body: ModBody) -> Self {
        let part =
            |label: &str, value: f64, color: ModColor| ModPart::new(label, value).colored(color);
        match body {
            ModBody::Stack | ModBody::Marks => Self {
                summary: "7 of 9 passed".to_owned(),
                level: ModLevel::Danger,
                badge: None,
                parts: vec![
                    part("passed", 7.0, ModColor::Green),
                    part("running", 1.0, ModColor::Amber),
                    part("failed", 1.0, ModColor::Red),
                ],
            },
            ModBody::Meters => Self {
                summary: "56% of the week".to_owned(),
                parts: vec![
                    ModPart::new("5h", 1.0).of(100.0).noted("resets 4:40pm"),
                    ModPart::new("7d", 56.0).of(100.0).noted("resets Tue"),
                ],
                ..Self::default()
            },
            ModBody::List => Self {
                summary: "3 open".to_owned(),
                parts: vec![
                    part("review the plan", 1.0, ModColor::Amber).reading("2m"),
                    part("merge add-due", 1.0, ModColor::Green).reading("now"),
                    part("fix the flaky test", 1.0, ModColor::Red).reading("1h"),
                ],
                ..Self::default()
            },
            ModBody::Columns | ModBody::Sparkline => Self {
                summary: "5.0M tokens this week".to_owned(),
                parts: [3.0, 5.0, 2.0, 4.0, 7.0, 1.0, 3.0]
                    .into_iter()
                    .zip(["M", "T", "W", "T", "F", "S", "S"])
                    .map(|(value, day)| ModPart::new(day, value))
                    .collect(),
                ..Self::default()
            },
            ModBody::Text => Self {
                summary: "main".to_owned(),
                ..Self::default()
            },
        }
    }
}

/// The context mod's sample parts: each category, then the free space in grey, each measured
/// against the window.
fn context_sample() -> Vec<ModPart> {
    let window = CONTEXT_SAMPLE_WINDOW as f64;
    let used: u64 = CONTEXT_SAMPLE.iter().map(|(_, tokens)| tokens).sum();
    let free = CONTEXT_SAMPLE_WINDOW - used - CONTEXT_SAMPLE_HELD;
    CONTEXT_SAMPLE
        .iter()
        .map(|(label, tokens)| ModPart::new(label, *tokens as f64).of(window))
        .chain([ModPart::new("Free space", free as f64)
            .of(window)
            .colored(ModColor::Grey)])
        .collect()
}

/// One line, without control characters, cut to what a card can hold.
fn clip(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MOST_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKS: &str = r#"{"summary": "7 of 9 passed", "level": "danger",
        "parts": [{"label": "passed", "value": 7, "color": "green"},
                  {"label": "running", "value": 1, "color": "amber"},
                  {"label": "failed", "value": 1, "color": "red"}]}"#;

    #[test]
    fn a_value_too_large_to_draw_is_refused() {
        let big = r#"{"summary": "big", "parts": [{"label": "a", "value": 1e19},
            {"label": "b", "value": 1e19}]}"#;
        assert_eq!(
            ModReading::from_output(big),
            Err("a's value is too large to draw".to_owned())
        );
    }

    #[test]
    fn the_readme_s_example_reads() {
        let reading = ModReading::from_output(CHECKS).unwrap();
        assert_eq!(reading.summary, "7 of 9 passed");
        assert_eq!(reading.level, ModLevel::Danger);
        assert_eq!(reading.parts.len(), 3);
        assert_eq!(reading.total(), 9.0);
        assert_eq!(reading.badge_words().as_deref(), Some("1 failed"));
        assert_eq!(reading, ModReading::sample(ModBody::Stack));
    }

    #[test]
    fn plain_words_are_the_summary() {
        let reading = ModReading::from_output("main\nsecond line\n").unwrap();
        assert_eq!(reading.summary, "main");
        assert!(reading.parts.is_empty());
        assert_eq!(reading.badge_words(), None);
    }

    #[test]
    fn what_cannot_be_drawn_is_refused_in_plain_words() {
        assert_eq!(
            ModReading::from_output("  \n").unwrap_err(),
            "it printed nothing"
        );
        assert!(
            ModReading::from_output("{\"parts\": 3}")
                .unwrap_err()
                .starts_with("its JSON doesn't read:")
        );
        assert_eq!(
            ModReading::from_output(r#"{"parts": [{"label": "x", "value": -1}]}"#).unwrap_err(),
            "x's value is below zero"
        );
        assert!(ModReading::from_output(r#"{"sumary": "typo"}"#).is_err());
    }

    #[test]
    fn long_and_odd_text_is_cut_to_one_clean_line() {
        let long = "x".repeat(500);
        let text = format!("{{\"summary\": \"a\\u001b[31m{long}\"}}");
        let reading = ModReading::from_output(&text).unwrap();
        assert_eq!(reading.summary.chars().count(), MOST_CHARS);
        assert!(!reading.summary.contains('\u{1b}'));
    }

    #[test]
    fn a_badge_of_its_own_wins_and_an_ok_reading_has_none() {
        let mut reading = ModReading::sample(ModBody::Stack);
        reading.badge = Some("red build".to_owned());
        assert_eq!(reading.badge_words().as_deref(), Some("red build"));
        reading.badge = None;
        reading.level = ModLevel::Ok;
        assert_eq!(reading.badge_words(), None);
        reading.level = ModLevel::Warn;
        assert_eq!(reading.badge_words().as_deref(), Some("1 running"));
    }

    #[test]
    fn numbers_read_short() {
        for (value, words) in [
            (7.0, "7"),
            (0.5, "0.5"),
            (1234.0, "1.2k"),
            (840_000.0, "840k"),
            (3_400_000.0, "3.4M"),
            (1_000_000.0, "1M"),
            (12_000_000.0, "12M"),
        ] {
            assert_eq!(plain_number(value), words);
        }
    }

    #[test]
    fn a_built_in_source_s_sample_looks_like_the_source() {
        use crate::settings::mods::builtin;
        let cards = builtin::cards();
        let sample = |name: &str| {
            ModReading::sample_for(cards.iter().find(|card| card.name == name).unwrap())
        };
        assert_eq!(sample("context").summary, "32.3k of 1M · compacts at 967k");
        assert_eq!(sample("context").parts.len(), 8);
        assert_eq!(sample("plan-usage").parts[1].label, "Claude Code 7d");
        assert_eq!(
            sample("spend").parts[2].cli.as_deref(),
            Some("antigravity-cli")
        );
        assert_eq!(sample("spend").summary, "~$1.86 today");
        assert_eq!(sample("tokens").parts.len(), 3);
        // The rest draw their body's sample.
        assert_eq!(sample("runs"), ModReading::sample(ModBody::List));
    }

    #[test]
    fn every_body_has_sample_numbers() {
        for body in ModBody::ALL {
            let sample = ModReading::sample(body);
            assert!(!sample.summary.is_empty(), "{body}");
        }
    }
}
