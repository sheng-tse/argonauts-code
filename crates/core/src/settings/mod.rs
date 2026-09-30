//! Every setting with its built-in default. Later layers ([`LAYERS`]) override it key by key,
//! but safety keys ([`SAFETY_KEYS`]) change only when you change them.

pub mod agents;
pub mod approvals;
pub mod budget;
pub mod evolution;
pub mod handoff;
pub mod protocol;
pub mod roles;
pub mod sessions;
pub mod ui;
pub mod workflows;

use std::collections::BTreeMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::names::{RoleName, WorkerRef};
use crate::workflow::WorkflowRef;

use agents::{AgentSettings, EndpointSettings, McpSettings, RegistrySettings};
use approvals::{ApprovalSettings, HardLimitSettings, SandboxSettings};
use budget::{BudgetSettings, WatchdogSettings};
use evolution::EvolutionSettings;
use handoff::HandoffSettings;
use protocol::{
    CritiqueSettings, DivideSettings, DraftingSettings, GateSettings, GoalSettings, IntakeSettings,
    JudgingSettings, MappingSettings, MergeSettings, ReviewSettings, RoutingSettings, SoloSettings,
    SpikeSettings, TriageSettings, VisibilitySettings,
};
use roles::RoleSettings;
use sessions::{
    Compaction, ConcurrencySettings, HealthSettings, NativeFanout, RunsSettings, SessionSettings,
    SupervisorSettings,
};
use ui::{NarratorSettings, NotificationSettings, UiSettings};
use workflows::WorkflowSettings;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    /// The preset: `peer-crew` (default), `coordinated-crew`, `solo`, `solo-reviewed`,
    /// `best-of-n`, `draft-and-vote`, or an `ablate-*` preset.
    pub preset: String,
    /// The workflow a run starts from, which each preset sets. With `workflows.lead_designs`
    /// the lead may pick another or compose one.
    pub workflow: WorkflowRef,
    /// Used for chat, the triage estimate, solo work, and as the last fallback.
    pub default_agent: WorkerRef,
    pub crew: Vec<WorkerRef>,
    pub agents: BTreeMap<String, AgentSettings>,
    pub endpoints: BTreeMap<String, EndpointSettings>,
    /// Per role. A role with no entry uses the defaults and the router.
    pub roles: BTreeMap<RoleName, RoleSettings>,
    /// Handoff defaults for every role. [`HandoffSettings`] says which keys the resolver copies
    /// into role tables, and [`HandoffSettings::applied`] walks the rest.
    pub handoff: HandoffSettings,
    pub intake: IntakeSettings,
    pub triage: TriageSettings,
    pub solo: SoloSettings,
    pub goal: GoalSettings,
    pub drafting: DraftingSettings,
    pub mapping: MappingSettings,
    pub critique: CritiqueSettings,
    pub spikes: SpikeSettings,
    pub judging: JudgingSettings,
    pub divide: DivideSettings,
    pub review: ReviewSettings,
    pub gates: GateSettings,
    pub routing: RoutingSettings,
    pub approvals: ApprovalSettings,
    pub limits: HardLimitSettings,
    pub sandbox: SandboxSettings,
    pub budget: BudgetSettings,
    pub watchdogs: WatchdogSettings,
    pub health: HealthSettings,
    pub sessions: SessionSettings,
    /// Compaction for every role unless the role overrides it.
    pub compaction: Compaction,
    /// Harness subagent cap for every role unless the role overrides it.
    pub native_fanout: NativeFanout,
    pub concurrency: ConcurrencySettings,
    pub visibility: VisibilitySettings,
    pub merge: MergeSettings,
    pub workflows: WorkflowSettings,
    pub evolution: EvolutionSettings,
    pub ui: UiSettings,
    pub narrator: NarratorSettings,
    pub supervisor: SupervisorSettings,
    pub runs: RunsSettings,
    pub notifications: NotificationSettings,
    pub mcp: McpSettings,
    pub registry: RegistrySettings,
}

