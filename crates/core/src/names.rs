//! Worker, vendor, and role names, and the short ids for sessions, checkpoints, and chats.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// `worker` or `worker/model`, such as `codex` or `openrouter/z-ai/glm-5`. The first `/` splits
/// them, because endpoint model ids often contain `/` themselves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkerRef {
    pub worker: String,
    /// `None` uses the worker's default.
    pub model: Option<String>,
}

impl WorkerRef {
    pub fn new(worker: impl Into<String>, model: Option<String>) -> Self {
        Self {
            worker: worker.into(),
            model,
        }
    }

    pub fn vendor(&self) -> VendorId {
        VendorId(self.worker.clone())
    }
}

impl fmt::Display for WorkerRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.model {
            Some(model) => write!(f, "{}/{}", self.worker, model),
            None => f.write_str(&self.worker),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid worker {0:?}: expected `worker` or `worker/model`")]
pub struct WorkerRefError(pub String);

impl FromStr for WorkerRef {
    type Err = WorkerRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (worker, model) = match s.split_once('/') {
            Some((worker, model)) => (worker, Some(model)),
            None => (s, None),
        };
        let worker_ok = !worker.is_empty()
            && worker
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        let model_ok = model.is_none_or(|m| !m.is_empty() && !m.starts_with('-'));
        if worker_ok && model_ok {
            Ok(Self::new(worker, model.map(str::to_owned)))
        } else {
            Err(WorkerRefError(s.to_owned()))
        }
    }
}

impl TryFrom<String> for WorkerRef {
    type Error = WorkerRefError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<WorkerRef> for String {
    fn from(value: WorkerRef) -> String {
        value.to_string()
    }
}

impl JsonSchema for WorkerRef {
    fn schema_name() -> Cow<'static, str> {
        "WorkerRef".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }
}

/// A vendor whose quota and health are tracked together: an adapter id or an endpoint id.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct VendorId(pub String);

impl fmt::Display for VendorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A slot in the protocol that config fills with a worker.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RoleName {
    /// The agent you talk to: chat answers, the triage estimate, solo work, and the last
    /// fallback.
    Main,
    /// Plans and delegates. The main agent unless you name another.
    Orchestrator,
    /// Rotating hat that synthesizes and breaks ties within a phase.
    PhaseLead,
    /// Restates the goal read-only. Every crew member by default.
    Restater,
    /// Compares goal restatements.
    Differ,
    /// Writes a blind plan.
    Drafter,
    /// Maps where drafts disagree, quoting them.
    Mapper,
    /// Argues against the leading plan.
    Critic,
    /// Builds a sandboxed experiment to settle a dispute.
    Spike,
    /// Scores the other plans.
    Judge,
    /// Builds one task in its own worktree.
    Worker,
    /// On-demand advice; owns nothing.
    Consultant,
    /// Reviews another owner's card and writes its acceptance tests.
    Reviewer,
    /// Fresh session that settles reviewer disagreements.
    Adjudicator,
    /// Reviews the finished run and proposes at most one process change.
    Retro,
    Approver,
    Narrator,
}

impl RoleName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Orchestrator => "orchestrator",
            Self::PhaseLead => "phase-lead",
            Self::Restater => "restater",
            Self::Differ => "differ",
            Self::Drafter => "drafter",
            Self::Mapper => "mapper",
            Self::Critic => "critic",
            Self::Spike => "spike",
            Self::Judge => "judge",
            Self::Worker => "worker",
            Self::Consultant => "consultant",
            Self::Reviewer => "reviewer",
            Self::Adjudicator => "adjudicator",
            Self::Retro => "retro",
            Self::Approver => "approver",
            Self::Narrator => "narrator",
        }
    }

    pub const ALL: [Self; 17] = [
        Self::Main,
        Self::Orchestrator,
        Self::PhaseLead,
        Self::Restater,
        Self::Differ,
        Self::Drafter,
        Self::Mapper,
        Self::Critic,
        Self::Spike,
        Self::Judge,
        Self::Worker,
        Self::Consultant,
        Self::Reviewer,
        Self::Adjudicator,
        Self::Retro,
        Self::Approver,
        Self::Narrator,
    ];

    /// The reverse of [`Self::as_str`].
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == name)
    }
}

macro_rules! short_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn numbered(n: u32) -> Self {
                Self(format!("{}-{:03}", $prefix, n))
            }

            pub fn parse(value: &str) -> Option<Self> {
                let ok = !value.is_empty()
                    && value.len() <= 64
                    && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
                ok.then(|| Self(value.to_owned()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

short_id!(
    /// Argonauts' own session key, not the vendor's session id.
    SessionKey,
    "s"
);
short_id!(CheckpointId, "cp");
short_id!(
    /// An app-level choice card that isn't part of a run.
    ChoiceId,
    "ch"
);
short_id!(TurnId, "t");
short_id!(ChatId, "chat");
short_id!(MessageId, "m");
short_id!(DecisionId, "D");
short_id!(DraftId, "dr");
short_id!(
    /// One workflow running in a run: `wf-001` for the run's own, more for nested ones.
    WorkflowId,
    "wf"
);

/// The anonymous label judges see, "Plan 1" to "Plan N".
pub type PlanLabel = u8;
