//! The widgets: what each one is, the forms it draws, the places it can go, and its table under
//! `[ui.widgets]`.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::super::Span;
use super::look::{ColorText, LevelSettings, Threshold};

/// One thing Argonauts can show as numbers.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WidgetName {
    Context,
    Sessions,
    /// Plan usage: each CLI's plan windows.
    #[serde(rename = "plan-usage")]
    Quota,
    /// The run's spend, tokens and time against the limits you set.
    #[serde(rename = "run-limits")]
    Budget,
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

impl WidgetName {
    pub const ALL: [WidgetName; 17] = [
        WidgetName::Context,
        WidgetName::Sessions,
        WidgetName::Quota,
        WidgetName::Budget,
        WidgetName::Tasks,
        WidgetName::Limits,
        WidgetName::Spend,
        WidgetName::Tokens,
        WidgetName::History,
        WidgetName::Runs,
        WidgetName::Stats,
        WidgetName::Run,
        WidgetName::Waiting,
        WidgetName::Recent,
        WidgetName::Agent,
        WidgetName::Narrator,
        WidgetName::Receipt,
    ];

    pub fn name(self) -> &'static str {
        match self {
            WidgetName::Context => "context",
            WidgetName::Sessions => "sessions",
            WidgetName::Quota => "plan-usage",
            WidgetName::Budget => "run-limits",
            WidgetName::Tasks => "tasks",
            WidgetName::Limits => "limits",
            WidgetName::Spend => "spend",
            WidgetName::Tokens => "tokens",
            WidgetName::History => "history",
            WidgetName::Runs => "runs",
            WidgetName::Stats => "stats",
            WidgetName::Run => "run",
            WidgetName::Waiting => "waiting",
            WidgetName::Recent => "recent",
            WidgetName::Agent => "agent",
            WidgetName::Narrator => "narrator",
            WidgetName::Receipt => "receipt",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|widget| widget.name() == name)
    }

    /// What it shows, for /mods and the help.
    pub fn describe(self) -> &'static str {
        match self {
            WidgetName::Context => "one session's context by category",
            WidgetName::Sessions => "every live session's context",
            WidgetName::Quota => "each agent's plan windows",
            WidgetName::Budget => "the run's spend, tokens, and time against its limits",
            WidgetName::Tasks => "each task's spend against its limit",
            WidgetName::Limits => "the limit keys, to edit",
            WidgetName::Spend => "spend by agent, model, CLI, or run",
            WidgetName::Tokens => "cached input, new input, and output",
            WidgetName::History => "spend per day",
            WidgetName::Runs => "the period's runs",
            WidgetName::Stats => "what each CLI and model did, and all together",
            WidgetName::Run => "the attached run's tasks, goal, and plan",
            WidgetName::Waiting => "what waits on you",
            WidgetName::Recent => "the run's last events",
            WidgetName::Agent => "the main agent",
            WidgetName::Narrator => "the narrator's spend, while it runs",
            WidgetName::Receipt => "the run's task above a closing answer",
        }
    }

    /// The forms its card draws, its default first. Empty for a mod that's only a chip.
    pub fn forms(self) -> &'static [WidgetForm] {
        use WidgetForm::*;
        match self {
            WidgetName::Context
            | WidgetName::Quota
            | WidgetName::Budget
            | WidgetName::Spend
            | WidgetName::Tokens
            | WidgetName::Run => &[Full, Compact, Line],
            WidgetName::Sessions | WidgetName::Tasks | WidgetName::Limits | WidgetName::Runs => {
                &[List]
            }
            WidgetName::History => &[Columns, Line],
            WidgetName::Stats => &[Full, Line],
            WidgetName::Waiting => &[List, Line],
            WidgetName::Recent => &[Line],
            WidgetName::Receipt => &[Full],
            WidgetName::Agent | WidgetName::Narrator => &[],
        }
    }

    /// Whether it can show in `place`. The receipt shows only in the transcript, and the
    /// budget keys only on their own tab.
    pub fn fits(self, place: Place) -> bool {
        match (self, place) {
            (WidgetName::Receipt, _) => false,
            (WidgetName::Limits, place) => place == Place::Budget,
            _ => !self.forms().is_empty(),
        }
    }

    /// The units it can show first, its default first. Empty when it has no choice.
    pub fn units(self) -> &'static [Units] {
        use Units::*;
        match self {
            WidgetName::Context | WidgetName::Sessions => &[Percent, Tokens],
            WidgetName::Spend
            | WidgetName::History
            | WidgetName::Runs
            | WidgetName::Budget
            | WidgetName::Tasks
            | WidgetName::Narrator => &[Usd, Tokens],
            _ => &[],
        }
    }

    /// The parts its card can show, in drawing order. Empty while a mod's parts are its own.
    pub fn parts(self) -> &'static [&'static str] {
        match self {
            WidgetName::Context => &["amount", "tail", "badge", "bar", "tick", "legend"],
            WidgetName::Quota => &["meter", "percent", "resets", "plan"],
            _ => &[],
        }
    }

    /// Whether its levels can start at `limits.soft_stop`.
    pub fn counts_budget(self) -> bool {
        matches!(self, WidgetName::Budget | WidgetName::Tasks)
    }
}

