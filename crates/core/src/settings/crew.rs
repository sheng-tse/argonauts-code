//! The crew, `[crew]`: who fills each of the five roles, the main agent first.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::names::{ModelRef, RoleName};

/// Each list is in order of preference: the first agent that's available is the one asked, and
/// a step that wants several takes them from the front.
///
/// What isn't set follows what is: in a `[crew]` that names the orchestrator but no
/// `worker`, the workers are the orchestrator's, and in one that names a `reviewer` but no
/// `judge`, the judge is the reviewers'. An empty list is how such a role is kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "WrittenCrew")]
pub struct CrewSettings {
    /// The agent you talk to: conversation, the estimate, Captain work, and the last fallback.
    pub main: ModelRef,
    /// Plans and delegates: `"main"`, or a model of its own.
    pub orchestrator: Orchestrator,
    /// Do the tasks. Empty follows the orchestrator.
    pub worker: Vec<ModelRef>,
    /// Check a result, never one their own provider made.
    pub reviewer: Vec<ModelRef>,
    /// Decides and signs off. Empty follows the reviewers.
    pub judge: Vec<ModelRef>,
    /// The crew's agents may start workflows and background agents of their CLI's own, such as
    /// Claude Code's Workflow tool. Off, since Argonauts Code already runs the crew, and a crew
    /// inside one agent can't be watched or merged.
    #[serde(default)]
    pub worker_workflows: bool,
    /// Merges the tasks that passed: conflicts between them, a main branch that moved
    /// meanwhile, the checks after merging, and the commit message. Empty follows the
    /// orchestrator. Which files make up a change, and whether a task passed, stay with code.
    #[serde(default)]
    pub merger: Vec<ModelRef>,
}

/// A `[crew]` table as written, where a role left out is told apart from one set.
#[derive(Default, Deserialize, JsonSchema)]
#[serde(default)]
struct WrittenCrew {
    main: Option<ModelRef>,
    orchestrator: Option<Orchestrator>,
    #[serde(deserialize_with = "some_one_or_many")]
    #[schemars(with = "Vec<ModelRef>")]
    worker: Option<Vec<ModelRef>>,
    #[serde(deserialize_with = "some_one_or_many")]
    #[schemars(with = "Vec<ModelRef>")]
    reviewer: Option<Vec<ModelRef>>,
    #[serde(deserialize_with = "some_one_or_many")]
    #[schemars(with = "Vec<ModelRef>")]
    judge: Option<Vec<ModelRef>>,
    /// The crew's agents may start workflows and background agents of their CLI's own.
    worker_workflows: bool,
    #[serde(deserialize_with = "some_one_or_many")]
    #[schemars(with = "Vec<ModelRef>")]
    merger: Option<Vec<ModelRef>>,
}

impl From<WrittenCrew> for CrewSettings {
    fn from(written: WrittenCrew) -> Self {
        let everyone = CrewSettings::default();
        let follows = |set: Option<Vec<ModelRef>>, followed_set: bool, default: Vec<ModelRef>| {
            set.unwrap_or(if followed_set { Vec::new() } else { default })
        };
        let orchestrator_set = written.orchestrator.is_some();
        let reviewer_set = written.reviewer.is_some();
        Self {
            main: written.main.unwrap_or(everyone.main),
            orchestrator: written.orchestrator.unwrap_or_default(),
            worker: follows(written.worker, orchestrator_set, everyone.worker),
            reviewer: written.reviewer.unwrap_or(everyone.reviewer),
            judge: follows(written.judge, reviewer_set, everyone.judge),
            worker_workflows: written.worker_workflows,
            merger: written.merger.unwrap_or_default(),
        }
    }
}

impl Default for CrewSettings {
    fn default() -> Self {
        Self::of(
            ["claude-code", "codex", "antigravity-cli"]
                .into_iter()
                .map(|id| ModelRef::new(id, None))
                .collect(),
        )
    }
}

impl CrewSettings {
    /// A crew where everyone listed may work, review and judge, and the main agent
    /// orchestrates.
    pub fn of(everyone: Vec<ModelRef>) -> Self {
        Self {
            main: ModelRef::new("claude-code", None),
            orchestrator: Orchestrator::Main,
            worker: everyone.clone(),
            reviewer: everyone.clone(),
            judge: everyone,
            worker_workflows: false,
            merger: Vec::new(),
        }
    }

