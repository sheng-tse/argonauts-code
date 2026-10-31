//! Model, agent, and role names, and the short ids for sessions, approvals, and conversations.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// `agent` or `agent/model`, such as `codex` or `openrouter/z-ai/glm-5`. The first `/` splits
/// them, because endpoint model ids often contain `/` themselves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelRef {
    pub agent: String,
    /// `None` uses the agent's default.
    pub model: Option<String>,
}

impl ModelRef {
    pub fn new(agent: impl Into<String>, model: Option<String>) -> Self {
        Self {
            agent: agent.into(),
            model,
        }
    }

    pub fn agent_id(&self) -> AgentId {
        AgentId(self.agent.clone())
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.model {
            Some(model) => write!(f, "{}/{}", self.agent, model),
            None => f.write_str(&self.agent),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid model {0:?}: expected `agent` or `agent/model`")]
pub struct ModelRefError(pub String);

impl FromStr for ModelRef {
    type Err = ModelRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (agent, model) = match s.split_once('/') {
            Some((agent, model)) => (agent, Some(model)),
            None => (s, None),
        };
        let agent_ok = !agent.is_empty()
            && agent
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        let model_ok = model.is_none_or(|m| !m.is_empty() && !m.starts_with('-'));
        if agent_ok && model_ok {
            Ok(Self::new(agent, model.map(str::to_owned)))
        } else {
            Err(ModelRefError(s.to_owned()))
        }
    }
}

impl TryFrom<String> for ModelRef {
    type Error = ModelRefError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ModelRef> for String {
    fn from(value: ModelRef) -> String {
        value.to_string()
    }
}

impl JsonSchema for ModelRef {
    fn schema_name() -> Cow<'static, str> {
        "ModelRef".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }
}

/// An agent whose plan usage and health are tracked together: an adapter id or an endpoint id.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct AgentId(pub String);

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A slot in the protocol that config fills with a model.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RoleName {
    /// The agent you talk to: conversation answers, the estimate, Captain work, and the last
    /// fallback.
    Main,
    /// Plans and delegates. The main agent unless you name another.
    Orchestrator,
    /// Rotating hat that synthesizes and breaks ties within a phase.
    TieBreaker,
    /// Reads the request back, read-only. Every crew member by default.
    Reader,
    /// Compares the readings.
    Comparer,
    /// Writes a blind plan.
    Draft,
    /// Maps where drafts disagree, quoting them.
    Map,
    /// Argues against the leading plan.
    Critic,
    /// Builds a sandboxed experiment to settle a dispute.
    Experiment,
    /// Scores the other plans, settles reviewers who disagree, and decides an approval in your
    /// place.
    Judge,
    /// Builds one task in its own worktree.
    Worker,
    /// On-demand advice; owns nothing.
    Consultant,
    /// Reviews another worker's task and writes its acceptance tests.
    Reviewer,
    /// Reviews the finished run and proposes at most one process change.
    RunReview,
    Narrator,
}

impl RoleName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Orchestrator => "orchestrator",
            Self::TieBreaker => "tie-breaker",
            Self::Reader => "reader",
            Self::Comparer => "comparer",
            Self::Draft => "draft",
            Self::Map => "map",
            Self::Critic => "critic",
            Self::Experiment => "experiment",
            Self::Judge => "judge",
            Self::Worker => "worker",
            Self::Consultant => "consultant",
            Self::Reviewer => "reviewer",
            Self::RunReview => "run-review",

            Self::Narrator => "narrator",
        }
    }

    pub const ALL: [Self; 15] = [
        Self::Main,
        Self::Orchestrator,
        Self::TieBreaker,
        Self::Reader,
        Self::Comparer,
        Self::Draft,
        Self::Map,
        Self::Critic,
        Self::Experiment,
        Self::Judge,
        Self::Worker,
        Self::Consultant,
        Self::Reviewer,
        Self::RunReview,
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
    /// Argonauts Code's own session key, not the agent's session id.
    SessionKey,
    "s"
);
short_id!(ApprovalId, "approval");
short_id!(
    /// An app-level choice question that isn't part of a run.
    ChoiceId,
    "ch"
);
short_id!(TurnId, "t");
short_id!(ConversationId, "conversation");
short_id!(MessageId, "m");
short_id!(DecisionId, "D");
short_id!(DraftId, "dr");
short_id!(
    /// One workflow running in a run: `wf-001` for the run's own, more for nested ones.
    WorkflowId,
    "wf"
);
short_id!(
    /// A seat in a council: `seat-001` to `seat-006`, in seat order. A seat is not a model: the
    /// same model can sit twice, and each seat keeps its own session from its first answer to
    /// its last task.
    SeatId,
    "seat"
);

/// The anonymous label judges see, "Plan 1" to "Plan N".
pub type PlanLabel = u8;

/// The letter one seat reads another by, as in "Seat T". Each seat has a map of its own, drawn
/// anew for every council, so a letter means nothing outside the session that reads it. Your
/// screen shows a seat's map only in the key at the head of that seat's own session, and
/// nothing records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "char", into = "char")]
pub struct SeatLetter(char);

impl SeatLetter {
    /// I, L, O, Q, U and V are left out because they read as digits or as each other. Maps are
    /// drawn from the pool in a shuffled order, so A never reads as the first seat.
    pub const POOL: [char; 20] = [
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'M', 'N', 'P', 'R', 'S', 'T', 'W', 'X',
        'Y', 'Z',
    ];

    /// `None` for a letter outside [`Self::POOL`], lowercase included.
    pub fn new(letter: char) -> Option<Self> {
        Self::POOL.contains(&letter).then_some(Self(letter))
    }

    pub fn as_char(self) -> char {
        self.0
    }
}

impl fmt::Display for SeatLetter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a seat letter: seats are lettered A to Z without I, L, O, Q, U and V")]
pub struct SeatLetterError(pub char);

impl TryFrom<char> for SeatLetter {
    type Error = SeatLetterError;
    fn try_from(letter: char) -> Result<Self, Self::Error> {
        Self::new(letter).ok_or(SeatLetterError(letter))
    }
}

impl From<SeatLetter> for char {
    fn from(letter: SeatLetter) -> char {
        letter.0
    }
}

#[cfg(test)]
mod seat_tests {
    use super::*;

    #[test]
    fn seats_are_numbered_like_the_other_short_ids() {
        assert_eq!(SeatId::numbered(2).as_str(), "seat-002");
        assert_eq!(SeatId::parse("seat-002"), Some(SeatId::numbered(2)));
        let json = serde_json::to_string(&SeatId::numbered(1)).unwrap();
        assert_eq!(json, "\"seat-001\"");
    }

    #[test]
    fn the_letter_pool_leaves_out_letters_that_read_as_others() {
        assert_eq!(SeatLetter::POOL.len(), 20);
        for left_out in ['I', 'L', 'O', 'Q', 'U', 'V', 't', '1', ' '] {
            assert_eq!(SeatLetter::new(left_out), None, "{left_out:?}");
        }
        let t = SeatLetter::new('T').unwrap();
        assert_eq!(t.to_string(), "T");
        assert_eq!(char::from(t), 'T');
    }

    #[test]
    fn a_letter_reads_back_and_one_outside_the_pool_is_refused() {
        let k = SeatLetter::new('K').unwrap();
        let json = serde_json::to_string(&k).unwrap();
        assert_eq!(json, "\"K\"");
        assert_eq!(serde_json::from_str::<SeatLetter>(&json).unwrap(), k);
        assert!(serde_json::from_str::<SeatLetter>("\"O\"").is_err());
    }
}
