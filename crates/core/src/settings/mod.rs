//! Every setting with its built-in default. Later layers ([`LAYERS`]) override it key by key,
//! but safety keys ([`SAFETY_KEYS`]) change only when you change them.

pub mod agents;
pub mod approvals;
pub mod asking;
pub mod awake;
pub mod behaviors;
pub mod bottom_line;
pub mod context;
pub mod council;
pub mod crew;
pub mod delivery;
pub mod endpoints;
pub mod evolution;
pub mod fallback;
pub mod goal;
pub mod issue;
pub mod layer;
pub mod learning;
pub mod limit;
pub mod limits_template;
pub mod main_session;
pub mod mods;
pub mod network;
pub mod outside;
pub mod overview;
pub mod periods;
pub mod pr;
pub mod prompts;
pub mod protocol;
pub mod roles;
pub mod saving;
pub mod sessions;
pub mod shell;
pub mod stats;
pub mod switch;
pub mod telemetry;
pub mod ui;
pub mod updates;
pub mod ways;
pub mod workflows;

use std::collections::BTreeMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::names::{ModelRef, RoleName};

use agents::{AgentSettings, EndpointSettings, McpSettings, RegistrySettings};
use approvals::{ApprovalSettings, HardLimitSettings, SandboxSettings};
use awake::AwakeSettings;
use council::CouncilSettings;
use crew::CrewSettings;
use evolution::EvolutionSettings;
use fallback::FallbackSettings;
use goal::GoalSettings;
use learning::{MemorySettings, OutcomeSettings, SkillSettings};
use limit::WatchdogSettings;
use prompts::PromptSettings;
use protocol::{
    CaptainOrCrewSettings, CaptainSettings, CheckSettings, CritiqueSettings, DivideSettings,
    DraftingSettings, ExperimentSettings, IntakeSettings, JudgingSettings, MappingSettings,
    MergeSettings, ReviewSettings, RoutingSettings, UnderstandSettings, VisibilitySettings,
};
use roles::RoleSettings;
use saving::SaveChangesTo;
use sessions::{
    Compaction, ConcurrencySettings, HealthSettings, NativeFanout, RunHostSettings, RunsSettings,
    SessionSettings,
};
use ui::{NarratorSettings, NotificationSettings, UiSettings};
use workflows::{OrchestratorDesign, WorkflowSettings, WorkflowSize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    /// How the crew is organised: `orchestrator`, `council`, `captain`, `maker-checker`,
    /// `best-of-n`, `draft-and-vote`, an `ablate-*` one, or one of your own. With no setting
    /// naming one, `captain` runs, on these built-in settings, which are `council`'s.
    /// One file holds a workflow: its steps, the settings it carries and the phases a run
    /// starts from. With `workflows.orchestrator_designs` the orchestrator sizes those phases
    /// for the task, and with `"any"` it may pick another workflow or compose one.
    pub workflow: String,
    /// Ultracode: every real request goes through a workflow, small ones included. Captain on
    /// Claude Code turns on Claude Code's own. Questions are still just answered.
    pub ultracode: bool,
    /// How big a workflow the orchestrator plans when the request doesn't say. A guideline, as
    /// in Claude Code.
    pub workflow_size: WorkflowSize,
    /// Where a change you make in the app is saved: `everywhere`, your global file; `just_you`,
    /// this project's file for you alone; `this_project`, the file your team shares; or
    /// `this_session`. Only your global file may set it
    pub save_changes_to: SaveChangesTo,
    /// Who fills the five roles. A fine role without `roles.<role>.models` follows one of them.
    pub crew: CrewSettings,
    pub agents: BTreeMap<String, AgentSettings>,
    pub endpoints: BTreeMap<String, EndpointSettings>,
    /// Per role. A role with no entry uses the defaults and the router.
    pub roles: BTreeMap<RoleName, RoleSettings>,
    /// Fallbacks: the defaults for every role, and `[fallback.<role>]` for each of the crew's
    /// five. [`FallbackTable`] says which keys the resolver copies into role tables, and
    /// [`FallbackSettings::applied_to`] walks the rest.
    pub fallback: FallbackSettings,
    pub intake: IntakeSettings,
    /// How a request is sent to Captain or to the crew.
    pub captain_or_crew: CaptainOrCrewSettings,
    pub captain: CaptainSettings,
    pub understand: UnderstandSettings,
    /// Goals: whether a request gets one by default, what checks it, and when it's stuck.
    pub goal: GoalSettings,
    pub drafting: DraftingSettings,
    pub mapping: MappingSettings,
    pub critique: CritiqueSettings,
    pub experiments: ExperimentSettings,
    pub judging: JudgingSettings,
    pub divide: DivideSettings,
    pub review: ReviewSettings,
    /// How Council's models talk until they agree, and how each of its tasks may be done.
    pub council: CouncilSettings,
    pub checks: CheckSettings,
    pub routing: RoutingSettings,
    pub approvals: ApprovalSettings,
    pub limits: HardLimitSettings,
    /// The sandbox for every role. [`Settings::sandbox_for`] lays a role's own table over it.
    pub sandbox: SandboxSettings,
    pub watchdogs: WatchdogSettings,
    pub health: HealthSettings,
    pub sessions: SessionSettings,
    pub switch: switch::SwitchSettings,
    /// Compaction for every role unless the role overrides it.
    pub compaction: Compaction,
    /// The cap on a CLI's own subagents for every role unless the role overrides it.
    pub native_fanout: NativeFanout,
    pub concurrency: ConcurrencySettings,
    pub visibility: VisibilitySettings,
    pub merge: MergeSettings,
    pub commit: delivery::CommitSettings,
    pub delivery: delivery::DeliverySettings,
    pub history: delivery::HistorySettings,
    pub workflows: WorkflowSettings,
    pub evolution: EvolutionSettings,
    pub memory: MemorySettings,
    pub skills: SkillSettings,
    pub outcomes: OutcomeSettings,
    pub prompts: PromptSettings,
    pub ui: UiSettings,
    /// What mod files may do.
    pub mods: mods::ModsSettings,
    pub narrator: NarratorSettings,
    pub run_host: RunHostSettings,
    pub runs: RunsSettings,
    pub notifications: NotificationSettings,
    pub mcp: McpSettings,
    pub registry: RegistrySettings,
    pub awake: AwakeSettings,
    pub main_session: main_session::MainSessionSettings,
    pub telemetry: telemetry::TelemetrySettings,
    pub overview: overview::OverviewSettings,
    pub periods: periods::PeriodSettings,
    /// Updating Argonauts Code itself.
    pub updates: updates::UpdateSettings,
    /// What `/issue` writes on the issue it works on.
    pub issue: issue::IssueSettings,
    /// Following the pull requests Argonauts Code opened.
    pub pr: pr::PrSettings,
    /// How long a run holds for the internet to come back.
    pub network: network::NetworkSettings,
    /// Where agents read and write outside the project, without asking or once asked, and what
    /// they never reach.
    pub outside: outside::OutsideSettings,
    /// The Behaviors map in Overview › Behaviors, and who writes its summaries.
    pub behaviors: behaviors::BehaviorSettings,
}

