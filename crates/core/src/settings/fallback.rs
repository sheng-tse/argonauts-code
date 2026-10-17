//! Fallbacks: who continues when a model can't be reached, its plan ran out, or a limit you
//! set was reached. The top-level `[fallback]` holds the defaults, `[fallback.<role>]` overrides
//! them for one of the crew's five roles, and either can name fallbacks per agent.
//! [`FallbackTable::applied`] walks the order.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use super::Span;
use super::crew::CrewRole;
use crate::names::{AgentId, ModelRef, RoleName};

/// `[fallback.<role>]` is written for the crew's five roles. Each finer role reads the table
/// of one of them, which isn't always the role whose models it takes.
impl CrewRole {
    /// The crew role whose fallback table a finer role reads: those that make something read
    /// the workers', those that check or compare the reviewers', and those that decide the
    /// judge's.
    pub fn fallback_of(role: RoleName) -> Self {
        match role {
            RoleName::Main | RoleName::Consultant | RoleName::Narrator => Self::Main,
            RoleName::Orchestrator | RoleName::TieBreaker => Self::Orchestrator,
            RoleName::Worker | RoleName::Draft | RoleName::Reader | RoleName::Experiment => {
                Self::Worker
            }
            RoleName::Reviewer
            | RoleName::Critic
            | RoleName::Comparer
            | RoleName::Map
            | RoleName::RunReview => Self::Reviewer,
            RoleName::Judge => Self::Judge,
        }
    }
}

/// How the results combine when several fallbacks do the same task at once (`together` of 2
/// or more).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    /// The first result that passes your checks wins, and the others stop.
    FirstPassing,
    /// The judge picks the best result.
    Judge,
    /// Every finding from all of them is kept, duplicates folded.
    Merge,
}

impl Pick {
    /// What a role's fallbacks do unless you say: plans and designs go to the judge, reviews
    /// are merged, and for everything built the first to pass wins.
    pub fn default_for(role: RoleName) -> Self {
        match role {
            RoleName::Draft | RoleName::Reader => Self::Judge,
            _ => match CrewRole::fallback_of(role) {
                CrewRole::Reviewer => Self::Merge,
                _ => Self::FirstPassing,
            },
        }
    }
}

/// All of `[fallback]`: the table for every role, and `[fallback.<role>]` for each of the
/// crew's five that you wrote one for.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct FallbackSettings {
    #[serde(flatten)]
    pub every_role: FallbackTable,
    #[serde(flatten, deserialize_with = "role_tables")]
    pub roles: BTreeMap<CrewRole, FallbackTable>,
}

/// The role tables among whatever else a `[fallback]` table holds. A key that names no role
/// is passed over, as an unknown key is anywhere else, so the resolver can say it's unknown
/// rather than serde refusing it as a misspelt role.
fn role_tables<'de, D>(deserializer: D) -> Result<BTreeMap<CrewRole, FallbackTable>, D::Error>
where
    D: Deserializer<'de>,
{
    let others = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    others
        .into_iter()
        .filter_map(|(name, table)| Some((name.parse().ok()?, table)))
        .map(|(role, table)| {
            let table = FallbackTable::deserialize(table).map_err(D::Error::custom)?;
            Ok((role, table))
        })
        .collect()
}

impl FallbackSettings {
    /// The built-in table for every role, and no role's own.
    pub fn built_in() -> Self {
        Self {
            every_role: FallbackTable::built_in(),
            roles: BTreeMap::new(),
        }
    }

    /// The table of the crew role that `role` follows, when you wrote one.
    pub fn for_role(&self, role: RoleName) -> Option<&FallbackTable> {
        self.roles.get(&CrewRole::fallback_of(role))
    }

    /// What applies when `agent` stops a task in `role`, from the table for every role and
    /// the role's own.
    pub fn applied_to(&self, role: RoleName, agent_id: &AgentId) -> AppliedFallback {
        let mut applied = FallbackTable::applied(&self.every_role, self.for_role(role), agent_id);
        applied.pick.get_or_insert(Pick::default_for(role));
        applied
    }
}