impl Default for Settings {
    fn default() -> Self {
        let worker = |s: &str| WorkerRef::new(s, None);
        let agents = ["claude-code", "codex", "antigravity-cli", "opencode"]
            .into_iter()
            .map(|id| (id.to_owned(), AgentSettings::default()))
            .collect();
        Self {
            preset: "peer-crew".to_owned(),
            workflow: WorkflowRef::new("peer-crew"),
            default_agent: worker("claude-code"),
            crew: vec![
                worker("claude-code"),
                worker("codex"),
                worker("antigravity-cli"),
            ],
            agents,
            endpoints: BTreeMap::new(),
            roles: BTreeMap::new(),
            handoff: HandoffSettings::built_in(),
            intake: IntakeSettings::default(),
            triage: TriageSettings::default(),
            solo: SoloSettings::default(),
            goal: GoalSettings::default(),
            drafting: DraftingSettings::default(),
            mapping: MappingSettings::default(),
            critique: CritiqueSettings::default(),
            spikes: SpikeSettings::default(),
            judging: JudgingSettings::default(),
            divide: DivideSettings::default(),
            review: ReviewSettings::default(),
            gates: GateSettings::default(),
            routing: RoutingSettings::default(),
            approvals: ApprovalSettings::default(),
            limits: HardLimitSettings::default(),
            sandbox: SandboxSettings::default(),
            budget: BudgetSettings::default(),
            watchdogs: WatchdogSettings::default(),
            health: HealthSettings::default(),
            sessions: SessionSettings::default(),
            compaction: Compaction::Inherit,
            native_fanout: NativeFanout::default(),
            concurrency: ConcurrencySettings::default(),
            visibility: VisibilitySettings::default(),
            merge: MergeSettings::default(),
            workflows: WorkflowSettings::default(),
            evolution: EvolutionSettings::default(),
            ui: UiSettings::default(),
            narrator: NarratorSettings::default(),
            supervisor: SupervisorSettings::default(),
            runs: RunsSettings::default(),
            notifications: NotificationSettings::default(),
            mcp: McpSettings::default(),
            registry: RegistrySettings::default(),
        }
    }
}

/// Keys only you may set: never a preset, recipe, or self-evolution. A `--set` of one of these
/// needs an interactive terminal and is refused inside an agent session, and one in a
/// project's `.argonauts/config.toml` applies only once you trust that file. A trailing `.*`
/// covers every key below it, and `*` in the middle matches one map key.
pub const SAFETY_KEYS: &[&str] = &[
    "limits.*",
    "sandbox.*",
    "approvals.*",
    "budget.run_usd",
    "budget.run_tokens",
    "budget.run_quota",
    "budget.run_minutes",
    "budget.enabled",
    "agents.*.auth",
    "agents.*.binary",
    "agents.*.extra_args",
    "endpoints.*.base_url",
    "endpoints.*.api_key_env",
    "endpoints.*.allow_api_billing",
    "endpoints.*.header_env.*",
    "roles.*.handoff.allow_api_billing",
    "handoff.allow_api_billing",
    "mcp.*",
    "merge.*",
    "gates.*",
    "notifications.chat.*",
];

/// Keys only your global config may set. A project file that sets one is refused, trusted or
/// not, since it would name a program for Argonauts to run.
pub const GLOBAL_ONLY_KEYS: &[&str] = &["notifications.chat.*"];

/// Lowest first; a later layer wins key by key. `session` is the app's own overrides, which
/// come after the `--set` flags.
pub const LAYERS: &[&str] = &[
    "built-in",
    "global",
    "project",
    "project-local",
    "preset",
    "flag",
    "session",
];

/// Where a setting you change inside the app is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    /// Only this app session. Gone when you quit; passed to runs as overrides.
    Session,
    /// This project, just you: `.argonauts/config.local.toml`, kept out of git through
    /// `.git/info/exclude`. The default for `/model`, `/effort`, and `/agent`.
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

    /// Whether a value from `layer` still wins over one saved in this scope. An unknown layer
    /// never hides anything.
    pub fn hidden_by(self, layer: &str) -> bool {
        let rank = |name: &str| LAYERS.iter().position(|known| *known == name);
        match (rank(layer), rank(self.layer())) {
            (Some(theirs), Some(ours)) => theirs > ours,
            _ => false,
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

/// A duration written as text: `"45s"`, `"30m"`, `"2h"`, or `"none"` for no limit.
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
        let secs = match unit {
            "s" => n,
            "m" => n * 60,
            "h" => n * 3600,
            _ => return Err(format!("{text:?}: use s, m, or h")),
        };
        Ok(Some(Duration::from_secs(secs)))
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
        assert!(ProjectLocal.hidden_by("preset"));
        assert!(ProjectLocal.hidden_by("flag"));
        assert!(!ProjectLocal.hidden_by("project"));
        assert!(Global.hidden_by("session"));
        assert!(!Session.hidden_by("flag"));
    }
}