impl Default for Settings {
    fn default() -> Self {
        let agents = ["claude-code", "codex", "antigravity-cli", "opencode"]
            .into_iter()
            .map(|id| (id.to_owned(), AgentSettings::default()))
            .collect();
        Self {
            workflow: "council".to_owned(),
            ultracode: false,
            workflow_size: WorkflowSize::default(),
            save_changes_to: SaveChangesTo::default(),
            crew: CrewSettings::default(),
            agents,
            endpoints: BTreeMap::new(),
            roles: BTreeMap::new(),
            fallback: FallbackSettings::built_in(),
            intake: IntakeSettings::default(),
            captain_or_crew: CaptainOrCrewSettings::default(),
            captain: CaptainSettings::default(),
            understand: UnderstandSettings::default(),
            goal: GoalSettings::default(),
            drafting: DraftingSettings::default(),
            mapping: MappingSettings::default(),
            critique: CritiqueSettings::default(),
            experiments: ExperimentSettings::default(),
            judging: JudgingSettings::default(),
            divide: DivideSettings::default(),
            review: ReviewSettings::default(),
            council: CouncilSettings::default(),
            checks: CheckSettings::default(),
            routing: RoutingSettings::default(),
            approvals: ApprovalSettings::default(),
            limits: HardLimitSettings::default(),
            sandbox: SandboxSettings::default(),
            watchdogs: WatchdogSettings::default(),
            health: HealthSettings::default(),
            sessions: SessionSettings::default(),
            switch: switch::SwitchSettings::default(),
            compaction: Compaction::Inherit,
            native_fanout: NativeFanout::default(),
            concurrency: ConcurrencySettings::default(),
            visibility: VisibilitySettings::default(),
            merge: MergeSettings::default(),
            commit: delivery::CommitSettings::default(),
            delivery: delivery::DeliverySettings::default(),
            history: delivery::HistorySettings::default(),
            workflows: WorkflowSettings::default(),
            evolution: EvolutionSettings::default(),
            memory: MemorySettings::default(),
            skills: SkillSettings::default(),
            outcomes: OutcomeSettings::default(),
            prompts: PromptSettings::default(),
            ui: UiSettings::default(),
            mods: mods::ModsSettings::default(),
            narrator: NarratorSettings::default(),
            run_host: RunHostSettings::default(),
            runs: RunsSettings::default(),
            notifications: NotificationSettings::default(),
            mcp: McpSettings::default(),
            awake: AwakeSettings::default(),
            registry: RegistrySettings::default(),
            main_session: main_session::MainSessionSettings::default(),
            telemetry: telemetry::TelemetrySettings::default(),
            overview: overview::OverviewSettings::default(),
            periods: periods::PeriodSettings::default(),
            updates: updates::UpdateSettings::default(),
            issue: issue::IssueSettings::default(),
            pr: pr::PrSettings::default(),
            network: network::NetworkSettings::default(),
            outside: outside::OutsideSettings::default(),
            behaviors: behaviors::BehaviorSettings::default(),
        }
    }
}