/// The table for every role is what nearly every reader wants, so `settings.fallback.mode`
/// reads it. A role's own table is asked for by name.
impl std::ops::Deref for FallbackSettings {
    type Target = FallbackTable;

    fn deref(&self) -> &FallbackTable {
        &self.every_role
    }
}

impl std::ops::DerefMut for FallbackSettings {
    fn deref_mut(&mut self) -> &mut FallbackTable {
        &mut self.every_role
    }
}

/// One `[fallback]` table, the top-level one or a role's.
///
/// Candidates come from the most specific list that isn't empty: your pick for the task, then
/// `fallback.<role>.replace.<agent>`, `fallback.<role>.order`, `fallback.replace.<agent>`, and
/// `fallback.order`. Lists never merge across tables. When all are empty, the candidates are
/// the other installed and logged-in CLIs, which the policy builder fills in for each run,
/// since what's installed and logged in changes.
///
/// The optional keys stay `None` when a table doesn't set them, so the walk can tell unset
/// from a default. They follow the same order, key by key. `mode`, `ask_timeout`,
/// `allow_api_billing`, and `broken_work` have no per-agent form, so the resolver copies them
/// from the top-level table into each role table that doesn't set them. It copies nothing else.
/// `broken_work` decides whether the code carries over, and `context` what the fallback is
/// told about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct FallbackTable {
    pub mode: FallbackMode,
    /// Tried in order.
    pub order: Vec<FallbackEntry>,
    /// How long an ask waits before falling back to auto: `"none"` or a span.
    pub ask_timeout: Span,
    /// Moving to a paid API needs your OK unless this is set.
    pub allow_api_billing: bool,
    pub broken_work: BrokenWork,
    /// What an agent on its CLI's wrap-up allowance does, as Claude Code gives at a limit.
    pub wrap_up: WrapUp,
    /// How one agent's stopped tasks are shared out when each goes to one fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<OrderStrategy>,
    /// How many fallbacks work at once. With 2 or more they do the same task side by
    /// side, each in its own worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub together: Option<u32>,
    /// How their results combine. Unset is [`Pick::default_for`] the role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pick: Option<Pick>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_return: Option<OnReturn>,
    /// Tag the task's state `argonauts/<run>/<task>/fallback-<n>` at every fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_rule: Option<MergeRule>,
    /// How a model counts when it has no tier and too short a track record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_tier: Option<UnknownTier>,
    /// Per task, before it comes to you instead of bouncing between agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fallbacks: Option<u32>,
    /// Keeps every fallback on the CLI that stopped. When none of its models can take the work,
    /// the work waits for the CLI instead of moving to another one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_cli: Option<bool>,
    /// When nothing else can take a job of a step, whether another model of a CLI already at
    /// work in the step may: `"ask"`, `"allow"`, or `"never"`, when the run stops instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_step: Option<SameStep>,
    /// How the same model tries again before any fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<Retry>,
    /// What a fallback is told, also when a full session rotates to a fresh one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<FallbackContext>,
    /// Who writes the summary for `context = "note+summary"`: `cli/model` or an endpoint.
    /// Unset uses the fallback's own CLI with its cheapest model, because that agent
    /// is known to have plan usage and the stopped one may have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summarizer: Option<ModelRef>,
    pub replace: BTreeMap<AgentId, AgentFallbacks>,
}

/// A table that sets nothing. The top-level `[fallback]` starts from [`FallbackTable::built_in`].
impl Default for FallbackTable {
    fn default() -> Self {
        Self {
            mode: FallbackMode::Auto,
            order: Vec::new(),
            ask_timeout: Span::new("none"),
            allow_api_billing: false,
            broken_work: BrokenWork::Continue,
            wrap_up: WrapUp::LetFinish,
            strategy: None,
            together: None,
            pick: None,
            on_return: None,
            checkpoint: None,
            merge_rule: None,
            unknown_tier: None,
            max_fallbacks: None,
            same_cli: None,
            same_step: None,
            retry: None,
            context: None,
            summarizer: None,
            replace: BTreeMap::new(),
        }
    }
}

