//! Approvals, hard limits, and the sandbox. Every key here is a safety key.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::names::WorkerRef;
use crate::settings::asking::DEFAULT_ASK_LIMIT;
pub use crate::settings::asking::{Decides, WhenDeclined};
use crate::settings::budget::BudgetSettings;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ApprovalSettings {
    /// Full auto for every checkpoint a run opens that its own mode leaves to auto. One you
    /// keep supervised, or in shadow, stays yours. Off, each checkpoint keeps its own mode.
    pub autopilot: bool,
    /// Who answers the crew's questions: the open questions from Understand, the go-ahead,
    /// a stuck goal, "keep going as a goal", and in full auto the agents' questions meant for
    /// you. `you`, `lead`, or a model such as `codex/gpt-5.6`. Unset follows the mode, so the
    /// lead answers in auto mode and you do otherwise. Set to the lead, it answers in every
    /// mode. A model answers in the lead's place in full auto only, where nothing waits for
    /// you; elsewhere the mode decides, as when it's unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Answerer>,
    /// Who settles what your rules can't. `judge` is the cheapest model you can run from
    /// another provider than the work it judges. A model you name, such as
    /// `antigravity-cli/gemini-3.6-flash`, judges in its place.
    pub decides: Decides,
    /// Topics that come to you even when the lead answers, such as `security`. Full auto
    /// answers them too, since nothing waits for you there.
    pub ask_you_about: Vec<String>,
    /// What Esc on a question or an approval does: `decide` hands it to the agent that asked,
    /// the main agent, or your rules and the judge, and `stop` ends that part of the work.
    pub when_declined: WhenDeclined,
    /// How many times an agent that ends its turn with a question is told to decide and carry
    /// on. After that many it is told to take the option it recommended, so 0 tells it that
    /// at once.
    pub ask_limit: u32,
    pub goal_changes: GoalChanges,
    /// Share of auto-approvals queued for your spot check.
    pub spot_check_sample: f64,
    pub pause_auto_when_spot_checks_waiting: u32,
    /// Suggest auto after this many auto preview matches in a row.
    pub suggest_auto_after_matches: u32,
    /// What "your rules" means at each approval: the numbers that let it go ahead in auto.
    pub rules: ApprovalRules,
    pub goal: CheckpointSettings,
    pub plan: CheckpointSettings,
    pub merge: CheckpointSettings,
    pub workflow_promotion: PromotionSettings,
    /// A task that is stuck: sent back, failing, or out of its limit.
    #[serde(rename = "stuck")]
    pub escalation: CheckpointSettings,
    /// More limit from the run's reserve.
    #[serde(rename = "limit")]
    pub budget: CheckpointSettings,
    pub change_request: CheckpointSettings,
    pub approver: ApproverSettings,
    /// What an always-on agent running as you, such as Hermes or OpenClaw, changes.
    pub presence: Presence,
    /// Variables that mark another agent's shell. A caller with one in its ancestry is refused
    /// every approval, like one with `ARGONAUTS_SESSION`.
    pub refuse_ancestor_env: Vec<String>,
    /// Commands a direct turn runs without asking, saved from its questions' "don't ask again". A
    /// pattern ending in `*` covers every command it starts.
    pub allow_commands: Vec<String>,
}

impl Default for ApprovalSettings {
    fn default() -> Self {
        Self {
            autopilot: false,
            answers: None,
            decides: Decides::Judge,
            ask_you_about: Vec::new(),
            when_declined: WhenDeclined::default(),
            ask_limit: DEFAULT_ASK_LIMIT,
            goal_changes: GoalChanges::Ask,
            spot_check_sample: 0.1,
            pause_auto_when_spot_checks_waiting: 5,
            suggest_auto_after_matches: 20,
            rules: ApprovalRules::default(),
            goal: CheckpointSettings::auto(DecideWith::RulesAndModel),
            plan: CheckpointSettings::auto(DecideWith::RulesAndModel),
            merge: CheckpointSettings::auto(DecideWith::Rules),
            // The evolution loop is autonomous by default: replay and holdout checks pass first,
            // and the canary watches what's promoted.
            workflow_promotion: PromotionSettings::default(),
            escalation: CheckpointSettings::auto(DecideWith::RulesAndModel),
            budget: CheckpointSettings::auto(DecideWith::RulesAndModel),
            change_request: CheckpointSettings::auto(DecideWith::RulesAndModel),
            approver: ApproverSettings::default(),
            presence: Presence::Warn,
            refuse_ancestor_env: vec!["OPENCLAW_SHELL".to_owned(), "HERMES_SESSION_ID".to_owned()],
            allow_commands: Vec::new(),
        }
    }
}