impl Settings {
    /// The ultracode key, which `/effort`'s toggle writes.
    pub const ULTRACODE: &'static str = "ultracode";

    /// Whether the word `ultracode` in a prompt asks for a workflow.
    pub const ULTRACODE_TRIGGER: &'static str = "workflows.ultracode_trigger";

    /// Who may fill one of the five roles, in order of preference.
    pub fn models_for(&self, role: crew::CrewRole) -> Vec<ModelRef> {
        self.crew.models_for(role)
    }

    /// The sandbox for `role`'s agents, its own `[roles.<role>.sandbox]` over `[sandbox]`.
    pub fn sandbox_for(&self, role: &RoleName) -> SandboxSettings {
        let own = self.roles.get(role).map(|settings| &settings.sandbox);
        self.sandbox.applied(own)
    }

    /// How far the main agent may change a crew run's workflow. Ultracode always has it size one.
    pub fn orchestrator_designs(&self) -> OrchestratorDesign {
        let design = self.workflows.orchestrator_designs.design();
        if self.ultracode {
            design.max(OrchestratorDesign::Size)
        } else {
            design
        }
    }
}

/// Keys only you may set: never a workflow, a learned workflow, or the evolution loop. A `--set`
/// of one of these needs an interactive terminal and is refused inside an agent session, and one in
/// a project's `.argonauts/config.toml` applies only once you trust that file. A trailing `.*`
/// covers every key below it, and `*` in the middle matches one map key.
pub const SAFETY_KEYS: &[&str] = &[
    "save_changes_to",
    "limits.protected_paths",
    "limits.dependency_allowlist",
    "limits.spend_per_day",
    "limits.auto_merges_per_day",
    "limits.irreversible_allowlist",
    "limits.evolution_usd_per_month",
    "telemetry.*",
    "updates.*",
    "sandbox.*",
    "outside.*",
    "approvals.*",
    "evolution.*",
    "memory.*",
    "limits.spend_per_run",
    "limits.run_tokens",
    "limits.run_plan_usage",
    "limits.run_minutes",
    "limits.plan_usage",
    "captain.kind",
    "captain.asks",
    "captain_or_crew.captain_kind",
    "agents.*.auth",
    "agents.*.binary",
    "agents.*.extra_args",
    "endpoints.*.base_url",
    "endpoints.*.api_key_env",
    "endpoints.*.allow_api_billing",
    "endpoints.*.header_env.*",
    "fallback.*.allow_api_billing",
    "roles.*.sandbox.*",
    "fallback.allow_api_billing",
    "mcp.*",
    "merge.*",
    "checks.*",
    "mods.*",
    "notifications.conversation.*",
    "captain.kind",
    "captain_or_crew.captain_kind",
];