impl FallbackTable {
    /// The top-level defaults, with every optional key set to its built-in value.
    pub fn built_in() -> Self {
        let rules = AppliedFallback::default();
        Self {
            strategy: Some(OrderStrategy::First),
            together: Some(rules.together),
            on_return: Some(rules.on_return),
            checkpoint: Some(rules.checkpoint),
            merge_rule: Some(rules.merge_rule),
            unknown_tier: Some(rules.unknown_tier),
            max_fallbacks: Some(rules.max_fallbacks),
            same_cli: Some(rules.same_cli),
            same_step: Some(rules.same_step),
            retry: Some(rules.retry),
            context: Some(rules.context),
            ..Self::default()
        }
    }

    /// What applies when `agent` stops a task in a role: `role` is that role's table, if it
    /// has one, and `top` is the top-level `[fallback]`. Your pick for the task still comes first.
    pub fn applied(
        top: &FallbackTable,
        role: Option<&FallbackTable>,
        agent_id: &AgentId,
    ) -> AppliedFallback {
        let role_replace = role.and_then(|role| role.replace.get(agent_id));
        let top_replace = top.replace.get(agent_id);
        let lists = [
            role_replace.map(|entry| &entry.with),
            role.map(|role| &role.order),
            top_replace.map(|entry| &entry.with),
            Some(&top.order),
        ];
        let candidates = lists
            .into_iter()
            .flatten()
            .find(|list| !list.is_empty())
            .cloned()
            .unwrap_or_default();

        // Keys with no per-agent form have only the role's table and the top-level one.
        fn set<T>(
            role: Option<&FallbackTable>,
            top: &FallbackTable,
            key: impl Fn(&FallbackTable) -> Option<T>,
        ) -> Option<T> {
            role.and_then(&key).or_else(|| key(top))
        }
        let built_in = AppliedFallback::default();
        let own = role.unwrap_or(top);
        let together = set(role, top, |table| table.together)
            .unwrap_or(built_in.together)
            .max(1);
        AppliedFallback {
            candidates,
            mode: own.mode,
            ask_timeout: own.ask_timeout.clone(),
            allow_api_billing: own.allow_api_billing,
            broken_work: own.broken_work,
            strategy: if together >= 2 {
                FallbackStrategy::Race
            } else {
                role_replace
                    .and_then(|entry| entry.strategy)
                    .or_else(|| role.and_then(|role| role.strategy))
                    .or_else(|| top_replace.and_then(|entry| entry.strategy))
                    .or(top.strategy)
                    .map_or(built_in.strategy, FallbackStrategy::from)
            },
            together,
            pick: set(role, top, |table| table.pick),
            on_return: role_replace
                .and_then(|entry| entry.on_return)
                .or_else(|| role.and_then(FallbackTable::own_on_return))
                .or_else(|| top_replace.and_then(|entry| entry.on_return))
                .or_else(|| top.own_on_return())
                .unwrap_or(built_in.on_return),
            context: role_replace
                .and_then(|entry| entry.context)
                .or_else(|| role.and_then(|role| role.context))
                .or_else(|| top_replace.and_then(|entry| entry.context))
                .or(top.context)
                .unwrap_or(built_in.context),
            checkpoint: set(role, top, |table| table.checkpoint).unwrap_or(built_in.checkpoint),
            merge_rule: set(role, top, |table| table.merge_rule).unwrap_or(built_in.merge_rule),
            unknown_tier: set(role, top, |table| table.unknown_tier)
                .unwrap_or(built_in.unknown_tier),
            max_fallbacks: set(role, top, |table| table.max_fallbacks)
                .unwrap_or(built_in.max_fallbacks),
            same_cli: set(role, top, |table| table.same_cli).unwrap_or(built_in.same_cli),
            same_step: set(role, top, |table| table.same_step).unwrap_or(built_in.same_step),
            retry: set(role, top, |table| table.retry.clone()).unwrap_or(built_in.retry),
            summarizer: set(role, top, |table| table.summarizer.clone()),
        }
    }

    fn own_on_return(&self) -> Option<OnReturn> {
        self.on_return
    }
}