impl ApprovalSettings {
    /// Who answers the crew's questions at a decision point running in `mode`. Full auto
    /// never has you answer, so there the model you picked answers, else the lead. A model
    /// you picked answers in full auto only, so elsewhere the mode decides, as when unset.
    pub fn answerer(&self, mode: ApprovalMode) -> Answerer {
        match (mode, &self.answers) {
            (ApprovalMode::Autopilot, Some(Answerer::Model(model))) => {
                Answerer::Model(model.clone())
            }
            (ApprovalMode::Autopilot, _) => Answerer::Main,
            (_, Some(Answerer::You)) => Answerer::You,
            (_, Some(Answerer::Main)) => Answerer::Main,
            (ApprovalMode::Auto, Some(Answerer::Model(_)) | None) => Answerer::Main,
            (
                ApprovalMode::Supervised | ApprovalMode::AutoPreview,
                Some(Answerer::Model(_)) | None,
            ) => Answerer::You,
        }
    }

    /// A run checkpoint's mode once the `autopilot` switch applies: full auto while it's on,
    /// unless the checkpoint's `own` mode keeps it for you (supervised or shadow), which no
    /// switch takes away. Off, its own. A learned workflow promotion is decided outside runs, so it
    /// keeps its own.
    pub fn run_mode(&self, own: ApprovalMode) -> ApprovalMode {
        if self.autopilot && own.goes_ahead() {
            ApprovalMode::Autopilot
        } else {
            own
        }
    }

    /// The model you named to settle what your rules can't. `None` is the judge: the first
    /// judge you named that may check the work, else one the policy picks for it.
    pub fn approver_model(&self) -> Option<&WorkerRef> {
        self.decides.model()
    }

    /// One mode for a run's checkpoints: theirs when they agree, else the least autonomous of
    /// them, since that one still waits for you. The kinds without settings of their own, such
    /// as an agent's question, follow it. A learned workflow promotion is decided outside runs, so it
    /// isn't one of them.
    pub fn mode(&self) -> ApprovalMode {
        [
            self.goal.mode,
            self.plan.mode,
            self.merge.mode,
            self.escalation.mode,
            self.budget.mode,
            self.change_request.mode,
        ]
        .into_iter()
        .map(|own| self.run_mode(own))
        .min_by_key(|mode| mode.autonomy())
        .unwrap_or_default()
    }
}

/// Who answers the crew's questions. Written as `you`, `lead`, or a model such as
/// `codex/gpt-5.6`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Answerer {
    You,
    /// The lead answers, and its answers become assumptions you can review. The approvals
    /// after them still follow their mode and rules, so in auto the approver model checks
    /// them, and a question it can't answer comes to you.
    Main,
    /// A model you picked answers in the lead's place, the same way: its answers become
    /// assumptions, and the approvals after them check them.
    Model(WorkerRef),
}

impl Answerer {
    /// Whether someone other than you answers, so the question never waits for you.
    pub fn answers_for_you(&self) -> bool {
        !matches!(self, Answerer::You)
    }

    /// The model you picked to answer, when you picked one.
    pub fn model(&self) -> Option<&WorkerRef> {
        match self {
            Answerer::Model(model) => Some(model),
            Answerer::You | Answerer::Main => None,
        }
    }
}

impl fmt::Display for Answerer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Answerer::You => f.write_str("you"),
            Answerer::Main => f.write_str("main"),
            Answerer::Model(model) => fmt::Display::fmt(model, f),
        }
    }
}