    /// Everyone a role names besides the main agent, once each, in the order the roles list
    /// them.
    pub fn members(&self) -> Vec<ModelRef> {
        let mut members: Vec<ModelRef> = Vec::new();
        let orchestrators: &[ModelRef] = match &self.orchestrator {
            Orchestrator::Main => &[],
            Orchestrator::Models(models) => models,
        };
        let lists = [
            orchestrators,
            self.worker.as_slice(),
            self.reviewer.as_slice(),
            self.judge.as_slice(),
        ];
        for worker in lists.into_iter().flatten() {
            if !members.contains(worker) {
                members.push(worker.clone());
            }
        }
        members
    }

    /// Who may fill one of the five roles, in order. The main agent stays the agent you talk
    /// to. Workers with none of their own follow the orchestrator, and a judge with none
    /// follows the reviewers.
    pub fn models_for(&self, role: CrewRole) -> Vec<ModelRef> {
        match role {
            CrewRole::Main => vec![self.main.clone()],
            CrewRole::Orchestrator => match &self.orchestrator {
                Orchestrator::Main => vec![self.main.clone()],
                Orchestrator::Models(models) => models.clone(),
            },
            CrewRole::Worker if self.worker.is_empty() => self.models_for(CrewRole::Orchestrator),
            CrewRole::Worker => self.worker.clone(),
            CrewRole::Reviewer => self.reviewer.clone(),
            CrewRole::Judge if self.judge.is_empty() => self.reviewer.clone(),
            CrewRole::Judge => self.judge.clone(),
        }
    }

    /// The role whose models `role` takes while it has none of its own.
    pub fn followed(&self, role: CrewRole) -> Option<CrewRole> {
        match role {
            CrewRole::Worker if self.worker.is_empty() => Some(CrewRole::Orchestrator),
            CrewRole::Judge if self.judge.is_empty() => Some(CrewRole::Reviewer),
            _ => None,
        }
    }

    /// Adds an agent to every list it isn't in, such as a CLI installed after setup.
    pub fn add(&mut self, worker: ModelRef) {
        for list in [&mut self.worker, &mut self.reviewer, &mut self.judge] {
            if !list.contains(&worker) {
                list.push(worker.clone());
            }
        }
    }

    /// Leaves every role to the main agent.
    pub fn clear(&mut self) {
        *self = Self {
            main: self.main.clone(),
            ..Self::of(Vec::new())
        };
    }

    pub fn is_empty(&self) -> bool {
        self.members().is_empty()
    }

    pub fn contains(&self, worker: &ModelRef) -> bool {
        self.members().contains(worker)
    }
}

impl From<Vec<ModelRef>> for CrewSettings {
    fn from(everyone: Vec<ModelRef>) -> Self {
        Self::of(everyone)
    }
}

impl FromIterator<ModelRef> for CrewSettings {
    fn from_iter<I: IntoIterator<Item = ModelRef>>(everyone: I) -> Self {
        Self::of(everyone.into_iter().collect())
    }
}

/// The crew's five roles, which a workflow's steps, `[crew]` and `[fallback.<role>]` name.
/// `Main` is the agent you talk to. Every finer role follows one of them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CrewRole {
    Main,
    Orchestrator,
    Worker,
    Reviewer,
    Judge,
}