/// The fallback rules for one role and one stopped agent, every key filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedFallback {
    /// The most specific list that isn't empty. Empty means the other installed and logged-in
    /// CLIs, which the policy builder fills in.
    pub candidates: Vec<FallbackEntry>,
    pub mode: FallbackMode,
    pub ask_timeout: Span,
    pub allow_api_billing: bool,
    pub broken_work: BrokenWork,
    /// `Race` when `together` is 2 or more, else how the order is walked.
    pub strategy: FallbackStrategy,
    /// How many fallbacks work at once.
    pub together: u32,
    /// `None` is [`Pick::default_for`] the role, which [`FallbackTable::applied_to`] fills.
    pub pick: Option<Pick>,
    pub on_return: OnReturn,
    pub checkpoint: bool,
    pub merge_rule: MergeRule,
    pub unknown_tier: UnknownTier,
    pub max_fallbacks: u32,
    /// Fallbacks stay on the CLI that stopped, and the work waits for it when none is left.
    pub same_cli: bool,
    /// Whether another model of a CLI already at work in the step may take a job nothing
    /// else can.
    pub same_step: SameStep,
    pub retry: Retry,
    pub context: FallbackContext,
    /// `None` uses the fallback's own CLI with its cheapest model.
    pub summarizer: Option<ModelRef>,
}

impl Default for AppliedFallback {
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
            mode: FallbackMode::Auto,
            ask_timeout: Span::new("none"),
            allow_api_billing: false,
            broken_work: BrokenWork::Continue,
            strategy: FallbackStrategy::First,
            together: 1,
            pick: None,
            on_return: OnReturn::Ask,
            checkpoint: true,
            merge_rule: MergeRule::UpgradeReview,
            unknown_tier: UnknownTier::Weaker,
            max_fallbacks: 3,
            same_cli: false,
            same_step: SameStep::Ask,
            retry: Retry::default(),
            context: FallbackContext::NoteAndTranscript,
            summarizer: None,
        }
    }
}

/// `[fallback.retry]`: the same model going again after a failure that may pass, before any
/// fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Retry {
    /// How many times the same model goes again. 0 falls back at once.
    pub tries: u32,
    /// The wait before the first of them.
    pub wait: Span,
    pub backoff: Backoff,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            tries: 1,
            wait: Span::new("5s"),
            backoff: Backoff::Fixed,
        }
    }
}

impl Retry {
    /// The default retries with no wait before them, as a test that can't sit through one wants.
    pub fn without_wait() -> Self {
        Self {
            wait: Span::new("0s"),
            ..Self::default()
        }
    }

    /// The wait before retry number `retry`, counted from 1: `wait` each time, or doubled
    /// after each one. A `wait` that can't be read waits not at all.
    pub fn wait_before(&self, retry: u32) -> std::time::Duration {
        let wait = self.wait.parse().ok().flatten().unwrap_or_default();
        match self.backoff {
            Backoff::Fixed => wait,
            Backoff::Double => {
                let doublings = retry.saturating_sub(1).min(MAX_DOUBLINGS);
                wait.saturating_mul(2u32.pow(doublings))
            }
        }
    }
}

/// Past this many doublings a wait stops growing: 5s has become nearly three hours.
const MAX_DOUBLINGS: u32 = 11;

/// How the wait between retries grows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Backoff {
    /// The same wait before every retry.
    #[default]
    Fixed,
    /// Twice the wait before each next retry: 5s, 10s, 20s.
    Double,
}

/// A job of a step that nothing else can take, and a CLI already at work in that step that has
/// another model: two answers from one CLI are less independent than two from two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SameStep {
    /// You're asked whether it may.
    Ask,
    /// It takes the job, and one line says so.
    Allow,
    /// It never does, and the run stops instead.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FallbackMode {
    /// Show the fallback question.
    Ask,
    /// Take the next valid candidate within your cost cap.
    Auto,
    Wait,
}

