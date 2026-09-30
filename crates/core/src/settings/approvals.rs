//! Approvals, hard limits, and the sandbox. Every key here is a safety key.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::names::WorkerRef;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ApprovalSettings {
    pub goal_changes: GoalChanges,
    /// Share of auto-approvals queued for your spot check.
    pub spot_check_sample: f64,
    pub pause_auto_when_spot_checks_waiting: u32,
    /// Suggest auto after this many shadow matches in a row.
    pub suggest_auto_after_matches: u32,
    pub goal: CheckpointSettings<GoalRules>,
    pub plan: CheckpointSettings<PlanRules>,
    pub merge: CheckpointSettings<MergeRules>,
    pub recipe_promotion: CheckpointSettings<PromotionRules>,
    pub escalation: CheckpointSettings<EscalationRules>,
    /// Budget increases from the run's reserve.
    pub budget: CheckpointSettings<BudgetRules>,
    pub change_request: CheckpointSettings<NoRules>,
    pub approver: ApproverSettings,
    /// What an always-on agent running as you, such as Hermes or OpenClaw, changes.
    pub presence: Presence,
    /// Variables that mark another agent's shell. A caller with one in its ancestry is refused
    /// every approval, like one with `ARGONAUTS_SESSION`.
    pub refuse_ancestor_env: Vec<String>,
}

