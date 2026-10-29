//! The ways a task may be done, `[crew.ways]` and `[council.ways]`: in one session, by a
//! workflow, or by a few agents at once. Whoever plans, the orchestrator or Council's note
//! keeper, picks one way for each task from what these allow and gives its reason. A way you
//! name in your prompt, or pick at the plan, decides that task even when it is off here.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use super::workflows::AnyWorkflow;

/// The ways one planner may pick from for each task. Every way is allowed by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Ways {
    /// The task is done in one session, by the model that takes it
    pub own_session: bool,
    /// The workflows a task may run: "any", or a list of names, any of which may be picked. An
    /// empty list keeps workflows out of every plan, and `workflows.max_depth` still caps how
    /// deep they nest
    pub workflows: WorkflowsAllowed,
    /// The most agents a task may start at once, 2 to 4, or 0 to keep agents out of every plan
    pub agents: MostAgents,
}

impl Default for Ways {
    fn default() -> Self {
        Self {
            own_session: true,
            workflows: WorkflowsAllowed::ANY,
            agents: MostAgents::MOST,
        }
    }
}

/// Written `"any"` or as a list of workflow names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum WorkflowsAllowed {
    /// Every workflow there is: the built-in ones, yours, and a CLI's own.
    Any(AnyWorkflow),
    /// Only these, by name. An empty list allows none.
    These(Vec<String>),
}

impl WorkflowsAllowed {
    pub const ANY: Self = Self::Any(AnyWorkflow::Any);

    /// Whether a task may run the workflow named `name`.
    pub fn allows(&self, name: &str) -> bool {
        match self {
            Self::Any(_) => true,
            Self::These(names) => names.iter().any(|allowed| allowed.trim() == name.trim()),
        }
    }

    /// Whether no workflow may be picked at all.
    pub fn is_off(&self) -> bool {
        matches!(self, Self::These(names) if names.is_empty())
    }
}

/// The most agents one task may start at once: 0 for none, or 2 to 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct MostAgents(u8);

impl MostAgents {
    /// No agents: the way is off.
    pub const NONE: Self = Self(0);
    /// The most a task may ever start.
    pub const MOST: Self = Self(4);

    pub fn get(self) -> u8 {
        self.0
    }

    pub fn is_off(self) -> bool {
        self == Self::NONE
    }
}

impl TryFrom<u8> for MostAgents {
    type Error = String;

    fn try_from(count: u8) -> Result<Self, Self::Error> {
        match count {
            0 | 2..=4 => Ok(Self(count)),
            _ => Err(format!(
                "agents is the most a task may start, 2 to 4, or 0 for none, not {count}"
            )),
        }
    }
}

impl From<MostAgents> for u8 {
    fn from(most: MostAgents) -> u8 {
        most.0
    }
}

impl JsonSchema for MostAgents {
    fn schema_name() -> Cow<'static, str> {
        "MostAgents".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "integer", "enum": [0, 2, 3, 4] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_way_is_allowed_by_default() {
        let ways = Ways::default();
        assert!(ways.own_session);
        assert!(ways.workflows.allows("maker-checker"));
        assert!(!ways.workflows.is_off());
        assert_eq!(ways.agents.get(), 4);
        let text = toml::to_string(&ways).unwrap();
        assert_eq!(
            text,
            "own_session = true\nworkflows = \"any\"\nagents = 4\n"
        );
        assert_eq!(toml::from_str::<Ways>(&text).unwrap(), ways);
        assert_eq!(toml::from_str::<Ways>("").unwrap(), ways);
    }

    #[test]
    fn a_list_names_the_workflows_and_an_empty_one_allows_none() {
        let ways: Ways =
            toml::from_str("workflows = [\"maker-checker\", \"best-of-n\"]\nagents = 2").unwrap();
        assert!(ways.workflows.allows("best-of-n"));
        assert!(!ways.workflows.allows("draft-and-vote"));
        assert_eq!(ways.agents.get(), 2);
        let off: Ways = toml::from_str("own_session = false\nworkflows = []\nagents = 0").unwrap();
        assert!(!off.own_session);
        assert!(off.workflows.is_off());
        assert!(!off.workflows.allows("maker-checker"));
        assert!(off.agents.is_off());
    }

    #[test]
    fn an_agent_count_outside_the_range_is_refused() {
        for count in [1, 5, 9] {
            let error = toml::from_str::<Ways>(&format!("agents = {count}")).unwrap_err();
            assert!(error.to_string().contains("2 to 4"), "{error}");
        }
        assert!(toml::from_str::<Ways>("workflows = \"all\"").is_err());
    }
}
