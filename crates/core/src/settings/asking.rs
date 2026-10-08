//! What becomes of a question nobody answers: the two `[approvals]` keys
//! that keep a run from waiting forever on one.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::names::WorkerRef;

/// What Esc on a question or an approval does. Esc means "carry on without me", so nothing
/// waits either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WhenDeclined {
    /// The agent that asked, or the main agent, decides and carries on, and what it assumed is
    /// recorded for you to review. An approval goes to your rules and the judge.
    #[default]
    Decide,
    /// That part of the work ends, with its work kept, and the closing answer says why.
    Stop,
}

/// How many times an agent that ends its turn with a question is told to decide and carry on,
/// before it is told to take the option it recommended.
pub const DEFAULT_ASK_LIMIT: u32 = 2;

/// Who settles what your rules can't decide. Written as `judge`, or a model such as
/// `antigravity-cli/gemini-3.6-flash`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Decides {
    /// The judge: the model picked to check the work, from another provider than its authors.
    #[default]
    Judge,
    /// A model you named, which approves in the judge's place.
    Model(WorkerRef),
}

impl Decides {
    /// The word the setting takes in a model's place.
    pub const JUDGE: &'static str = "judge";

    /// The model you named, when you named one.
    pub fn model(&self) -> Option<&WorkerRef> {
        match self {
            Decides::Judge => None,
            Decides::Model(model) => Some(model),
        }
    }
}

impl fmt::Display for Decides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Decides::Judge => f.write_str(Self::JUDGE),
            Decides::Model(model) => fmt::Display::fmt(model, f),
        }
    }
}

impl TryFrom<String> for Decides {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value = value.trim();
        if value.eq_ignore_ascii_case(Self::JUDGE) {
            return Ok(Decides::Judge);
        }
        value.parse().map(Decides::Model).map_err(|_| {
            format!("expected `judge`, or a model such as `codex/gpt-5.6`, not {value:?}")
        })
    }
}

impl From<WorkerRef> for Decides {
    fn from(model: WorkerRef) -> Self {
        Decides::Model(model)
    }
}

impl From<Decides> for String {
    fn from(value: Decides) -> String {
        value.to_string()
    }
}

impl JsonSchema for Decides {
    fn schema_name() -> Cow<'static, str> {
        "Decides".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decides_reads_the_judge_or_a_model() {
        let read = |word: &str| Decides::try_from(word.to_owned());
        assert_eq!(read("judge"), Ok(Decides::Judge));
        assert_eq!(read(" Judge "), Ok(Decides::Judge));
        let model: WorkerRef = "codex/gpt-5.6".parse().unwrap();
        assert_eq!(read("codex/gpt-5.6"), Ok(Decides::Model(model.clone())));
        assert_eq!(String::from(Decides::Model(model)), "codex/gpt-5.6");
        assert_eq!(String::from(Decides::Judge), "judge");
        assert!(read("the judge model").is_err());
    }
}