impl Default for ApprovalSettings {
    fn default() -> Self {
        Self {
            goal_changes: GoalChanges::Ask,
            spot_check_sample: 0.1,
            pause_auto_when_spot_checks_waiting: 5,
            suggest_auto_after_matches: 20,
            goal: CheckpointSettings::default(),
            plan: CheckpointSettings::default(),
            merge: CheckpointSettings::default(),
            recipe_promotion: CheckpointSettings::default(),
            escalation: CheckpointSettings::default(),
            budget: CheckpointSettings::default(),
            change_request: CheckpointSettings::default(),
            approver: ApproverSettings::default(),
            presence: Presence::Warn,
            refuse_ancestor_env: vec!["OPENCLAW_SHELL".to_owned(), "HERMES_SESSION_ID".to_owned()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CheckpointSettings<R: Default> {
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    pub rules: R,
    pub otherwise: Otherwise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Supervised,
    /// Waits for you, and records what auto would have decided.
    Shadow,
    /// Your settings decide; what they can't settle waits for you.
    Auto,
}

/// Who decides in auto mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecideWith {
    /// Your rules: code over measured evidence. Zero tokens.
    #[default]
    Rules,
    /// Your approver model, after the hard limits pass.
    Model,
    RulesAndModel,
    /// Rules first; what they don't approve goes to the model.
    RulesOrModel,
}

/// What happens when auto doesn't approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Otherwise {
    #[default]
    Ask,
    SendBack,
    Stop,
    /// Merge only: open a pull request and wait.
    OpenPullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Off,
    /// Doctor and the app's start check warn once.
    Warn,
    /// While such an agent runs, plan and merge approvals, and anything that raises autonomy,
    /// need a passkey with user verification. Refused until the passkey flow exists.
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalChanges {
    /// The run pauses at the next safe point and you decide.
    #[default]
    Ask,
    /// Clarifications that leave the criteria unchanged are recorded; the rest waits for you.
    AllowClarifications,
    Deny,
}

/// Goal rules have no numbers: auto approves only when no question is open.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct GoalRules {}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct NoRules {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PlanRules {
    /// Largest gap between one judge's two orders, out of 10.
    pub max_score_spread: f64,
    pub min_winner_margin: f64,
}

impl Default for PlanRules {
    fn default() -> Self {
        Self {
            max_score_spread: 1.0,
            min_winner_margin: 1.5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MergeRules {
    /// Share of changed lines covered by tests.
    pub min_changed_line_coverage: f64,
    pub max_combined_diff_lines: u32,
    /// Auto may merge a card whose quality debt no strong enough model reviewed.
    pub allow_debt: bool,
}

impl Default for MergeRules {
    fn default() -> Self {
        Self {
            min_changed_line_coverage: 0.8,
            max_combined_diff_lines: 400,
            allow_debt: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PromotionRules {
    pub change_classes: Vec<String>,
    /// Days between promotions or canaries of one recipe.
    pub min_days_between: u32,
}

impl Default for PromotionRules {
    fn default() -> Self {
        Self {
            change_classes: vec!["checklist_item".to_owned(), "drafting_prompt".to_owned()],
            min_days_between: 14,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EscalationRules {
    /// Extra attempts auto may grant a card, by a fallback agent from a family that hasn't tried it.
    pub max_extra_attempts: u32,
}

impl Default for EscalationRules {
    fn default() -> Self {
        Self {
            max_extra_attempts: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BudgetRules {
    /// Auto may release the run's reserve, never above a ceiling.
    pub use_reserve: bool,
}

impl Default for BudgetRules {
    fn default() -> Self {
        Self { use_reserve: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ApproverSettings {
    pub model: Option<WorkerRef>,
    /// Your rules for it, in `.argonauts/`.
    pub model_rules: String,
    /// Require a model family different from every author and owner.
    pub different_family: bool,
    /// A second approver from another vendor that must also approve.
    pub second_model: Option<WorkerRef>,
}

impl Default for ApproverSettings {
    fn default() -> Self {
        Self {
            model: None,
            model_rules: "approval-rules.md".to_owned(),
            different_family: true,
            second_model: None,
        }
    }
}

/// Hard limits. Your rules can't loosen them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HardLimitSettings {
    /// Paths no card may touch, on top of `.argonauts/**`. Glob patterns.
    pub protected_paths: Vec<String>,
    /// Packages a plan may add. The conductor adds them itself, with install scripts off.
    pub dependency_allowlist: Vec<String>,
    pub usd_per_day: Option<f64>,
    pub auto_merges_per_day: u32,
    /// Irreversible outside actions you allow, such as `publish`.
    pub irreversible_allowlist: Vec<String>,
    pub evolution_usd_per_month: f64,
}

impl Default for HardLimitSettings {
    fn default() -> Self {
        Self {
            protected_paths: [
                ".github/**",
                ".gitlab-ci.yml",
                "**/package-lock.json",
                "**/Cargo.lock",
                "**/pnpm-lock.yaml",
                "**/yarn.lock",
            ]
            .map(str::to_owned)
            .to_vec(),
            dependency_allowlist: Vec::new(),
            usd_per_day: Some(40.0),
            auto_merges_per_day: 3,
            irreversible_allowlist: Vec::new(),
            evolution_usd_per_month: 1000.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SandboxSettings {
    /// Every harness session and gate runs in the OS sandbox, or a container.
    pub mode: SandboxChoice,
    /// Network for commands agents run.
    pub network: bool,
    /// Refuse to start a harness session when no sandbox works.
    pub fail_closed: bool,
    pub extra_writable: Vec<String>,
    pub untrusted_repos: UntrustedRepos,
    /// Whether your own harness hooks run in Argonauts sessions, where the harness allows a choice.
    pub user_hooks: UserHooks,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            mode: SandboxChoice::Os,
            network: false,
            fail_closed: true,
            extra_writable: Vec::new(),
            untrusted_repos: UntrustedRepos::Container,
            user_hooks: UserHooks::Inherit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SandboxChoice {
    /// Seatbelt on macOS, Landlock plus seccomp on Linux.
    Os,
    /// A container with only the worktree mounted.
    Container,
    /// No sandbox. Solo runs only, and only when you set it; every checkpoint of such a run
    /// asks you, because `check_session` reports the missing sandbox.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UntrustedRepos {
    Container,
    Refuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserHooks {
    Inherit,
    /// Turned off for Argonauts sessions where the harness allows it; `doctor` says where not.
    Disable,
}
