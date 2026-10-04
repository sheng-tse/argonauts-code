//! Handoffs: who continues when an agent stops for a limit, an outage, a logout, or a billing
//! cap. The top-level `[handoff]` holds the defaults, `[roles.<role>.handoff]` overrides them,
//! and either can name replacements per vendor. [`HandoffSettings::applied`] walks the order.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;
use crate::names::{VendorId, WorkerRef};

/// One `[handoff]` table, top-level or a role's.
///
/// Candidates come from the most specific list that isn't empty: your pick for the card, then
/// `roles.<role>.handoff.replace.<vendor>`, `roles.<role>.handoff.fallback`,
/// `handoff.replace.<vendor>`, and `handoff.fallback`. Lists never merge across tables. When
/// all are empty, the candidates are the other installed and logged-in CLIs, which the policy
/// builder fills in for each run, since what's installed and logged in changes.
///
/// The optional keys stay `None` when a table doesn't set them, so the walk can tell unset
/// from a default. They follow the same order, key by key. `mode`, `ask_timeout`,
/// `allow_api_billing`, and `broken_work` have no per-vendor form, so the resolver copies them
/// from the top-level table into each role table that doesn't set them. It copies nothing else.
/// `broken_work` decides whether the code carries over, and `context` what the replacement is
/// told about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HandoffSettings {
    pub mode: HandoffMode,
    /// Tried in order.
    pub fallback: Vec<FallbackEntry>,
    /// How long an ask waits before falling back to auto: `"none"` or a span.
    pub ask_timeout: Span,
    /// Moving to a paid API needs your OK unless this is set.
    pub allow_api_billing: bool,
    /// The older spelling of `on_return = "hand_back"`, for this table only.
    pub return_to_original: bool,
    pub broken_work: BrokenWork,
    /// What an agent on its CLI's wrap-up allowance does, as Claude Code gives at a limit.
    pub wrap_up: WrapUp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<HandoffStrategy>,
    /// Attempts at once with `strategy = "race"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub race_count: Option<u32>,
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
    pub max_handoffs: Option<u32>,
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
            fallback: Vec::new(),
            ask_timeout: Span::new("none"),
            allow_api_billing: false,
            return_to_original: false,
            broken_work: BrokenWork::Continue,
            wrap_up: WrapUp::LetFinish,
            strategy: None,
            race_count: None,
            on_return: None,
            checkpoint: None,
            merge_rule: None,
            unknown_tier: None,
            max_handoffs: None,
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
            strategy: Some(rules.strategy),
            race_count: Some(rules.race_count),
            on_return: Some(rules.on_return),
            checkpoint: Some(rules.checkpoint),
            merge_rule: Some(rules.merge_rule),
            unknown_tier: Some(rules.unknown_tier),
            max_handoffs: Some(rules.max_handoffs),
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
            role.map(|role| &role.fallback),
            top_replace.map(|entry| &entry.with),
            Some(&top.fallback),
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
        AppliedHandoff {
            candidates,
            mode: own.mode,
            ask_timeout: own.ask_timeout.clone(),
            allow_api_billing: own.allow_api_billing,
            broken_work: own.broken_work,
            strategy: role_replace
                .and_then(|entry| entry.strategy)
                .or_else(|| role.and_then(|role| role.strategy))
                .or_else(|| top_replace.and_then(|entry| entry.strategy))
                .or(top.strategy)
                .unwrap_or(built_in.strategy),
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
            race_count: set(role, top, |table| table.race_count).unwrap_or(built_in.race_count),
            checkpoint: set(role, top, |table| table.checkpoint).unwrap_or(built_in.checkpoint),
            merge_rule: set(role, top, |table| table.merge_rule).unwrap_or(built_in.merge_rule),
            unknown_tier: set(role, top, |table| table.unknown_tier)
                .unwrap_or(built_in.unknown_tier),
            max_handoffs: set(role, top, |table| table.max_handoffs)
                .unwrap_or(built_in.max_handoffs),
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
    pub strategy: HandoffStrategy,
    pub race_count: u32,
    pub on_return: OnReturn,
    pub checkpoint: bool,
    pub merge_rule: MergeRule,
    pub unknown_tier: UnknownTier,
    pub max_handoffs: u32,
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
            race_count: 2,
            on_return: OnReturn::Ask,
            checkpoint: true,
            merge_rule: MergeRule::UpgradeReview,
            unknown_tier: UnknownTier::Weaker,
            max_handoffs: 3,
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
    /// `{ worker = "openrouter/<model>", max_budget_usd = 2, instructions = "..." }`.
    WithRules(FallbackRules),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FallbackRules {
    pub worker: WorkerRef,
    #[serde(default)]
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
    pub strategy: Option<HandoffStrategy>,
    pub on_return: Option<OnReturn>,
    pub context: Option<HandoffContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStrategy {
    /// Each stopped card goes to the first valid candidate, then the next if that one runs out.
    #[default]
    First,
    /// The stopped vendor's cards are spread across every valid candidate, balanced by quota
    /// left, free slots, and track record. Each card still has one owner at a time.
    Spread,
    /// One card goes to `race_count` candidates at once, each in its own worktree. The first
    /// that passes its gates and review wins, and the others' work stays on side branches.
    /// In `auto` mode a race starts only when a list that applies sets it.
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
    /// reviewed it. Merging anyway needs you unless `approvals.merge.rules.allow_debt` is set.
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
