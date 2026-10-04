//! Approvals, hard limits, and the sandbox. Every key here is a safety key.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::names::WorkerRef;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ApprovalSettings {
    /// Who answers the crew's questions: the open questions from Understand, the go-ahead,
    /// a stuck goal, and "keep going as a goal". Unset follows the mode, so the lead answers in
    /// auto mode and you do otherwise. Set to the lead, it answers in every mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<Answerer>,
    /// Topics that come to you even when the lead answers, such as `security`.
    pub ask_you_about: Vec<String>,
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
            answered_by: None,
            ask_you_about: Vec::new(),
            goal_changes: GoalChanges::Ask,
            spot_check_sample: 0.1,
            pause_auto_when_spot_checks_waiting: 5,
            suggest_auto_after_matches: 20,
            goal: CheckpointSettings::auto(DecideWith::RulesAndModel),
            plan: CheckpointSettings::auto(DecideWith::RulesAndModel),
            merge: CheckpointSettings::auto(DecideWith::Rules),
            // The evolution loop is autonomous by default: replay and holdout checks pass first,
            // and the canary watches what's promoted.
            recipe_promotion: CheckpointSettings::auto(DecideWith::Rules),
            escalation: CheckpointSettings::auto(DecideWith::RulesAndModel),
            budget: CheckpointSettings::auto(DecideWith::RulesAndModel),
            change_request: CheckpointSettings::auto(DecideWith::RulesAndModel),
            approver: ApproverSettings::default(),
            presence: Presence::Warn,
            refuse_ancestor_env: vec!["OPENCLAW_SHELL".to_owned(), "HERMES_SESSION_ID".to_owned()],
        }
    }
}

impl ApprovalSettings {
    /// Who answers the crew's questions at a decision point running in `mode`.
    pub fn answerer(&self, mode: ApprovalMode) -> Answerer {
        self.answered_by.unwrap_or(match mode {
            ApprovalMode::Auto => Answerer::Lead,
            ApprovalMode::Supervised | ApprovalMode::Shadow => Answerer::You,
        })
    }
}

/// Who answers the crew's questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Answerer {
    You,
    /// The lead answers, and its answers become assumptions you can review. The approvals
    /// after them still follow their mode and rules, so in auto the approver model checks
    /// them, and a question it can't answer comes to you.
    Lead,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CheckpointSettings<R: Default> {
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    pub rules: R,
    pub otherwise: Otherwise,
}

impl<R: Default> CheckpointSettings<R> {
    /// Auto mode, where `decide_with` approves and anything it doesn't goes to you.
    pub fn auto(decide_with: DecideWith) -> Self {
        Self {
            mode: ApprovalMode::Auto,
            decide_with,
            ..Self::default()
        }
    }
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
    /// Both must approve.
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
            // The classes of the keys evolvable by default.
            change_classes: [
                "checklist_item",
                "critique_rounds",
                "drafting_prompt",
                "card_split",
                "card_budget",
            ]
            .map(str::to_owned)
            .to_vec(),
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
    /// Unset means the cheapest model from another company than the work it judges, which the
    /// policy builder picks from the models you can run.
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
    /// Whose rules guard the agents. A role's `[roles.<role>.sandbox]` can set this, `network`,
    /// `agents_see_other_copies`, and `agents_commit` for its own agents, as [`RoleSandbox`]
    /// says.
    pub mode: SandboxChoice,
    /// Network for commands agents run.
    pub network: bool,
    /// Whether an agent may read the other agents' copies of the repo, not only its own.
    pub agents_see_other_copies: bool,
    /// Whether agents commit their own work. Off, git's files stay read-only to them and
    /// Argonauts commits for them.
    pub agents_commit: bool,
    /// Refuse to start a harness session when no sandbox works.
    pub fail_closed: bool,
    pub extra_writable: Vec<String>,
    /// Hosts sessions may reach through the Argonauts proxy, on top of their harness's own
    /// list. `host` means port 443; `host:port` names another, and `*.example.com` covers its
    /// subdomains.
    pub proxy_allow: Vec<String>,
    pub untrusted_repos: UntrustedRepos,
    /// The image container sessions run in, with the harness installed. Empty refuses them.
    pub container_image: String,
    /// Whether your own harness hooks run in Argonauts sessions, where the harness allows a choice.
    pub user_hooks: UserHooks,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            mode: SandboxChoice::Os,
            // Agents that evolve themselves need packages and docs from the internet, and the
            // sandbox still keeps them out of your other files.
            network: true,
            agents_see_other_copies: false,
            agents_commit: true,
            fail_closed: true,
            extra_writable: Vec::new(),
            proxy_allow: Vec::new(),
            untrusted_repos: UntrustedRepos::Container,
            container_image: String::new(),
            user_hooks: UserHooks::Inherit,
        }
    }
}

impl SandboxSettings {
    /// The sandbox for one role's agents: each key the role's table sets wins, and the rest
    /// come from this top-level `[sandbox]`.
    pub fn applied(&self, role: Option<&RoleSandbox>) -> SandboxSettings {
        let Some(role) = role else {
            return self.clone();
        };
        SandboxSettings {
            mode: role.mode.unwrap_or(self.mode),
            network: role.network.unwrap_or(self.network),
            agents_see_other_copies: role
                .agents_see_other_copies
                .unwrap_or(self.agents_see_other_copies),
            agents_commit: role.agents_commit.unwrap_or(self.agents_commit),
            ..self.clone()
        }
    }
}

/// A role's `[roles.<role>.sandbox]`. A key it leaves unset comes from the top-level `[sandbox]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RoleSandbox {
    /// Whose rules guard this role's agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<SandboxChoice>,
    /// Network for commands this role's agents run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    /// Whether this role's agents may read the other agents' copies of the repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents_see_other_copies: Option<bool>,
    /// Whether this role's agents commit their own work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents_commit: Option<bool>,
}

impl RoleSandbox {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SandboxChoice {
    /// Seatbelt on macOS, Landlock with user, mount, network, and PID namespaces on Linux.
    Os,
    /// A container with only the worktree mounted.
    Container,
    /// Each AI tool's own sandbox and approvals, with ours off. `off` is its older name.
    #[serde(alias = "off")]
    Tool,
    /// No protection at all, neither ours nor the tool's. Never a default.
    None,
}

impl SandboxChoice {
    /// Ours is off, leaving each tool's own protection or none.
    pub fn ours_off(self) -> bool {
        matches!(self, Self::Tool | Self::None)
    }

    /// The word a settings file uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Os => "os",
            Self::Container => "container",
            Self::Tool => "tool",
            Self::None => "none",
        }
    }
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
