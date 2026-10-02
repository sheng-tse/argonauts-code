//! Roles, `[roles.<role>]`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use super::approvals::RoleSandbox;
pub use super::handoff::{BrokenWork, FallbackEntry, FallbackRules, HandoffMode, HandoffSettings};
use super::sessions::{Compaction, NativeFanout, SessionLifetime};
use crate::names::WorkerRef;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RoleSettings {
    /// Workers that fill the role. Empty lets the router pick from the crew.
    pub workers: Vec<WorkerRef>,
    /// How many seats, where a role has several (drafters, reviewers per card).
    pub count: Option<u32>,
    /// Blind to peers in the same role (reviewers, drafters).
    pub blind: Option<bool>,
    /// Always a fresh session (judges, adjudicator, approver default to yes).
    pub fresh_session: Option<bool>,
    /// Only when asked (consultant).
    pub on_demand: bool,
    /// When the role runs, such as `reviewers_disagree` for the adjudicator.
    pub when: Option<RoleTrigger>,
    pub effort: Option<String>,
    pub lifetime: Option<SessionLifetime>,
    pub compaction: Option<Compaction>,
    pub native_fanout: Option<NativeFanout>,
    /// What happens when this role's agent stops. Unset keys come from the top-level
    /// `[handoff]` through [`HandoffSettings::applied`].
    pub handoff: HandoffSettings,
    /// The sandbox for this role's agents. Unset keys come from the top-level `[sandbox]`
    /// through [`SandboxSettings::applied`](super::approvals::SandboxSettings::applied).
    #[serde(skip_serializing_if = "RoleSandbox::is_empty")]
    pub sandbox: RoleSandbox,
    pub instructions: Option<String>,
}

/// When a conditional role runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleTrigger {
    Always,
    /// Only when a card's reviewers disagree.
    ReviewersDisagree,
    /// Only when asked (`@consultant`, or the coordinator asks).
    Asked,
}