/// Keys only your global config may set. A project file that sets one is refused, trusted or
/// not, since it would name a program for Argonauts Code to run, decide which build of it runs,
/// or send your changes into a repository's shared file.
pub const GLOBAL_ONLY_KEYS: &[&str] = &[
    "notifications.conversation.*",
    "telemetry.*",
    "updates.*",
    "save_changes_to",
];

/// Why global-only `key` can't be set in a project file, read on from "can only be set in
/// your global config, since".
pub fn global_only_because(key: &str) -> &'static str {
    match key.split('.').next() {
        Some("save_changes_to") => {
            "it says where your changes save, and a repository you clone must not send them \
             into its shared file"
        }
        Some("updates") => "it decides which build of Argonauts Code runs",
        Some("telemetry") => "it decides what Argonauts Code reports about your use",
        _ => "it names a program for Argonauts Code to run",
    }
}

/// Global-only keys a project file may still set to one value, as TOML text, since that value
/// only takes something away: a project may turn usage reports off, never on.
pub const PROJECT_MAY_TIGHTEN: &[(&str, &str)] = &[("telemetry.enabled", "false")];

/// Whether a project file may set global-only `key` to `toml_value`.
pub fn project_may_tighten(key: &str, toml_value: &str) -> bool {
    PROJECT_MAY_TIGHTEN
        .iter()
        .any(|(tightened, value)| *tightened == key && toml_value.trim() == *value)
}

/// Lowest first; a later layer wins key by key. `session` is the app's own overrides, which
/// come after the `--set` flags. `request` holds what a request said about how to run it, for
/// that run only.
pub const LAYERS: &[&str] = &[
    "built-in",
    "global",
    "project",
    "project-local",
    "workflow",
    "flag",
    "session",
    "request",
];

/// The layers that are settings files, lowest first. A workflow picked in one sits just below it:
/// what that file and later layers set wins over the workflow.
pub const FILE_LAYERS: [&str; 3] = ["global", "project", "project-local"];

/// Where a setting you change inside the app is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    /// Only this app session. Gone when you quit; passed to runs as overrides.
    Session,
    /// This project, just you: `.argonauts/config.local.toml`, kept out of git through
    /// `.git/info/exclude`.
    ProjectLocal,
    /// This project, shared: `.argonauts/config.toml`, which a team commits.
    Project,
    /// Everywhere, for you: `~/.argonauts/config.toml`.
    Global,
}

impl SettingScope {
    pub fn layer(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::ProjectLocal => "project-local",
            Self::Project => "project",
            Self::Global => "global",
        }
    }

    /// The file scope a settings layer names; `None` for a layer that isn't a file.
    pub fn from_layer(layer: &str) -> Option<Self> {
        match layer {
            "project-local" => Some(Self::ProjectLocal),
            "project" => Some(Self::Project),
            "global" => Some(Self::Global),
            "session" => Some(Self::Session),
            _ => None,
        }
    }

    /// Where a change to a value now from `layer` is saved, so it takes effect and never hides
    /// under a layer that wins: the file it came from, and everywhere for a default. A value
    /// from the workflow goes to the file that picked the workflow, `workflow_from`. One from a
    /// flag, a request, or a workflow no file picked can only be outdone by this session.
    pub fn for_change(layer: &str, workflow_from: Option<&str>) -> Self {
        match layer {
            "built-in" => Self::Global,
            "workflow" => workflow_from
                .and_then(Self::from_layer)
                .unwrap_or(Self::Session),
            "flag" | "request" => Self::Session,
            other => Self::from_layer(other).unwrap_or(Self::Global),
        }
    }

    /// Whether a value from `layer` still wins over one saved in this scope. An unknown layer
    /// never hides anything.
    pub fn hidden_by(self, layer: &str) -> bool {
        let rank = |name: &str| LAYERS.iter().position(|known| *known == name);
        match (rank(layer), rank(self.layer())) {
            (Some(theirs), Some(ours)) => theirs > ours,
            _ => false,
        }
    }

    /// Like [`hidden_by`](Self::hidden_by) for a value in effect. One from the workflow counts
    /// from `workflow_from`, the settings file that picked the workflow when one did, since what
    /// you set there or in a later layer wins over the workflow.
    pub fn hidden_by_value(self, layer: &str, workflow_from: Option<&str>) -> bool {
        match workflow_from {
            Some(from) if layer == "workflow" => self.hidden_by(from),
            _ => self.hidden_by(layer),
        }
    }
}