impl TryFrom<String> for Answerer {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("you") {
            return Ok(Answerer::You);
        }
        if value.eq_ignore_ascii_case("main") {
            return Ok(Answerer::Main);
        }
        value.parse().map(Answerer::Model).map_err(|_| {
            format!("expected `you`, `main`, or a model such as `codex/gpt-5.6`, not {value:?}")
        })
    }
}

impl From<Answerer> for String {
    fn from(value: Answerer) -> String {
        value.to_string()
    }
}

impl JsonSchema for Answerer {
    fn schema_name() -> Cow<'static, str> {
        "Answerer".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CheckpointSettings {
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    pub otherwise: Otherwise,
    /// In full auto, what a hard limit at this checkpoint does, since no one is there to ask.
    pub at_hard_limit: AtHardLimit,
}

impl CheckpointSettings {
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
    AutoPreview,
    /// Your settings decide; what they can't settle waits for you.
    Auto,
    /// Nothing waits for you. The lead answers every question, the topics
    /// `approvals.ask_you_about` names too, and your rules and the approver decide as in auto.
    /// What they can't approve, or a hard limit, ends that part safely, keeps the work, and is
    /// reported instead of asking.
    Autopilot,
}

impl ApprovalMode {
    /// Auto and full auto decide without you; supervised and shadow wait.
    pub fn goes_ahead(self) -> bool {
        matches!(self, ApprovalMode::Auto | ApprovalMode::Autopilot)
    }

    /// Higher decides more without you.
    pub fn autonomy(self) -> u8 {
        match self {
            ApprovalMode::Supervised => 0,
            ApprovalMode::AutoPreview => 1,
            ApprovalMode::Auto => 2,
            ApprovalMode::Autopilot => 3,
        }
    }

    /// The mode in words, as the mode line and the transcript say it.
    pub fn label(self) -> &'static str {
        match self {
            ApprovalMode::Supervised => "supervised",
            ApprovalMode::AutoPreview => "auto preview",
            ApprovalMode::Auto => "auto",
            ApprovalMode::Autopilot => "autopilot",
        }
    }
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

/// What a hard limit does in full auto, where no one is there to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AtHardLimit {
    /// Ends that part safely and keeps its work: a merge keeps its branch, a stuck card is
    /// deferred, a budget stops. A goal or a plan with nothing safe to end parks for you.
    #[default]
    Stop,
    /// Sends it back for another round, or declines the change.
    SendBack,
    /// Merge only: opens a pull request for you to review.
    OpenPullRequest,
    /// Leaves the card for you, and the run parks until you answer it.
    Park,
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

/// `[approvals.rules.<approval>]`: what "your rules" means for each approval that has numbers
/// to check. A goal has none: auto approves one only when no question is open.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ApprovalRules {
    pub plan: PlanRules,
    pub merge: MergeRules,
    pub workflow_promotion: PromotionRules,
    #[serde(rename = "stuck")]
    pub escalation: EscalationRules,
    #[serde(rename = "limit")]
    pub budget: BudgetRules,
}

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
    /// The share of changed lines the tests must cover, from 0 to 1.
    pub min_test_coverage_of_changed_lines: f64,
    /// The most lines the run's combined diff may have.
    pub max_changed_lines: u32,
    /// Auto may merge a card whose quality debt no strong enough model reviewed.
    pub allow_debt: bool,
}

impl Default for MergeRules {
    fn default() -> Self {
        Self {
            min_test_coverage_of_changed_lines: 0.8,
            max_changed_lines: 400,
            allow_debt: false,
        }
    }
}

/// Promoting a learned workflow change. It's decided outside runs, so the `autopilot` switch and what a
/// hard limit does there never reach it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PromotionSettings {
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    pub otherwise: Otherwise,
}

impl Default for PromotionSettings {
    fn default() -> Self {
        CheckpointSettings::auto(DecideWith::Rules).into()
    }
}

impl PromotionSettings {
    /// As the table every checkpoint shares, for what reads them all alike.
    pub fn as_checkpoint(&self) -> CheckpointSettings {
        CheckpointSettings {
            mode: self.mode,
            decide_with: self.decide_with,
            otherwise: self.otherwise,
            at_hard_limit: AtHardLimit::default(),
        }
    }
}

