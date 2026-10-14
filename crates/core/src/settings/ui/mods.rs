//! The mods: what each one is, the forms it draws, the places it can go, and its table under
//! `[ui.mods]`.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::super::Span;
use super::look::{ColorText, LevelSettings, Threshold};

/// One thing Argonauts Code can show as numbers.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ModName {
    Context,
    Sessions,
    /// Plan usage: each CLI's plan windows.
    #[serde(rename = "plan-usage")]
    PlanUsage,
    /// The run's spend, tokens and time against the limits you set.
    #[serde(rename = "run-limits")]
    RunLimits,
    Tasks,
    Limits,
    Spend,
    Tokens,
    History,
    Runs,
    Stats,
    Run,
    Waiting,
    Recent,
    Agent,
    Narrator,
    Receipt,
}

impl ModName {
    pub const ALL: [ModName; 17] = [
        ModName::Context,
        ModName::Sessions,
        ModName::PlanUsage,
        ModName::RunLimits,
        ModName::Tasks,
        ModName::Limits,
        ModName::Spend,
        ModName::Tokens,
        ModName::History,
        ModName::Runs,
        ModName::Stats,
        ModName::Run,
        ModName::Waiting,
        ModName::Recent,
        ModName::Agent,
        ModName::Narrator,
        ModName::Receipt,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ModName::Context => "context",
            ModName::Sessions => "sessions",
            ModName::PlanUsage => "plan-usage",
            ModName::RunLimits => "run-limits",
            ModName::Tasks => "tasks",
            ModName::Limits => "limits",
            ModName::Spend => "spend",
            ModName::Tokens => "tokens",
            ModName::History => "history",
            ModName::Runs => "runs",
            ModName::Stats => "stats",
            ModName::Run => "run",
            ModName::Waiting => "waiting",
            ModName::Recent => "recent",
            ModName::Agent => "agent",
            ModName::Narrator => "narrator",
            ModName::Receipt => "receipt",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|the_mod| the_mod.name() == name)
    }

    /// What it shows, for /mods and the help.
    pub fn describe(self) -> &'static str {
        match self {
            ModName::Context => "one session's context by category",
            ModName::Sessions => "every live session's context",
            ModName::PlanUsage => "each agent's plan windows",
            ModName::RunLimits => "the run's spend, tokens, and time against its limits",
            ModName::Tasks => "each task's spend against its limit",
            ModName::Limits => "the limit keys, to edit",
            ModName::Spend => "spend by agent, model, CLI, or run",
            ModName::Tokens => "cached input, new input, and output",
            ModName::History => "spend per day",
            ModName::Runs => "the period's runs",
            ModName::Stats => "what each CLI and model did, and all together",
            ModName::Run => "the attached run's tasks, goal, and plan",
            ModName::Waiting => "what waits on you",
            ModName::Recent => "the run's last events",
            ModName::Agent => "the main agent",
            ModName::Narrator => "the narrator's spend, while it runs",
            ModName::Receipt => "the run's card above a closing answer",
        }
    }

    /// The forms its card draws, its default first. Empty for a mod that's only a chip.
    pub fn forms(self) -> &'static [ModShape] {
        use ModShape::*;
        match self {
            ModName::Context
            | ModName::PlanUsage
            | ModName::RunLimits
            | ModName::Spend
            | ModName::Tokens
            | ModName::Run => &[Full, Compact, Line],
            ModName::Sessions | ModName::Tasks | ModName::Limits | ModName::Runs => &[List],
            ModName::History => &[Columns, Line],
            ModName::Stats => &[Full, Line],
            ModName::Waiting => &[List, Line],
            ModName::Recent => &[Line],
            ModName::Receipt => &[Full],
            ModName::Agent | ModName::Narrator => &[],
        }
    }

    /// Whether it can show in `place`. The receipt shows only in the transcript, and the
    /// limit keys only on their own tab.
    pub fn fits(self, place: Place) -> bool {
        match (self, place) {
            (ModName::Receipt, _) => false,
            (ModName::Limits, place) => place == Place::Limits,
            _ => !self.forms().is_empty(),
        }
    }

    /// The units it can show first, its default first. Empty when it has no choice.
    pub fn units(self) -> &'static [Units] {
        use Units::*;
        match self {
            ModName::Context | ModName::Sessions => &[Percent, Tokens],
            ModName::Spend
            | ModName::History
            | ModName::Runs
            | ModName::RunLimits
            | ModName::Tasks
            | ModName::Narrator => &[Usd, Tokens],
            _ => &[],
        }
    }

    /// The parts its card can show, in drawing order. Empty while a mod's parts are its own.
    pub fn parts(self) -> &'static [&'static str] {
        match self {
            ModName::Context => &["amount", "tail", "badge", "bar", "tick", "legend"],
            ModName::PlanUsage => &["meter", "percent", "resets", "plan"],
            _ => &[],
        }
    }

    /// Whether its levels can start at `limits.soft_stop`.
    pub fn counts_limit(self) -> bool {
        matches!(self, ModName::RunLimits | ModName::Tasks)
    }
}