/// What `/config` and the pickers know about one leaf key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingMeta {
    /// Dotted key, such as `approvals.merge.mode`. Map keys appear as `*`.
    pub key: String,
    pub description: String,
    /// The values a picker offers, as TOML text. Empty when any value of the type goes.
    pub allowed: Vec<String>,
    pub default_scope: SettingScope,
    /// A safety key: changing it asks you to confirm, and agents never can.
    pub safety: bool,
}

/// A duration written as text: `"150ms"`, `"45s"`, `"30m"`, `"2h"`, or `"none"` for no limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Span(pub String);

impl Span {
    pub fn new(text: &str) -> Self {
        Self(text.to_owned())
    }

    pub fn parse(&self) -> Result<Option<Duration>, String> {
        let text = self.0.trim();
        if text == "none" {
            return Ok(None);
        }
        let split = text
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| format!("{text:?} needs a unit: s, m, or h"))?;
        let (number, unit) = text.split_at(split);
        let n: u64 = number
            .parse()
            .map_err(|_| format!("{text:?} isn't a duration"))?;
        let duration = match unit {
            "ms" => Duration::from_millis(n),
            "s" => Duration::from_secs(n),
            "m" => Duration::from_secs(n * 60),
            "h" => Duration::from_secs(n * 3600),
            _ => return Err(format!("{text:?}: use ms, s, m, or h")),
        };
        Ok(Some(duration))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_know_which_layers_hide_them() {
        use SettingScope::*;
        assert_eq!(
            [Session, ProjectLocal, Project, Global].map(SettingScope::layer),
            ["session", "project-local", "project", "global"]
        );
        for scope in [Session, ProjectLocal, Project, Global] {
            assert!(LAYERS.contains(&scope.layer()));
            assert!(!scope.hidden_by("built-in"));
            assert!(!scope.hidden_by(scope.layer()), "{scope:?}");
            assert!(!scope.hidden_by("nowhere"));
        }
        assert!(Global.hidden_by("project"));
        assert!(Project.hidden_by("project-local"));
        assert!(ProjectLocal.hidden_by("workflow"));
        assert!(ProjectLocal.hidden_by("flag"));
        assert!(!ProjectLocal.hidden_by("project"));
        assert!(Global.hidden_by("session"));
        assert!(!Session.hidden_by("flag"));
    }

    #[test]
    fn a_change_is_saved_where_its_value_came_from() {
        use SettingScope::*;
        assert_eq!(SettingScope::for_change("project", None), Project);
        assert_eq!(
            SettingScope::for_change("project-local", None),
            ProjectLocal
        );
        assert_eq!(SettingScope::for_change("global", None), Global);
        assert_eq!(SettingScope::for_change("session", None), Session);
        assert_eq!(SettingScope::for_change("built-in", None), Global);
        assert_eq!(
            SettingScope::for_change("workflow", Some("project")),
            Project
        );
        assert_eq!(SettingScope::for_change("workflow", None), Session);
        assert_eq!(SettingScope::for_change("flag", None), Session);
        // Wherever it goes, nothing above it hides the change. A request's own words hold for
        // its run alone, so they never hide what you save.
        for layer in LAYERS.iter().filter(|layer| **layer != "request") {
            let scope = SettingScope::for_change(layer, None);
            assert!(!scope.hidden_by(layer), "{layer}");
        }
    }

    #[test]
    fn a_workflow_picked_in_a_file_hides_only_the_files_before_it() {
        use SettingScope::*;
        assert!(Global.hidden_by_value("workflow", Some("project")));
        assert!(!Project.hidden_by_value("workflow", Some("project")));
        assert!(!ProjectLocal.hidden_by_value("workflow", Some("project")));
        assert!(!Global.hidden_by_value("workflow", Some("global")));
        // Picked by default or by a flag, the workflow sits above every file.
        assert!(ProjectLocal.hidden_by_value("workflow", None));
        assert!(!Session.hidden_by_value("workflow", None));
        // Other layers ignore where the workflow was picked.
        assert!(Global.hidden_by_value("project", Some("global")));
    }
}