impl CrewRole {
    pub const ALL: [CrewRole; 5] = [
        CrewRole::Main,
        CrewRole::Orchestrator,
        CrewRole::Worker,
        CrewRole::Reviewer,
        CrewRole::Judge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CrewRole::Main => "main",
            CrewRole::Orchestrator => "orchestrator",
            CrewRole::Worker => "worker",
            CrewRole::Reviewer => "reviewer",
            CrewRole::Judge => "judge",
        }
    }

    /// The role's people in a phrase or a heading, as in "all the workers".
    pub fn plural(self) -> &'static str {
        match self {
            CrewRole::Main => "main",
            CrewRole::Orchestrator => "orchestrator",
            CrewRole::Worker => "workers",
            CrewRole::Reviewer => "reviewers",
            CrewRole::Judge => "judge",
        }
    }

    /// The reverse of [`Self::as_str`].
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == name)
    }

    /// The role a fine role follows while `roles.<role>.models` is unset.
    pub fn followed_by(role: RoleName) -> Self {
        match role {
            RoleName::Main | RoleName::Narrator => CrewRole::Main,
            RoleName::Orchestrator => CrewRole::Orchestrator,
            // The tie-breaker's hat goes round the workers, as it does in a council.
            RoleName::Worker
            | RoleName::TieBreaker
            | RoleName::Draft
            | RoleName::Reader
            | RoleName::Critic
            | RoleName::Experiment
            | RoleName::Consultant => CrewRole::Worker,
            RoleName::Reviewer | RoleName::Comparer | RoleName::Map => CrewRole::Reviewer,
            RoleName::Judge | RoleName::RunReview => CrewRole::Judge,
        }
    }

    /// The fine role whose settings (effort, sandbox, fallback, instructions) an agent in this
    /// role runs under.
    pub fn fine_role(self) -> RoleName {
        match self {
            CrewRole::Main => RoleName::Main,
            CrewRole::Orchestrator => RoleName::Orchestrator,
            CrewRole::Worker => RoleName::Worker,
            CrewRole::Reviewer => RoleName::Reviewer,
            CrewRole::Judge => RoleName::Judge,
        }
    }

    /// Whether its agents change files. The others read.
    pub fn writes(self) -> bool {
        matches!(self, CrewRole::Main | CrewRole::Worker)
    }
}

impl fmt::Display for CrewRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown role {0:?}: a step's role is main, orchestrator, worker, reviewer or judge")]
pub struct CrewRoleError(pub String);

impl FromStr for CrewRole {
    type Err = CrewRoleError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        CrewRole::parse(text).ok_or_else(|| CrewRoleError(text.to_owned()))
    }
}

/// Who plans and delegates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Orchestrator {
    /// The agent you talk to.
    #[default]
    Main,
    /// In order of preference.
    Models(Vec<ModelRef>),
}

const MAIN: &str = "main";

impl JsonSchema for Orchestrator {
    fn schema_name() -> Cow<'static, str> {
        "Orchestrator".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [
                {"type": "string"},
                {"type": "array", "items": {"type": "string"}}
            ]
        })
    }
}

impl Serialize for Orchestrator {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Orchestrator::Main => serializer.serialize_str(MAIN),
            Orchestrator::Models(models) => models.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Orchestrator {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RoleModels { may_say_main: true })
    }
}

/// A role written as one model or a list of them, kept apart from a role left out.
fn some_one_or_many<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<ModelRef>>, D::Error> {
    one_or_many(deserializer).map(Some)
}

/// A role's models, written as one model or a list of them.
fn one_or_many<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<ModelRef>, D::Error> {
    let models = deserializer.deserialize_any(RoleModels {
        may_say_main: false,
    })?;
    match models {
        Orchestrator::Models(models) => Ok(models),
        Orchestrator::Main => Ok(Vec::new()),
    }
}

struct RoleModels {
    /// Only the orchestrator may say `"main"`.
    may_say_main: bool,
}

impl<'de> Visitor<'de> for RoleModels {
    type Value = Orchestrator;