/// A fallback entry: a plain `"agent/model"`, or a table with its own rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FallbackEntry {
    Model(ModelRef),
    /// `{ model = "openrouter/<model>", max_spend_usd = 2, instructions = "..." }`.
    WithRules(FallbackRules),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FallbackRules {
    pub model: ModelRef,
    #[serde(default)]
    pub max_spend_usd: Option<f64>,
    #[serde(default)]
    pub instructions: Option<String>,
    /// Read-only even if the role usually builds.
    #[serde(default)]
    pub read_only: bool,
    /// This entry may bill an API without asking. It counts only from your own settings
    /// layers, like the safety keys.
    #[serde(default)]
    pub allow_api_billing: bool,
}

impl FallbackEntry {
    pub fn model(&self) -> &ModelRef {
        match self {
            Self::Model(model) => model,
            Self::WithRules(rules) => &rules.model,
        }
    }
}

/// What a fallback does with the broken work it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrokenWork {
    Continue,
    /// Restart from the task's base.
    Restart,
}

/// An agent whose CLI is out of plan usage but finishing on a small allowance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WrapUp {
    /// It finishes, and its work moves only if it ends on the limit after all.
    #[default]
    LetFinish,
    /// Stopped at the first sign and fell back at once, to save the weekly allowance.
    Fallback,
}

/// The fallbacks for one agent. Unset keys come from the next list in the order above.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AgentFallbacks {
    pub with: Vec<FallbackEntry>,
    pub strategy: Option<OrderStrategy>,
    pub on_return: Option<OnReturn>,
    pub context: Option<FallbackContext>,
}

/// How one agent's stopped tasks are shared out when each goes to one fallback
/// (`strategy`). Several fallbacks on one task is `together`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrderStrategy {
    /// Each stopped task goes to the first valid fallback, then the next if that one runs out.
    #[default]
    First,
    /// The stopped agent's tasks are spread across every valid fallback, balanced by plan
    /// left, free slots, and track record.
    Spread,
}

impl From<OrderStrategy> for FallbackStrategy {
    fn from(order: OrderStrategy) -> Self {
        match order {
            OrderStrategy::First => Self::First,
            OrderStrategy::Spread => Self::Spread,
        }
    }
}

/// How the engine moves a stopped task, as the fallback rules settle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FallbackStrategy {
    /// Each stopped task goes to the first valid candidate, then the next if that one runs out.
    #[default]
    First,
    /// The stopped agent's tasks are spread across every valid candidate, balanced by plan usage
    /// left, free slots, and track record. Each task still has one worker at a time.
    Spread,
    /// One task goes to `together` candidates at once, each in its own worktree, and `pick`
    /// says how their results combine. The others' work stays on side branches.
    Race,
}

/// What happens to a task with quality debt when its original agent is back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnReturn {
    /// The "original is back" question asks you.
    #[default]
    Ask,
    /// The fallback keeps the task, and the debt stays for the merge rule.
    Keep,
    /// The original reviews everything since the checkpoint, then fixes it or sends it back.
    Review,
    /// The original takes the task back from its current state.
    GiveBack,
    /// The task resets to the checkpoint and the original redoes the work.
    Redo,
}

/// What a fallback gets besides the task's code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub enum FallbackContext {
    /// The task only.
    #[serde(rename = "fresh")]
    Fresh,
    /// The fallback note rendered as a prompt.
    #[serde(rename = "note")]
    Note,
    /// The note, plus the old session's transcript as a Markdown file. The note names its path
    /// and says to read it only if more detail is needed.
    #[default]
    #[serde(rename = "note+transcript")]
    NoteAndTranscript,
    /// The note, with a summary of that transcript by `summarizer` written into it. If the
    /// summary fails, the fallback goes on as `note+transcript`.
    #[serde(rename = "note+summary")]
    NoteAndSummary,
}

impl FallbackContext {
    /// How the transcript line names it, as in `task-003 → codex/gpt-5.6 · note + transcript`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Note => "note",
            Self::NoteAndTranscript => "note + transcript",
            Self::NoteAndSummary => "note + summary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeRule {
    /// A task with debt merges only after the original, or a model at least as strong,
    /// reviewed it. Merging anyway needs you unless `approvals.rules.merge.allow_debt` is set.
    #[default]
    UpgradeReview,
    /// Debt is shown, never enforced.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnknownTier {
    /// An unknown fallback always gets checked.
    #[default]
    Weaker,
    Equal,
}