impl fmt::Display for WidgetName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a built-in card shows: above the input, or on one of the Overview's tabs. The bottom
/// line reads its own mod file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    AboveInput,
    Status,
    Usage,
    Context,
    Budget,
}

impl Place {
    pub const ALL: [Place; 5] = [
        Place::AboveInput,
        Place::Status,
        Place::Usage,
        Place::Context,
        Place::Budget,
    ];

    /// The word `/mods` takes.
    pub fn word(self) -> &'static str {
        match self {
            Place::AboveInput => "above",
            Place::Status => "status",
            Place::Usage => "usage",
            Place::Context => "context",
            Place::Budget => "limits",
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
            Place::Budget => "ui.views.limits.mods",
        }
    }

    /// The words for a sentence, such as "above the input".
    pub fn phrase(self) -> &'static str {
        match self {
            Place::AboveInput => "above the input",
            Place::Status => "on the Status tab",
            Place::Usage => "on the Usage tab",
            Place::Context => "on the Context tab",
            Place::Budget => "on the Limits tab",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetForm {
    #[default]
    Full,
    Compact,
    Line,
    List,
    Columns,
}

impl WidgetForm {
    pub fn word(self) -> &'static str {
        match self {
            WidgetForm::Full => "full",
            WidgetForm::Compact => "compact",
            WidgetForm::Line => "line",
            WidgetForm::List => "list",
            WidgetForm::Columns => "columns",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        [
            WidgetForm::Full,
            WidgetForm::Compact,
            WidgetForm::Line,
            WidgetForm::List,
            WidgetForm::Columns,
        ]
        .into_iter()
        .find(|form| form.word() == word)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetWidth {
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
pub struct WidgetTable<Own: Default> {
    /// Open draws its card; false folds it to its line
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    /// The form its card draws: full, compact, or line, or list or columns for the mods that draw them
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form: Option<WidgetForm>,
    /// Full or half, where two half-width mods share a row on a wide screen
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<WidgetWidth>,
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
pub struct QuotaKeys {
    /// The agents it shows. Empty is the crew's, in crew order
    #[serde(rename = "agents")]
    pub vendors: Vec<String>,
    /// The windows it shows, such as ["5h", "7d"]. Empty is every window an agent reports
    pub windows: Vec<String>,
    /// How old a reading gets before it shows as stale, such as "10m"
    pub stale_after: Span,
}

impl Default for QuotaKeys {
    fn default() -> Self {
        Self {
            vendors: Vec::new(),
            windows: Vec::new(),
            stale_after: Span::new("10m"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BudgetKeys {
    /// Which run it follows: the attached one or the newest
    pub run: BudgetRun,
    /// The limits it measures
    pub limits: Vec<BudgetLimitKind>,
}

impl Default for BudgetKeys {
    fn default() -> Self {
        Self {
            run: BudgetRun::Attached,
            limits: vec![
                BudgetLimitKind::Spend,
                BudgetLimitKind::Tokens,
                BudgetLimitKind::Time,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BudgetRun {
    #[default]
    Attached,
    Newest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BudgetLimitKind {
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
    #[serde(rename = "provider")]
    Vendor,
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
pub struct WidgetsSettings {
    /// The form a mod's card draws unless its own table says: full, compact, or line
    pub form: WidgetForm,
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
    pub context: WidgetTable<ContextKeys>,
    /// Every live session's context
    pub sessions: WidgetTable<NoKeys>,
    /// Each CLI's plan usage windows
    #[serde(rename = "plan-usage")]
    pub quota: WidgetTable<QuotaKeys>,
    /// The run's spend, tokens, and time against its limits
    #[serde(rename = "run-limits")]
    pub budget: WidgetTable<BudgetKeys>,
    /// Each task's spend against its limit
    pub tasks: WidgetTable<NoKeys>,
    /// The budget keys, to edit
    pub limits: WidgetTable<NoKeys>,
    /// Spend by agent, model, provider, or run
    pub spend: WidgetTable<SpendKeys>,
    /// Cached input, new input, and output
    pub tokens: WidgetTable<NoKeys>,
    /// Spend per day
    pub history: WidgetTable<HistoryKeys>,
    /// The period's runs
    pub runs: WidgetTable<RunsKeys>,
    /// What each CLI and model did, and all together. Its own keys live in `mods/stats.toml`.
    pub stats: WidgetTable<NoKeys>,
    /// The attached run's cards, goal, and plan
    pub run: WidgetTable<NoKeys>,
    /// What waits on you
    pub waiting: WidgetTable<NoKeys>,
    /// The run's last events
    pub recent: WidgetTable<NoKeys>,
    /// The main agent, on the bottom line
    pub agent: WidgetTable<NoKeys>,
    /// The narrator's spend while it runs, on the bottom line
    pub narrator: WidgetTable<NoKeys>,
    /// The run's card above a closing answer
    pub receipt: WidgetTable<ReceiptKeys>,
}

impl Default for WidgetsSettings {
    fn default() -> Self {
        let levels = |warn, danger| WidgetTable::<NoKeys> {
            warn: Some(warn),
            danger: Some(danger),
            ..WidgetTable::default()
        };
        let parts = |name: WidgetName| Some(name.parts().iter().map(|&p| p.to_owned()).collect());
        Self {
            form: WidgetForm::Full,
            border: Border::Round,
            legend_columns: 3,
            legend_marker: LegendMarker::Bar,
            badge: true,
            glyph: "◆".to_owned(),
            context: WidgetTable {
                open: Some(true),
                width: Some(WidgetWidth::Full),
                parts: parts(WidgetName::Context),
                units: Some(Units::Percent),
                warn: Some(Threshold::percent(80.0)),
                danger: Some(Threshold::percent(95.0)),
                glyph_color: Some(ColorText::from("accent")),
                ..WidgetTable::default()
            },
            sessions: levels(Threshold::percent(80.0), Threshold::percent(95.0)),
            quota: WidgetTable {
                parts: parts(WidgetName::Quota),
                warn: Some(Threshold::percent(90.0)),
                danger: Some(Threshold::percent(100.0)),
                ..WidgetTable::default()
            },
            budget: WidgetTable {
                warn: Some(Threshold::SoftStop),
                danger: Some(Threshold::percent(100.0)),
                ..WidgetTable::default()
            },
            tasks: levels(Threshold::SoftStop, Threshold::percent(100.0)),
            limits: WidgetTable::default(),
            spend: WidgetTable {
                units: Some(Units::Usd),
                ..WidgetTable::default()
            },
            tokens: WidgetTable::default(),
            history: WidgetTable {
                units: Some(Units::Usd),
                ..WidgetTable::default()
            },
            runs: WidgetTable::default(),
            stats: WidgetTable::default(),
            run: WidgetTable::default(),
            waiting: WidgetTable::default(),
            recent: WidgetTable::default(),
            agent: WidgetTable::default(),
            narrator: WidgetTable::default(),
            receipt: WidgetTable::default(),
        }
    }
}

/// One mod's settings with every fallback applied, as the screen draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetLook {
    pub open: bool,
    pub form: WidgetForm,
    pub width: WidgetWidth,
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
    pub form: Option<WidgetForm>,
    pub width: Option<WidgetWidth>,
    pub parts: Option<&'a [String]>,
    pub units: Option<Units>,
    pub warn: Option<Threshold>,
    pub danger: Option<Threshold>,
    pub colors: &'a BTreeMap<String, ColorText>,
    pub glyph: Option<&'a str>,
    pub glyph_color: Option<&'a ColorText>,
}

impl<Own: Default> WidgetTable<Own> {
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

impl WidgetsSettings {
    pub(crate) fn shared(&self, name: WidgetName) -> Shared<'_> {
        match name {
            WidgetName::Context => self.context.shared(),
            WidgetName::Sessions => self.sessions.shared(),
            WidgetName::Quota => self.quota.shared(),
            WidgetName::Budget => self.budget.shared(),
            WidgetName::Tasks => self.tasks.shared(),
            WidgetName::Limits => self.limits.shared(),
            WidgetName::Spend => self.spend.shared(),
            WidgetName::Tokens => self.tokens.shared(),
            WidgetName::History => self.history.shared(),
            WidgetName::Runs => self.runs.shared(),
            WidgetName::Stats => self.stats.shared(),
            WidgetName::Run => self.run.shared(),
            WidgetName::Waiting => self.waiting.shared(),
            WidgetName::Recent => self.recent.shared(),
            WidgetName::Agent => self.agent.shared(),
            WidgetName::Narrator => self.narrator.shared(),
            WidgetName::Receipt => self.receipt.shared(),
        }
    }

    pub(crate) fn clear(&mut self, name: WidgetName, key: SharedKey) {
        match name {
            WidgetName::Context => self.context.clear(key),
            WidgetName::Sessions => self.sessions.clear(key),
            WidgetName::Quota => self.quota.clear(key),
            WidgetName::Budget => self.budget.clear(key),
            WidgetName::Tasks => self.tasks.clear(key),
            WidgetName::Limits => self.limits.clear(key),
            WidgetName::Spend => self.spend.clear(key),
            WidgetName::Tokens => self.tokens.clear(key),
            WidgetName::History => self.history.clear(key),
            WidgetName::Runs => self.runs.clear(key),
            WidgetName::Stats => self.stats.clear(key),
            WidgetName::Run => self.run.clear(key),
            WidgetName::Waiting => self.waiting.clear(key),
            WidgetName::Recent => self.recent.clear(key),
            WidgetName::Agent => self.agent.clear(key),
            WidgetName::Narrator => self.narrator.clear(key),
            WidgetName::Receipt => self.receipt.clear(key),
        }
    }

    /// `name`'s settings with its table over `[ui.mods]`, and `levels` under both.
    pub fn look(&self, name: WidgetName, levels: &LevelSettings) -> WidgetLook {
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
        WidgetLook {
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
        for widget in WidgetName::ALL {
            assert_eq!(WidgetName::from_name(widget.name()), Some(widget));
            let json = serde_json::to_value(widget).unwrap();
            assert_eq!(json, serde_json::json!(widget.name()));
        }
        for place in Place::ALL {
            assert_eq!(Place::from_word(place.word()), Some(place));
        }
        assert_eq!(
            Place::from_word("mode-line"),
            None,
            "the bottom line has a file of its own"
        );
        assert_eq!(WidgetName::from_name("nope"), None);
    }

    #[test]
    fn each_widget_goes_only_where_it_can_draw() {
        use WidgetName::*;
        assert!(Context.fits(Place::AboveInput) && Context.fits(Place::Context));
        assert!(!Agent.fits(Place::AboveInput));
        assert!(!Receipt.fits(Place::AboveInput) && !Receipt.fits(Place::Status));
        assert!(Limits.fits(Place::Budget) && !Limits.fits(Place::AboveInput));
        assert!(Sessions.fits(Place::Usage));
        // The receipt draws in the transcript, and the agent and narrator chips only on the
        // bottom line, which reads its own file.
        for widget in WidgetName::ALL {
            let somewhere = Place::ALL.into_iter().any(|place| widget.fits(place));
            let nowhere = matches!(widget, Receipt | Agent | Narrator);
            assert_eq!(somewhere, !nowhere, "{widget}");
        }
    }

    #[test]
    fn a_widget_falls_back_to_the_shared_table_then_the_levels() {
        let levels = LevelSettings::default();
        let mut widgets = WidgetsSettings::default();
        let context = widgets.look(WidgetName::Context, &levels);
        assert_eq!(context.warn, Threshold::percent(80.0));
        assert_eq!(context.units, Some(Units::Percent));
        assert_eq!(context.parts.len(), 6);
        assert_eq!(context.glyph, "◆");

        let run = widgets.look(WidgetName::Run, &levels);
        assert_eq!(run.warn, Threshold::Percent(levels.warn));
        assert_eq!(run.form, WidgetForm::Full);
        assert!(run.open);

        // A shared form a widget can't draw leaves it on its own default.
        widgets.form = WidgetForm::Compact;
        assert_eq!(
            widgets.look(WidgetName::Sessions, &levels).form,
            WidgetForm::List
        );
        assert_eq!(
            widgets.look(WidgetName::Quota, &levels).form,
            WidgetForm::Compact
        );
        widgets.history.form = Some(WidgetForm::Line);
        assert_eq!(
            widgets.look(WidgetName::History, &levels).form,
            WidgetForm::Line
        );
    }
}