    // Serde's own words for a list, which the settings checker turns into plain ones.
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sequence")
    }

    fn visit_str<E: de::Error>(self, name: &str) -> Result<Self::Value, E> {
        if self.may_say_main && name == MAIN {
            return Ok(Orchestrator::Main);
        }
        let model = name.parse::<ModelRef>().map_err(E::custom)?;
        Ok(Orchestrator::Models(vec![model]))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut models = Vec::new();
        while let Some(model) = seq.next_element::<ModelRef>()? {
            models.push(model);
        }
        Ok(Orchestrator::Models(models))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Serialize)]
    struct File {
        crew: CrewSettings,
    }

    fn worker(name: &str) -> ModelRef {
        name.parse().expect("a model")
    }

    /// Only the orchestrator and the reviewer are set.
    #[test]
    fn what_isnt_set_follows_what_is() {
        let file: File = toml::from_str(
            "[crew]\nmain = \"codex\"\norchestrator = \"claude-code/opus-5-5\"\n\
             reviewer = [\"astra\"]\n",
        )
        .expect("reads");
        let main = worker("codex");
        assert_eq!(file.crew.main, main);
        assert_eq!(
            file.crew.models_for(CrewRole::Worker),
            [worker("claude-code/opus-5-5")]
        );
        assert_eq!(file.crew.models_for(CrewRole::Judge), [worker("astra")]);
        assert_eq!(
            file.crew.models_for(CrewRole::Main),
            std::slice::from_ref(&main)
        );
        assert_eq!(
            file.crew.followed(CrewRole::Worker),
            Some(CrewRole::Orchestrator)
        );
        assert_eq!(
            file.crew.followed(CrewRole::Judge),
            Some(CrewRole::Reviewer)
        );
        assert_eq!(file.crew.followed(CrewRole::Reviewer), None);
        // Written back and read again, the roles still follow.
        let again: File = toml::from_str(&toml::to_string(&file).unwrap()).unwrap();
        assert_eq!(again.crew, file.crew);
        // Nothing set at all is everyone, as before.
        let empty: File = toml::from_str("[crew]\n").unwrap();
        assert_eq!(empty.crew, CrewSettings::default());
    }

    #[test]
    fn a_role_reads_one_model_or_a_list_and_what_is_unset_keeps_its_default() {
        let file: File = toml::from_str(
            "[crew]\norchestrator = \"main\"\nworker = [\"codex/gpt-5.6\", \"claude-code\"]\n\
             judge = \"antigravity-cli/gemini-3.6-flash\"\n",
        )
        .expect("reads");
        assert_eq!(file.crew.orchestrator, Orchestrator::Main);
        assert_eq!(
            file.crew.worker,
            [worker("codex/gpt-5.6"), worker("claude-code")]
        );
        assert_eq!(
            file.crew.judge,
            [worker("antigravity-cli/gemini-3.6-flash")]
        );
        assert_eq!(file.crew.reviewer, CrewSettings::default().reviewer);
        let written = toml::to_string(&file).expect("writes");
        assert!(written.contains("orchestrator = \"main\""), "{written}");
        assert!(
            written.contains("judge = [\"antigravity-cli/gemini-3.6-flash\"]"),
            "{written}"
        );
    }

    #[test]
    fn an_orchestrator_of_its_own_is_a_model_or_a_list() {
        let file: File = toml::from_str("[crew]\norchestrator = \"codex/gpt-5.6\"\n").expect("one");
        assert_eq!(
            file.crew.orchestrator,
            Orchestrator::Models(vec![worker("codex/gpt-5.6")])
        );
        let main = worker("claude-code");
        assert_eq!(
            file.crew.models_for(CrewRole::Orchestrator),
            [worker("codex/gpt-5.6")]
        );
        assert_eq!(
            CrewSettings::default().models_for(CrewRole::Orchestrator),
            std::slice::from_ref(&main)
        );
        assert!(toml::from_str::<File>("[crew]\nworker = [\"a b\"]\n").is_err());
    }

    #[test]
    fn members_are_everyone_a_role_names_once() {
        let crew = CrewSettings {
            main: worker("claude-code"),
            orchestrator: Orchestrator::Models(vec![worker("codex")]),
            worker: vec![worker("codex"), worker("claude-code/sonnet")],
            reviewer: vec![worker("antigravity-cli"), worker("codex")],
            judge: vec![worker("antigravity-cli")],
            worker_workflows: false,
            merger: Vec::new(),
        };
        assert_eq!(
            crew.members(),
            [
                worker("codex"),
                worker("claude-code/sonnet"),
                worker("antigravity-cli")
            ]
        );
    }

    #[test]
    fn every_fine_role_follows_one_of_the_five() {
        assert_eq!(CrewRole::followed_by(RoleName::Draft), CrewRole::Worker);
        assert_eq!(CrewRole::followed_by(RoleName::RunReview), CrewRole::Judge);
        assert_eq!(
            CrewRole::followed_by(RoleName::Reviewer),
            CrewRole::Reviewer
        );
        assert_eq!(
            CrewRole::followed_by(RoleName::TieBreaker),
            CrewRole::Worker
        );
        for role in CrewRole::ALL {
            assert_eq!(CrewRole::followed_by(role.fine_role()), role);
            assert_eq!(role.as_str().parse::<CrewRole>(), Ok(role));
        }
        assert_eq!("reviewer".parse::<CrewRole>(), Ok(CrewRole::Reviewer));
        assert!("reviewers".parse::<CrewRole>().is_err());
        assert!("workers".parse::<CrewRole>().is_err());
    }
}
