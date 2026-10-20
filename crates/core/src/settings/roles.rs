//! Roles, `[roles.<role>]`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use super::approvals::RoleSandbox;
pub use super::fallback::{BrokenWork, FallbackEntry, FallbackMode, FallbackRules, FallbackTable};
pub use super::goal::RoleGoal;
use super::sessions::{Compaction, NativeFanout, SessionLifetime};
use super::shell::RoleShell;
use crate::names::ModelRef;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RoleSettings {
    /// Models that fill the role, written `agent` or `agent/model`. Empty lets the router pick
    /// from the crew.
    pub models: Vec<ModelRef>,
    /// How many places, where a role has several (drafting agents, reviewers per task).
    pub count: Option<u32>,
    /// Blind to the others in the same role (reviewers, drafting agents).
    pub blind: Option<bool>,
    /// Always a fresh session, never one carried on from an earlier task, run or main session
    /// (the judge defaults to yes). A blind role starts fresh whatever this says.
    pub fresh_session: Option<bool>,
    /// Only when asked (consultant).
    pub on_demand: bool,
    /// When the role runs, such as `asked` for the consultant.
    pub when: Option<RoleTrigger>,
    pub effort: Option<String>,
    pub lifetime: Option<SessionLifetime>,
    pub compaction: Option<Compaction>,
    pub native_fanout: Option<NativeFanout>,
    /// The sandbox for this role's agents. Unset keys come from the top-level `[sandbox]`
    /// through [`SandboxSettings::applied`](super::approvals::SandboxSettings::applied).
    #[serde(skip_serializing_if = "RoleSandbox::is_empty")]
    pub sandbox: RoleSandbox,
    pub instructions: Option<String>,
    /// Whether the role works each of its tasks as a goal, and who keeps it going: `cli`,
    /// `argonauts`, or `off`. Unset is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<RoleGoal>,
    /// What a read-only session of the role may run in a shell. Unset is
    /// [`RoleShell::default_for`] the role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<RoleShell>,
}

/// When a conditional role runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleTrigger {
    Always,
    /// Only when a task's reviewers disagree.
    ReviewersDisagree,
    /// Only when asked (`@consultant`, or the orchestrator asks).
    Asked,
}