impl fmt::Display for ModName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a built-in card shows: above the input, or on one of the usage panel's tabs. The bottom
/// line reads its own mod file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    AboveInput,
    Status,
    Usage,
    Context,
    Limits,
}

impl Place {
    pub const ALL: [Place; 5] = [
        Place::AboveInput,
        Place::Status,
        Place::Usage,
        Place::Context,
        Place::Limits,
    ];

    /// The word `/mods` takes.
    pub fn word(self) -> &'static str {
        match self {
            Place::AboveInput => "above",
            Place::Status => "status",
            Place::Usage => "usage",
            Place::Context => "context",
            Place::Limits => "limits",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        let word = word.trim().to_ascii_lowercase().replace('_', "-");
        Self::ALL.into_iter().find(|place| place.word() == word)
    }

    /// The settings list that records which mods show here, in order.
    pub fn key(self) -> &'static str {
        match self {
            Place::AboveInput => "ui.above_input.mods",
            Place::Status => "ui.views.status.mods",
            Place::Usage => "ui.views.usage.mods",
            Place::Context => "ui.views.context.mods",
            Place::Limits => "ui.views.limits.mods",
        }
    }

    /// The words for a sentence, such as "above the input".
    pub fn phrase(self) -> &'static str {
        match self {
            Place::AboveInput => "above the input",
            Place::Status => "on the Status tab",
            Place::Usage => "on the Usage tab",
            Place::Context => "on the Context tab",
            Place::Limits => "on the Limits tab",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModShape {
    #[default]
    Full,
    Compact,
    Line,
    List,
    Columns,
}

impl ModShape {
    pub fn word(self) -> &'static str {
        match self {
            ModShape::Full => "full",
            ModShape::Compact => "compact",
            ModShape::Line => "line",
            ModShape::List => "list",
            ModShape::Columns => "columns",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        [
            ModShape::Full,
            ModShape::Compact,
            ModShape::Line,
            ModShape::List,
            ModShape::Columns,
        ]
        .into_iter()
        .find(|form| form.word() == word)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModWidth {
    #[default]
    Full,
    Half,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Units {
    Percent,
    Tokens,
    Usd,
}

impl Units {
    pub fn word(self) -> &'static str {
        match self {
            Units::Percent => "percent",
            Units::Tokens => "tokens",
            Units::Usd => "usd",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        [Units::Percent, Units::Tokens, Units::Usd]
            .into_iter()
            .find(|units| units.word() == word)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Border {
    #[default]
    Round,
    Rule,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub enum LegendMarker {
    #[default]
    #[serde(rename = "▌")]
    Bar,
    #[serde(rename = "■")]
    Square,
    #[serde(rename = "●")]
    Dot,
}

/// One mod's table. Every key it leaves unset falls back to `[ui.mods]` and
/// `[ui.levels]`, and `own` holds the keys only this mod takes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ModTable<Own: Default> {
    /// Open draws its card; false folds it to its line
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    /// The form its card draws: full, compact, or line, or list or columns for the mods that draw
    /// them
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form: Option<ModShape>,
    /// Full or half, where two half-width mods share a row on a wide screen
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<ModWidth>,
    /// The parts its card shows, in order
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<String>>,
    /// What its chip and rows show first: percent, tokens, or usd
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<Units>,
    /// Where it turns amber, such as "80%"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<Threshold>,
    /// Where it turns red, such as "95%"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub danger: Option<Threshold>,
    /// A role or a color for each of its parts or categories
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<String, ColorText>,
    /// The glyph before its title
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    /// The role or color of the glyph before its title
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph_color: Option<ColorText>,
    #[serde(flatten)]
    pub own: Own,
}

/// For the mods with no keys of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct NoKeys {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ContextKeys {
    /// Whose context it shows: the main agent's, the focused agent's, or the fullest
    pub session: ContextSession,
    /// The categories its bar and legend show, in order
    pub categories: Vec<ContextCategory>,
    /// Let an empty category leave the legend
    pub hide_empty: bool,
    /// How Claude Code is asked: summary costs no extra calls, full counts exactly
    pub detail: ContextDetail,
}

impl Default for ContextKeys {
    fn default() -> Self {
        Self {
            session: ContextSession::Main,
            categories: ContextCategory::ALL.to_vec(),
            hide_empty: false,
            detail: ContextDetail::Summary,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextSession {
    #[default]
    Main,
    Focused,
    Fullest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextCategory {
    System,
    Tools,
    Mcp,
    Agents,
    Memory,
    Skills,
    Messages,
    Free,
}

impl ContextCategory {
    pub const ALL: [ContextCategory; 8] = [
        ContextCategory::System,
        ContextCategory::Tools,
        ContextCategory::Mcp,
        ContextCategory::Agents,
        ContextCategory::Memory,
        ContextCategory::Skills,
        ContextCategory::Messages,
        ContextCategory::Free,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextDetail {
    #[default]
    Summary,
    Full,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PlanUsageKeys {
    /// The agents it shows. Empty is the crew's, in crew order
    pub agents: Vec<String>,
    /// The windows it shows, such as ["5h", "7d"]. Empty is every window an agent reports
    pub windows: Vec<String>,
    /// How old a reading gets before it shows as stale, such as "10m"
    pub stale_after: Span,
}

impl Default for PlanUsageKeys {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            windows: Vec::new(),
            stale_after: Span::new("10m"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunLimitsKeys {
    /// Which run it follows: the attached one or the newest
    pub run: RunLimitsRun,
    /// The limits it measures
    pub limits: Vec<RunLimitKind>,
}

impl Default for RunLimitsKeys {
    fn default() -> Self {
        Self {
            run: RunLimitsRun::Attached,
            limits: vec![
                RunLimitKind::Spend,
                RunLimitKind::Tokens,
                RunLimitKind::Time,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunLimitsRun {
    #[default]
    Attached,
    Newest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunLimitKind {
    Spend,
    Tokens,
    Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SpendKeys {
    /// What it groups spend by: agent, model, provider, or run
    pub by: SpendBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpendBy {
    #[default]
    Agent,
    Model,
    Provider,
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HistoryKeys {
    /// How many days it shows
    pub days: u16,
}

impl Default for HistoryKeys {
    fn default() -> Self {
        Self { days: 14 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunsKeys {
    /// The most runs it lists
    pub max_rows: u16,
}

impl Default for RunsKeys {
    fn default() -> Self {
        Self { max_rows: 5 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ReceiptKeys {
    /// Show the run's card above a closing answer
    pub show: bool,
}

impl Default for ReceiptKeys {
    fn default() -> Self {
        Self { show: true }
    }
}

/// What every mod inherits, and each mod's own table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UiModsSettings {
    /// The form a mod's card draws unless its own table says: full, compact, or line
    pub form: ModShape,
    /// A mod's frame: round, rule (a top rule, as Claude Code's dialogs), or none
    pub border: Border,
    /// The most legend entries in a row; fewer when they don't fit
    pub legend_columns: u8,
    /// The mark before each legend entry
    pub legend_marker: LegendMarker,
    /// Show a mod's percent as a colored badge
    pub badge: bool,
    /// The glyph before a mod's title
    pub glyph: String,
    /// One session's context by category
    pub context: ModTable<ContextKeys>,
    /// Every live session's context
    pub sessions: ModTable<NoKeys>,
    /// Each CLI's plan usage windows
    #[serde(rename = "plan-usage")]
    pub plan_usage: ModTable<PlanUsageKeys>,
    /// The run's spend, tokens, and time against its limits
    #[serde(rename = "run-limits")]
    pub run_limits: ModTable<RunLimitsKeys>,
    /// Each task's spend against its limit
    pub tasks: ModTable<NoKeys>,
    /// The limit keys, to edit
    pub limits: ModTable<NoKeys>,
    /// Spend by agent, model, provider, or run
    pub spend: ModTable<SpendKeys>,
    /// Cached input, new input, and output
    pub tokens: ModTable<NoKeys>,
    /// Spend per day
    pub history: ModTable<HistoryKeys>,
    /// The period's runs
    pub runs: ModTable<RunsKeys>,
    /// What each CLI and model did, and all together. Its own keys live in `mods/stats.toml`.
    pub stats: ModTable<NoKeys>,
    /// The attached run's tasks, goal, and plan
    pub run: ModTable<NoKeys>,
    /// What waits on you
    pub waiting: ModTable<NoKeys>,
    /// The run's last events
    pub recent: ModTable<NoKeys>,
    /// The main agent, on the bottom line
    pub agent: ModTable<NoKeys>,
    /// The narrator's spend while it runs, on the bottom line
    pub narrator: ModTable<NoKeys>,
    /// The run's card above a closing answer
    pub receipt: ModTable<ReceiptKeys>,
}

impl Default for UiModsSettings {
    fn default() -> Self {
        let levels = |warn, danger| ModTable::<NoKeys> {
            warn: Some(warn),
            danger: Some(danger),
            ..ModTable::default()
        };
        let parts = |name: ModName| Some(name.parts().iter().map(|&p| p.to_owned()).collect());
        Self {
            form: ModShape::Full,
            border: Border::Round,
            legend_columns: 3,
            legend_marker: LegendMarker::Bar,
            badge: true,
            glyph: "◆".to_owned(),
            context: ModTable {
                open: Some(true),
                width: Some(ModWidth::Full),
                parts: parts(ModName::Context),
                units: Some(Units::Percent),
                warn: Some(Threshold::percent(80.0)),
                danger: Some(Threshold::percent(95.0)),
                glyph_color: Some(ColorText::from("accent")),
                ..ModTable::default()
            },
            sessions: levels(Threshold::percent(80.0), Threshold::percent(95.0)),
            plan_usage: ModTable {
                parts: parts(ModName::PlanUsage),
                warn: Some(Threshold::percent(90.0)),
                danger: Some(Threshold::percent(100.0)),
                ..ModTable::default()
            },
            run_limits: ModTable {
                warn: Some(Threshold::SoftStop),
                danger: Some(Threshold::percent(100.0)),
                ..ModTable::default()
            },
            tasks: levels(Threshold::SoftStop, Threshold::percent(100.0)),
            limits: ModTable::default(),
            spend: ModTable {
                units: Some(Units::Usd),
                ..ModTable::default()
            },
            tokens: ModTable::default(),
            history: ModTable {
                units: Some(Units::Usd),
                ..ModTable::default()
            },
            runs: ModTable::default(),
            stats: ModTable::default(),
            run: ModTable::default(),
            waiting: ModTable::default(),
            recent: ModTable::default(),
            agent: ModTable::default(),
            narrator: ModTable::default(),
            receipt: ModTable::default(),
        }
    }
}

/// One mod's settings with every fallback applied, as the screen draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModLook {
    pub open: bool,
    pub form: ModShape,
    pub width: ModWidth,
    pub parts: Vec<String>,
    /// `None` for a mod with no choice of units.
    pub units: Option<Units>,
    pub warn: Threshold,
    pub danger: Threshold,
    pub colors: BTreeMap<String, ColorText>,
    pub glyph: String,
    pub glyph_color: Option<ColorText>,
    pub border: Border,
    pub legend_columns: u8,
    pub legend_marker: LegendMarker,
    pub badge: bool,
}

/// The keys every mod table shares, borrowed whatever its own keys are.
pub(crate) struct Shared<'a> {
    pub open: Option<bool>,
    pub form: Option<ModShape>,
    pub width: Option<ModWidth>,
    pub parts: Option<&'a [String]>,
    pub units: Option<Units>,
    pub warn: Option<Threshold>,
    pub danger: Option<Threshold>,
    pub colors: &'a BTreeMap<String, ColorText>,
    pub glyph: Option<&'a str>,
    pub glyph_color: Option<&'a ColorText>,
}

impl<Own: Default> ModTable<Own> {
    pub(crate) fn shared(&self) -> Shared<'_> {
        Shared {
            open: self.open,
            form: self.form,
            width: self.width,
            parts: self.parts.as_deref(),
            units: self.units,
            warn: self.warn,
            danger: self.danger,
            colors: &self.colors,
            glyph: self.glyph.as_deref(),
            glyph_color: self.glyph_color.as_ref(),
        }
    }

    /// Clears the keys a check found wrong, so they fall back.
    pub(crate) fn clear(&mut self, key: SharedKey) {
        match key {
            SharedKey::Form => self.form = None,
            SharedKey::Units => self.units = None,
            SharedKey::Warn => self.warn = None,
            SharedKey::Danger => self.danger = None,
            SharedKey::Parts(keep) => self.parts = Some(keep),
        }
    }
}

/// A shared key that a check can put back.
pub(crate) enum SharedKey {
    Form,
    Units,
    Warn,
    Danger,
    /// The parts to keep.
    Parts(Vec<String>),
}

impl UiModsSettings {
    pub(crate) fn shared(&self, name: ModName) -> Shared<'_> {
        match name {
            ModName::Context => self.context.shared(),
            ModName::Sessions => self.sessions.shared(),
            ModName::PlanUsage => self.plan_usage.shared(),
            ModName::RunLimits => self.run_limits.shared(),
            ModName::Tasks => self.tasks.shared(),
            ModName::Limits => self.limits.shared(),
            ModName::Spend => self.spend.shared(),
            ModName::Tokens => self.tokens.shared(),
            ModName::History => self.history.shared(),
            ModName::Runs => self.runs.shared(),
            ModName::Stats => self.stats.shared(),
            ModName::Run => self.run.shared(),
            ModName::Waiting => self.waiting.shared(),
            ModName::Recent => self.recent.shared(),
            ModName::Agent => self.agent.shared(),
            ModName::Narrator => self.narrator.shared(),
            ModName::Receipt => self.receipt.shared(),
        }
    }

    pub(crate) fn clear(&mut self, name: ModName, key: SharedKey) {
        match name {
            ModName::Context => self.context.clear(key),
            ModName::Sessions => self.sessions.clear(key),
            ModName::PlanUsage => self.plan_usage.clear(key),
            ModName::RunLimits => self.run_limits.clear(key),
            ModName::Tasks => self.tasks.clear(key),
            ModName::Limits => self.limits.clear(key),
            ModName::Spend => self.spend.clear(key),
            ModName::Tokens => self.tokens.clear(key),
            ModName::History => self.history.clear(key),
            ModName::Runs => self.runs.clear(key),
            ModName::Stats => self.stats.clear(key),
            ModName::Run => self.run.clear(key),
            ModName::Waiting => self.waiting.clear(key),
            ModName::Recent => self.recent.clear(key),
            ModName::Agent => self.agent.clear(key),
            ModName::Narrator => self.narrator.clear(key),
            ModName::Receipt => self.receipt.clear(key),
        }
    }

    /// `name`'s settings with its table over `[ui.mods]`, and `levels` under both.
    pub fn look(&self, name: ModName, levels: &LevelSettings) -> ModLook {
        let own = self.shared(name);
        let forms = name.forms();
        let form = [own.form, Some(self.form)]
            .into_iter()
            .flatten()
            .find(|form| forms.contains(form))
            .or_else(|| forms.first().copied())
            .unwrap_or_default();
        let parts = own.parts.map_or_else(
            || name.parts().iter().map(|&part| part.to_owned()).collect(),
            <[String]>::to_vec,
        );
        ModLook {
            open: own.open.unwrap_or(true),
            form,
            width: own.width.unwrap_or_default(),
            parts,
            units: own.units.or_else(|| name.units().first().copied()),
            warn: own.warn.unwrap_or(Threshold::Percent(levels.warn)),
            danger: own.danger.unwrap_or(Threshold::Percent(levels.danger)),
            colors: own.colors.clone(),
            glyph: own.glyph.unwrap_or(&self.glyph).to_owned(),
            glyph_color: own.glyph_color.cloned(),
            border: self.border,
            legend_columns: self.legend_columns,
            legend_marker: self.legend_marker,
            badge: self.badge,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_words_read_back() {
        for the_mod in ModName::ALL {
            assert_eq!(ModName::from_name(the_mod.name()), Some(the_mod));
            let json = serde_json::to_value(the_mod).unwrap();
            assert_eq!(json, serde_json::json!(the_mod.name()));
        }
        for place in Place::ALL {
            assert_eq!(Place::from_word(place.word()), Some(place));
        }
        assert_eq!(
            Place::from_word("bottom-line"),
            None,
            "the bottom line has a file of its own"
        );
        assert_eq!(ModName::from_name("nope"), None);
    }

    #[test]
    fn each_mod_goes_only_where_it_can_draw() {
        use ModName::*;
        assert!(Context.fits(Place::AboveInput) && Context.fits(Place::Context));
        assert!(!Agent.fits(Place::AboveInput));
        assert!(!Receipt.fits(Place::AboveInput) && !Receipt.fits(Place::Status));
        assert!(Limits.fits(Place::Limits) && !Limits.fits(Place::AboveInput));
        assert!(Sessions.fits(Place::Usage));
        // The receipt draws in the transcript, and the agent and narrator chips only on the
        // bottom line, which reads its own file.
        for the_mod in ModName::ALL {
            let somewhere = Place::ALL.into_iter().any(|place| the_mod.fits(place));
            let nowhere = matches!(the_mod, Receipt | Agent | Narrator);
            assert_eq!(somewhere, !nowhere, "{the_mod}");
        }
    }

    #[test]
    fn a_mod_falls_back_to_the_shared_table_then_the_levels() {
        let levels = LevelSettings::default();
        let mut mods = UiModsSettings::default();
        let context = mods.look(ModName::Context, &levels);
        assert_eq!(context.warn, Threshold::percent(80.0));
        assert_eq!(context.units, Some(Units::Percent));
        assert_eq!(context.parts.len(), 6);
        assert_eq!(context.glyph, "◆");

        let run = mods.look(ModName::Run, &levels);
        assert_eq!(run.warn, Threshold::Percent(levels.warn));
        assert_eq!(run.form, ModShape::Full);
        assert!(run.open);

        // A shared form a mod can't draw leaves it on its own default.
        mods.form = ModShape::Compact;
        assert_eq!(mods.look(ModName::Sessions, &levels).form, ModShape::List);
        assert_eq!(
            mods.look(ModName::PlanUsage, &levels).form,
            ModShape::Compact
        );
        mods.history.form = Some(ModShape::Line);
        assert_eq!(mods.look(ModName::History, &levels).form, ModShape::Line);
    }
}