impl From<CheckpointSettings> for PromotionSettings {
    fn from(table: CheckpointSettings) -> Self {
        Self {
            mode: table.mode,
            decide_with: table.decide_with,
            otherwise: table.otherwise,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PromotionRules {
    pub change_classes: Vec<String>,
    /// Days between promotions or canaries of one learned workflow.
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
                "task_split",
                "task_limit",
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
    /// Your rules for the model `approvals.decides` names, in `.argonauts/`.
    pub model_rules: String,
    /// Require a model family different from every author and owner.
    pub different_provider: bool,
    /// A second approver from another vendor that must also approve.
    pub second_model: Option<WorkerRef>,
}

impl Default for ApproverSettings {
    fn default() -> Self {
        Self {
            model_rules: "approval-rules.md".to_owned(),
            different_provider: true,
            second_model: None,
        }
    }
}

/// The `[limits]` table: the hard limits, which your rules can't loosen, and beside them the run
/// and task limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HardLimitSettings {
    /// Paths no card may touch, on top of `.argonauts/**`. Glob patterns.
    pub protected_paths: Vec<String>,
    /// Packages a plan may add. The conductor adds them itself, with install scripts off.
    pub dependency_allowlist: Vec<String>,
    /// Dollars all runs and chats together may spend in a day. Unset, there is no limit.
    pub spend_per_day: Option<f64>,
    pub auto_merges_per_day: u32,
    /// Irreversible outside actions you allow, such as `publish`.
    pub irreversible_allowlist: Vec<String>,
    pub evolution_usd_per_month: f64,
    /// The run and task limits. Their keys sit in this table too, such as `limits.spend_per_run`.
    #[serde(flatten)]
    pub budget: BudgetSettings,
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
            spend_per_day: None,
            auto_merges_per_day: 3,
            irreversible_allowlist: Vec::new(),
            evolution_usd_per_month: 1000.0,
            budget: BudgetSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SandboxSettings {
    /// Whose rules guard the agents. A role's `[roles.<role>.sandbox]` can set this, `network`,
    /// `agents_see_other_worktrees`, and `agents_commit` for its own agents, as [`RoleSandbox`]
    /// says.
    pub mode: SandboxChoice,
    /// Network for commands agents run.
    pub network: bool,
    /// Whether an agent may read the other agents' copies of the repo, not only its own.
    pub agents_see_other_worktrees: bool,
    /// Whether agents commit their own work. Off, git's files stay read-only to them and
    /// Argonauts commits for them.
    pub agents_commit: bool,
    /// Refuse to start an agent's session when no sandbox works.
    pub fail_closed: bool,
    pub extra_writable: Vec<String>,
    /// Hosts sessions may reach through the Argonauts proxy, on top of their harness's own
    /// list. `host` means port 443; `host:port` names another, and `*.example.com` covers its
    /// subdomains.
    pub proxy_allow: Vec<String>,
    pub untrusted_repos: UntrustedRepos,
    /// The image container sessions run in, with the harness installed. Empty refuses them.
    pub container_image: String,
    /// Whether your own CLI hooks run in Argonauts sessions, where the CLI allows a choice.
    pub user_hooks: UserHooks,
    /// What happens to an isolated session that loaded your plugins or synced skills anyway.
    pub isolated_leak: IsolatedLeak,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            mode: SandboxChoice::Os,
            // Agents that evolve themselves need packages and docs from the internet, and the
            // sandbox still keeps them out of your other files.
            network: true,
            agents_see_other_worktrees: false,
            agents_commit: true,
            fail_closed: true,
            extra_writable: Vec::new(),
            proxy_allow: Vec::new(),
            untrusted_repos: UntrustedRepos::Container,
            container_image: String::new(),
            user_hooks: UserHooks::Inherit,
            isolated_leak: IsolatedLeak::Stop,
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
            agents_see_other_worktrees: role
                .agents_see_other_worktrees
                .unwrap_or(self.agents_see_other_worktrees),
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
    pub agents_see_other_worktrees: Option<bool>,
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

/// An isolated session, such as a blind reviewer's, must not see your plugins or what other
/// sessions did through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedLeak {
    /// Stopped before its first tool call, and its role handed to the next agent whose CLI
    /// keeps your plugins out, or to you when none does.
    Stop,
    /// It runs on, and a notice says what it loaded.
    Warn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserHooks {
    Inherit,
    /// Turned off for Argonauts sessions where the CLI allows it; `doctor` says where not.
    Disable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autopilot_has_the_lead_answer_and_is_the_most_autonomous() {
        let mut approvals = ApprovalSettings {
            answers: Some(Answerer::You),
            ..ApprovalSettings::default()
        };
        assert_eq!(approvals.answerer(ApprovalMode::Auto), Answerer::You);
        assert_eq!(approvals.answerer(ApprovalMode::Autopilot), Answerer::Main);
        approvals.answers = None;
        assert_eq!(approvals.answerer(ApprovalMode::AutoPreview), Answerer::You);

        assert_eq!(approvals.mode(), ApprovalMode::Auto);
        approvals.goal.mode = ApprovalMode::Autopilot;
        assert_eq!(
            approvals.mode(),
            ApprovalMode::Auto,
            "the others still stop at auto"
        );
        // The six a run opens decide it; the setup's answer sets them all.
        approvals.plan.mode = ApprovalMode::Autopilot;
        approvals.merge.mode = ApprovalMode::Autopilot;
        approvals.escalation.mode = ApprovalMode::Autopilot;
        approvals.budget.mode = ApprovalMode::Autopilot;
        approvals.change_request.mode = ApprovalMode::Autopilot;
        approvals.workflow_promotion.mode = ApprovalMode::Supervised;
        assert_eq!(approvals.mode(), ApprovalMode::Autopilot);
        approvals.merge.mode = ApprovalMode::Supervised;
        assert_eq!(approvals.mode(), ApprovalMode::Supervised);

        let written: toml::Value = toml::Value::try_from(&approvals).expect("written");
        assert_eq!(written["goal"]["mode"].as_str(), Some("autopilot"));
    }

    /// A picked model answers in full auto only; in supervised and auto the mode decides, as
    /// when nothing is picked, so `approvals.ask_you_about` topics stay yours there.
    #[test]
    fn a_picked_model_answers_in_autopilot_only() {
        let model: WorkerRef = "openrouter/z-ai/glm-5".parse().expect("a worker");
        let approvals = ApprovalSettings {
            answers: Some(Answerer::Model(model.clone())),
            ..ApprovalSettings::default()
        };
        assert_eq!(
            approvals.answerer(ApprovalMode::Autopilot),
            Answerer::Model(model.clone())
        );
        assert_eq!(approvals.answerer(ApprovalMode::Auto), Answerer::Main);
        assert_eq!(approvals.answerer(ApprovalMode::Supervised), Answerer::You);
        assert_eq!(approvals.answerer(ApprovalMode::AutoPreview), Answerer::You);
        assert!(Answerer::Main.answers_for_you());
        assert!(!Answerer::You.answers_for_you());
    }

    /// `you` and `lead` read in any case, so `You` never becomes a model's name.
    #[test]
    fn you_and_the_lead_read_in_any_case() {
        for (word, answerer) in [
            ("You", Answerer::You),
            (" YOU ", Answerer::You),
            ("Main", Answerer::Main),
            ("MAIN", Answerer::Main),
        ] {
            assert_eq!(Answerer::try_from(word.to_owned()), Ok(answerer), "{word}");
        }
    }

    /// `answers`, `autopilot`, and each checkpoint's `at_hard_limit` read back as written.
    #[test]
    fn autopilot_settings_round_trip() {
        let file = r#"
            autopilot = true
            answers = "openrouter/z-ai/glm-5"

            [merge]
            at_hard_limit = "open_pull_request"

            [plan]
            at_hard_limit = "park"
        "#;
        let approvals: ApprovalSettings = toml::from_str(file).expect("read");
        assert!(approvals.autopilot);
        assert_eq!(
            approvals.answers,
            Some(Answerer::Model(WorkerRef::new(
                "openrouter",
                Some("z-ai/glm-5".to_owned())
            )))
        );
        assert_eq!(approvals.merge.at_hard_limit, AtHardLimit::OpenPullRequest);
        assert_eq!(approvals.plan.at_hard_limit, AtHardLimit::Park);
        assert_eq!(approvals.goal.at_hard_limit, AtHardLimit::Stop);

        let written = toml::to_string(&approvals).expect("written");
        let again: ApprovalSettings = toml::from_str(&written).expect("read again");
        assert_eq!(again, approvals);

        for word in ["you", "main"] {
            let file = format!("answers = {word:?}");
            let read: ApprovalSettings = toml::from_str(&file).expect("read");
            let answerer = read.answers.expect("set");
            assert_eq!(answerer.to_string(), word);
            assert_eq!(answerer.model(), None);
        }
        assert!(toml::from_str::<ApprovalSettings>("answers = \"the main agent\"").is_err());
        assert!(toml::from_str::<ApprovalSettings>("[goal]\nat_hard_limit = \"ask\"").is_err());
    }

    /// Who decides when you don't, as the settings template writes it: the four keys under
    /// `[approvals]`, and a merge's rules under `[approvals.rules.merge]`.
    #[test]
    fn who_decides_when_you_dont_reads_as_the_template_writes_it() {
        let file = r#"
            answers       = "main"
            decides       = "judge"
            when_declined = "decide"
            ask_limit     = 2

            [rules.merge]
            min_test_coverage_of_changed_lines = 0.9
            max_changed_lines                  = 250
        "#;
        let approvals: ApprovalSettings = toml::from_str(file).expect("read");
        assert_eq!(approvals.answers, Some(Answerer::Main));
        assert_eq!(approvals.decides, Decides::Judge);
        assert_eq!(approvals.approver_model(), None);
        assert_eq!(approvals.when_declined, WhenDeclined::Decide);
        assert_eq!(approvals.ask_limit, 2);
        assert_eq!(
            approvals.rules.merge.min_test_coverage_of_changed_lines,
            0.9
        );
        assert_eq!(approvals.rules.merge.max_changed_lines, 250);
        // What the file leaves out keeps its default, rule by rule.
        assert!(!approvals.rules.merge.allow_debt);
        assert_eq!(approvals.rules.plan, PlanRules::default());

        // A model you name judges in the judge's place, and Esc can end the part instead.
        let named: ApprovalSettings =
            toml::from_str("decides = \"codex/gpt-5.6\"\nwhen_declined = \"stop\"").expect("read");
        let model: WorkerRef = "codex/gpt-5.6".parse().expect("a worker");
        assert_eq!(named.approver_model(), Some(&model));
        assert_eq!(named.when_declined, WhenDeclined::Stop);
        assert!(toml::from_str::<ApprovalSettings>("decides = \"the judge model\"").is_err());
    }

    /// The switch puts every checkpoint a run opens in full auto, except one you keep
    /// supervised or in shadow, and off each keeps its own.
    #[test]
    fn the_autopilot_switch_covers_every_run_checkpoint_you_dont_keep() {
        let mut approvals = ApprovalSettings {
            autopilot: true,
            ..ApprovalSettings::default()
        };
        assert_eq!(approvals.mode(), ApprovalMode::Autopilot);
        assert_eq!(approvals.answerer(approvals.mode()), Answerer::Main);
        assert_eq!(
            approvals.run_mode(ApprovalMode::Auto),
            ApprovalMode::Autopilot
        );

        approvals.merge.mode = ApprovalMode::Supervised;
        assert_eq!(
            approvals.run_mode(approvals.merge.mode),
            ApprovalMode::Supervised
        );
        assert_eq!(approvals.mode(), ApprovalMode::Supervised);
        assert_eq!(
            approvals.run_mode(ApprovalMode::AutoPreview),
            ApprovalMode::AutoPreview
        );
        approvals.autopilot = false;
        assert_eq!(approvals.run_mode(ApprovalMode::Auto), ApprovalMode::Auto);
    }
}
