//! What the Behaviors map draws, as the app sends it to the screen: every action your agents
//! took as a dot at its place, the groups they fall in, and for each group the density
//! difference against the rest and its distinctive behaviors. The screen lights, rings and
//! opens these without asking again; a new grouping, period or scope asks.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::overview::OverviewPeriod;

/// The density grid's side: the difference is worked out at `GRID` by `GRID` places.
pub const GRID: usize = 60;

/// What the dots are grouped and coloured by, in the order `g` cycles them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grouping {
    #[default]
    Model,
    Cli,
    Role,
    Workflow,
    Project,
    /// Whether the action's task merged, was sent back, or was dropped.
    Outcome,
}

impl Grouping {
    pub const ALL: [Grouping; 6] = [
        Grouping::Model,
        Grouping::Cli,
        Grouping::Role,
        Grouping::Workflow,
        Grouping::Project,
        Grouping::Outcome,
    ];

    /// As the line under the tabs says it: `by model`.
    pub fn word(self) -> &'static str {
        match self {
            Grouping::Model => "model",
            Grouping::Cli => "CLI",
            Grouping::Role => "role",
            Grouping::Workflow => "workflow",
            Grouping::Project => "project",
            Grouping::Outcome => "outcome",
        }
    }

    /// The groups together, as the legend says what one is compared with: `the other models`.
    pub fn plural(self) -> &'static str {
        match self {
            Grouping::Model => "models",
            Grouping::Cli => "CLIs",
            Grouping::Role => "roles",
            Grouping::Workflow => "workflows",
            Grouping::Project => "projects",
            Grouping::Outcome => "outcomes",
        }
    }

    /// The next one `by` along, wrapping.
    pub fn step(self, by: isize) -> Self {
        let at = Self::ALL.iter().position(|g| *g == self).unwrap_or(0) as isize;
        Self::ALL[(at + by).rem_euclid(Self::ALL.len() as isize) as usize]
    }
}

/// Which actions the map reads: every project's, or the one Argonauts runs in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Everywhere,
    ThisProject,
}

/// What the screen asks the map for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MapRequest {
    pub grouping: Grouping,
    pub period: OverviewPeriod,
    pub scope: Scope,
    /// The label map or the deeper one, as `d` switched; `behaviors.map` says when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<crate::settings::behaviors::BehaviorMap>,
}

/// Where the dots' places came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Placing {
    /// Each label's fixed place on the map.
    #[default]
    Labels,
    /// Each action's summary, embedded and projected.
    Summaries {
        /// Who summarized them, as the line under the tabs names it: `Qwen3 Coder 30B on Ollama`.
        by: String,
    },
}

/// The map for one request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BehaviorMap {
    pub request: MapRequest,
    pub placing: Placing,
    /// Runs the actions came from.
    pub runs: usize,
    /// Conversations whose main agent acted on its own.
    pub conversations: usize,
    /// The projects the actions came from, by name.
    pub projects: Vec<String>,
    /// The project Argonauts runs in, by name, for the scope of this one project.
    pub here: Option<String>,
    pub groups: Vec<MapGroup>,
    pub actions: Vec<MapAction>,
    /// One for each group, in the groups' order.
    pub comparisons: Vec<Comparison>,
    /// Actions no label fits, which the map leaves out.
    pub unlabelled: usize,
    /// No deeper map has been made yet, so the tab offers one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub offer_deeper: bool,
    /// On the deeper map: actions it has no summary of yet, which `d` offers to add.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unsummarized: usize,
    /// On the deeper map: the summaries were placed by their words, with no embedding model.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub by_words: bool,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

/// One group of dots: a model, a CLI, a role, a workflow, a project or an outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapGroup {
    pub name: String,
    /// The CLI whose colour the group draws in, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The model's id, for its shade of the CLI's colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub actions: usize,
}

/// One action as a dot, with whose it was and what it did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapAction {
    /// Its place on the map, both in 0..1, `y` up.
    pub x: f32,
    pub y: f32,
    /// Its group's index.
    pub group: usize,
    /// The behavior it counts under: its label's words on the label map, its summary on the
    /// deeper one.
    pub behavior: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The agent as the run names it, such as `build:per-api-key`.
    pub agent: String,
    /// Display names.
    pub model: String,
    pub cli: String,
    pub role: String,
    pub workflow: String,
    pub project: String,
    /// The run's id, or `None` for the main agent's own turn in a conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Its place among its session's actions, from 1, and how many there were.
    pub nth: usize,
    pub of: usize,
    /// The tool as the CLI names it, and what it was given, such as a command.
    pub tool: String,
    pub input: String,
    /// Where it acted, such as `in the run's worktree`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The run's own words about it, such as the review's note on its task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_shows: Option<String>,
}

/// One group against the rest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    /// The group's density minus the mean of the others', on the grid, row by row from
    /// `y = 0`, scaled so the largest is 1 across. `None` with `why_not` when it can't be drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_not: Option<String>,
    /// Its distinctive behaviors, most distinctive first.
    pub rows: Vec<BehaviorRow>,
    /// Its actions, and the other groups' together.
    pub mine: usize,
    pub others: usize,
}

/// A behavior one group shows more than the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorRow {
    pub behavior: String,
    /// The label of the row's first action, where the behavior is a summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Its actions in the group, and in the others.
    pub mine: usize,
    pub others: usize,
    /// The actions the row lights on the map, by index.
    pub lit: Vec<usize>,
    /// Its summary says more than its label: the deeper map marks it new.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
}

impl BehaviorMap {
    /// The group's comparison, when it has one.
    pub fn comparison(&self, group: usize) -> Option<&Comparison> {
        self.comparisons.get(group)
    }

    /// The actions in the group.
    pub fn in_group(&self, group: usize) -> impl Iterator<Item = (usize, &MapAction)> {
        self.actions
            .iter()
            .enumerate()
            .filter(move |(_, action)| action.group == group)
    }
}
