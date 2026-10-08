//! Fallbacks: who continues when a model can't be reached, its plan ran out, or a limit you
//! set was reached. The top-level `[fallback]` holds the defaults, `[fallback.<role>]` overrides
//! them for one of the crew's five roles, and either can name fallbacks per agent.
//! [`HandoffSettings::applied`] walks the order.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use super::Span;
use crate::names::{RoleName, VendorId, WorkerRef};

/// One of the five roles a crew has. Each finer role follows one of them, and
/// `[fallback.<role>]` is written for these.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CrewRole {
    Main,
    Orchestrator,
    Workers,
    Reviewers,
    Judge,
}

impl CrewRole {
    pub const ALL: [Self; 5] = [
        Self::Main,
        Self::Orchestrator,
        Self::Workers,
        Self::Reviewers,
        Self::Judge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Orchestrator => "orchestrator",
            Self::Workers => "workers",
            Self::Reviewers => "reviewers",
            Self::Judge => "judge",
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == name)
    }

    /// The crew role a finer role follows: those that make something follow the workers,
    /// those that check or compare follow the reviewers, and those that decide the judge.
    pub fn of(role: RoleName) -> Self {
        match role {
            RoleName::Main | RoleName::Consultant | RoleName::Narrator => Self::Main,
            RoleName::Orchestrator | RoleName::PhaseLead => Self::Orchestrator,
            RoleName::Worker | RoleName::Drafter | RoleName::Restater | RoleName::Spike => {
                Self::Workers
            }
            RoleName::Reviewer
            | RoleName::Critic
            | RoleName::Differ
            | RoleName::Mapper
            | RoleName::Adjudicator
            | RoleName::Retro => Self::Reviewers,
            RoleName::Judge | RoleName::Approver => Self::Judge,
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
            RoleName::Drafter | RoleName::Restater => Self::Judge,
            _ => match CrewRole::of(role) {
                CrewRole::Reviewers => Self::Merge,
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
    pub every_role: HandoffSettings,
    #[serde(flatten, deserialize_with = "role_tables")]
    pub roles: BTreeMap<CrewRole, HandoffSettings>,
}

/// The role tables among whatever else a `[fallback]` table holds. A key that names no role
/// is passed over, as an unknown key is anywhere else, so the resolver can say it's unknown
/// rather than serde refusing it as a misspelt role.
fn role_tables<'de, D>(deserializer: D) -> Result<BTreeMap<CrewRole, HandoffSettings>, D::Error>
where
    D: Deserializer<'de>,
{
    let others = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    others
        .into_iter()
        .filter_map(|(name, table)| Some((CrewRole::named(&name)?, table)))
        .map(|(role, table)| {
            let table = HandoffSettings::deserialize(table).map_err(D::Error::custom)?;
            Ok((role, table))
        })
        .collect()
}

impl FallbackSettings {
    /// The built-in table for every role, and no role's own.
    pub fn built_in() -> Self {
        Self {
            every_role: HandoffSettings::built_in(),
            roles: BTreeMap::new(),
        }
    }

    /// The table of the crew role that `role` follows, when you wrote one.
    pub fn for_role(&self, role: RoleName) -> Option<&HandoffSettings> {
        self.roles.get(&CrewRole::of(role))
    }

    /// What applies when `vendor` stops a task in `role`, from the table for every role and
    /// the role's own.
    pub fn applied_to(&self, role: RoleName, vendor: &VendorId) -> AppliedHandoff {
        let mut applied = HandoffSettings::applied(&self.every_role, self.for_role(role), vendor);
        applied.pick.get_or_insert(Pick::default_for(role));
        applied
    }
}

/// The table for every role is what nearly every reader wants, so `settings.fallback.mode`
/// reads it. A role's own table is asked for by name.
impl std::ops::Deref for FallbackSettings {
    type Target = HandoffSettings;

    fn deref(&self) -> &HandoffSettings {
        &self.every_role
    }
}

impl std::ops::DerefMut for FallbackSettings {
    fn deref_mut(&mut self) -> &mut HandoffSettings {
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
pub struct HandoffSettings {
    pub mode: HandoffMode,
    /// Tried in order.
    pub order: Vec<FallbackEntry>,
    /// How long an ask waits before falling back to auto: `"none"` or a span.
    pub ask_timeout: Span,
    /// Moving to a paid API needs your OK unless this is set.
    pub allow_api_billing: bool,
    /// The older spelling of `on_return = "hand_back"`, for this table only.
    pub return_to_original: bool,
    pub broken_work: BrokenWork,
    /// What an agent on its CLI's wrap-up allowance does, as Claude Code gives at a limit.
    pub wrap_up: WrapUp,
    /// How one agent's stopped tasks are shared out when each goes to one fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<OrderStrategy>,
    /// How many fallbacks take over at once. With 2 or more they do the same task side by
    /// side, each in its own worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub together: Option<u32>,
    /// How their results combine. Unset is [`Pick::default_for`] the role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pick: Option<Pick>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_return: Option<OnReturn>,
    /// Tag the card's state `argonauts/<run>/<card>/handoff-<n>` at every handoff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_rule: Option<MergeRule>,
    /// How a model counts when it has no tier and too short a track record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_tier: Option<UnknownTier>,
    /// Per card, before it escalates to you instead of bouncing between vendors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fallbacks: Option<u32>,
    /// What a replacement is told, also when a full session rotates to a fresh one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<HandoffContext>,
    /// Who writes the summary for `context = "note+summary"`: `harness/model` or an endpoint.
    /// Unset uses the replacement's own harness with its cheapest model, because that vendor
    /// is known to have quota and the stopped one may have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summarizer: Option<WorkerRef>,
    pub replace: BTreeMap<VendorId, VendorReplacement>,
}

/// A table that sets nothing. The top-level `[handoff]` starts from [`HandoffSettings::built_in`].
impl Default for HandoffSettings {
    fn default() -> Self {
        Self {
            mode: HandoffMode::Auto,
            order: Vec::new(),
            ask_timeout: Span::new("none"),
            allow_api_billing: false,
            return_to_original: false,
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
            context: None,
            summarizer: None,
            replace: BTreeMap::new(),
        }
    }
}

impl HandoffSettings {
    /// The top-level defaults, with every optional key set to its built-in value.
    pub fn built_in() -> Self {
        let rules = AppliedHandoff::default();
        Self {
            strategy: Some(OrderStrategy::First),
            together: Some(rules.together),
            on_return: Some(rules.on_return),
            checkpoint: Some(rules.checkpoint),
            merge_rule: Some(rules.merge_rule),
            unknown_tier: Some(rules.unknown_tier),
            max_fallbacks: Some(rules.max_fallbacks),
            context: Some(rules.context),
            ..Self::default()
        }
    }

    /// What applies when `vendor` stops a card in a role: `role` is that role's table, if it
    /// has one, and `top` is the top-level `[handoff]`. Your pick for the card still comes first.
    pub fn applied(
        top: &HandoffSettings,
        role: Option<&HandoffSettings>,
        vendor: &VendorId,
    ) -> AppliedHandoff {
        let role_replace = role.and_then(|role| role.replace.get(vendor));
        let top_replace = top.replace.get(vendor);
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

        // Keys with no per-vendor form have only the role's table and the top-level one.
        fn set<T>(
            role: Option<&HandoffSettings>,
            top: &HandoffSettings,
            key: impl Fn(&HandoffSettings) -> Option<T>,
        ) -> Option<T> {
            role.and_then(&key).or_else(|| key(top))
        }
        let built_in = AppliedHandoff::default();
        let own = role.unwrap_or(top);
        let together = set(role, top, |table| table.together)
            .unwrap_or(built_in.together)
            .max(1);
        AppliedHandoff {
            candidates,
            mode: own.mode,
            ask_timeout: own.ask_timeout.clone(),
            allow_api_billing: own.allow_api_billing,
            broken_work: own.broken_work,
            strategy: if together >= 2 {
                HandoffStrategy::Race
            } else {
                role_replace
                    .and_then(|entry| entry.strategy)
                    .or_else(|| role.and_then(|role| role.strategy))
                    .or_else(|| top_replace.and_then(|entry| entry.strategy))
                    .or(top.strategy)
                    .map_or(built_in.strategy, HandoffStrategy::from)
            },
            together,
            pick: set(role, top, |table| table.pick),
            on_return: role_replace
                .and_then(|entry| entry.on_return)
                .or_else(|| role.and_then(HandoffSettings::own_on_return))
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
            summarizer: set(role, top, |table| table.summarizer.clone()),
        }
    }

    fn own_on_return(&self) -> Option<OnReturn> {
        self.on_return
            .or(self.return_to_original.then_some(OnReturn::HandBack))
    }
}

/// The handoff rules for one role and one stopped vendor, every key filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedHandoff {
    /// The most specific list that isn't empty. Empty means the other installed and logged-in
    /// CLIs, which the policy builder fills in.
    pub candidates: Vec<FallbackEntry>,
    pub mode: HandoffMode,
    pub ask_timeout: Span,
    pub allow_api_billing: bool,
    pub broken_work: BrokenWork,
    /// `Race` when `together` is 2 or more, else how the order is walked.
    pub strategy: HandoffStrategy,
    /// How many fallbacks take over at once.
    pub together: u32,
    /// `None` is [`Pick::default_for`] the role, which [`HandoffSettings::applied_to`] fills.
    pub pick: Option<Pick>,
    pub on_return: OnReturn,
    pub checkpoint: bool,
    pub merge_rule: MergeRule,
    pub unknown_tier: UnknownTier,
    pub max_fallbacks: u32,
    pub context: HandoffContext,
    /// `None` uses the replacement's own harness with its cheapest model.
    pub summarizer: Option<WorkerRef>,
}

impl Default for AppliedHandoff {
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
            mode: HandoffMode::Auto,
            ask_timeout: Span::new("none"),
            allow_api_billing: false,
            broken_work: BrokenWork::Continue,
            strategy: HandoffStrategy::First,
            together: 1,
            pick: None,
            on_return: OnReturn::Ask,
            checkpoint: true,
            merge_rule: MergeRule::UpgradeReview,
            unknown_tier: UnknownTier::Weaker,
            max_fallbacks: 3,
            context: HandoffContext::NoteAndTranscript,
            summarizer: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandoffMode {
    /// Show the handoff card.
    Ask,
    /// Take the next valid candidate within your cost cap.
    Auto,
    Wait,
}

/// A fallback entry: a plain `"worker/model"`, or a table with its own rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FallbackEntry {
    Worker(WorkerRef),
    /// `{ worker = "openrouter/<model>", max_spend_usd = 2, instructions = "..." }`.
    WithRules(FallbackRules),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FallbackRules {
    pub worker: WorkerRef,
    #[serde(default, rename = "max_spend_usd")]
    pub max_budget_usd: Option<f64>,
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
    pub fn worker(&self) -> &WorkerRef {
        match self {
            Self::Worker(worker) => worker,
            Self::WithRules(rules) => &rules.worker,
        }
    }
}

/// What a new owner does with the previous owner's broken work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrokenWork {
    Continue,
    /// Restart from the card's base.
    Restart,
}

/// An agent whose CLI is out of quota but finishing on a small allowance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WrapUp {
    /// It finishes, and its work moves only if it ends on the limit after all.
    #[default]
    LetFinish,
    /// Stopped at the first sign and handed off at once, to save the weekly allowance.
    HandOff,
}

/// Who replaces one vendor. Unset keys come from the next list in the order above.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct VendorReplacement {
    pub with: Vec<FallbackEntry>,
    pub strategy: Option<OrderStrategy>,
    pub on_return: Option<OnReturn>,
    pub context: Option<HandoffContext>,
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

impl From<OrderStrategy> for HandoffStrategy {
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
pub enum HandoffStrategy {
    /// Each stopped card goes to the first valid candidate, then the next if that one runs out.
    #[default]
    First,
    /// The stopped vendor's cards are spread across every valid candidate, balanced by quota
    /// left, free slots, and track record. Each card still has one owner at a time.
    Spread,
    /// One task goes to `together` candidates at once, each in its own worktree, and `pick`
    /// says how their results combine. The others' work stays on side branches.
    Race,
}

/// What happens to a card with quality debt when its original vendor is back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnReturn {
    /// The "original is back" card asks you.
    #[default]
    Ask,
    /// The replacement keeps the card, and the debt stays for the merge rule.
    Keep,
    /// The original reviews everything since the checkpoint, then fixes it or sends it back.
    Review,
    /// The original takes the card back from its current state.
    HandBack,
    /// The card resets to the checkpoint and the original redoes the work.
    Redo,
}

/// What a replacement gets besides the card's code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub enum HandoffContext {
    /// The card only.
    #[serde(rename = "fresh")]
    Fresh,
    /// The handoff note: the packet rendered as a prompt.
    #[serde(rename = "note")]
    Note,
    /// The note, plus the old session's transcript as a Markdown file. The note names its path
    /// and says to read it only if more detail is needed.
    #[default]
    #[serde(rename = "note+transcript")]
    NoteAndTranscript,
    /// The note, with a summary of that transcript by `summarizer` written into it. If the
    /// summary fails, the handoff goes on as `note+transcript`.
    #[serde(rename = "note+summary")]
    NoteAndSummary,
}

impl HandoffContext {
    /// How the transcript line names it, as in `card-003 → codex/gpt-5.6 · note + transcript`.
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
    /// A card with debt merges only after the original, or a model at least as strong,
    /// reviewed it. Merging anyway needs you unless `approvals.rules.merge.allow_debt` is set.
    #[default]
    UpgradeReview,
    /// Debt is shown, never enforced.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnknownTier {
    /// An unknown replacement always gets checked.
    #[default]
    Weaker,
    Equal,
}
