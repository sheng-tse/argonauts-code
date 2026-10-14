//! The crew, `[crew]`: who fills each of the five roles. The main agent is `main_agent`, which a
//! settings file may also write as `crew.main`.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::names::{RoleName, WorkerRef};

/// Each list is in order of preference: the first agent that's available takes the seat, and a
/// step that wants several takes them from the front.
///
/// What isn't set follows what is: in a `[crew]` that names the orchestrator but
/// no workers, the workers are the orchestrator's, and in one that names reviewers but no judge,
/// the judge is the reviewers'. An empty list is how such a role is kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "WrittenCrew")]
pub struct CrewSettings {
    /// Plans and delegates: `"main"`, or a model of its own.
    pub orchestrator: Orchestrator,
    /// Do the tasks. Empty follows the orchestrator.
    pub workers: Vec<WorkerRef>,
    /// Check a result, never one their own provider made.
    pub reviewers: Vec<WorkerRef>,
    /// Decides and signs off. Empty follows the reviewers.
    pub judge: Vec<WorkerRef>,
    /// The crew's agents may start workflows and background agents of their CLI's own, such as
    /// Claude Code's Workflow tool. Off, since Argonauts already runs the crew, and a crew
    /// inside one agent can't be watched or merged.
    #[serde(default)]
    pub worker_workflows: bool,
}

/// A `[crew]` table as written, where a role left out is told apart from one set.
#[derive(Default, Deserialize, JsonSchema)]
#[serde(default)]
struct WrittenCrew {
    orchestrator: Option<Orchestrator>,
    #[serde(deserialize_with = "some_one_or_many")]
    #[schemars(with = "Vec<WorkerRef>")]
    workers: Option<Vec<WorkerRef>>,
    #[serde(deserialize_with = "some_one_or_many")]
    #[schemars(with = "Vec<WorkerRef>")]
    reviewers: Option<Vec<WorkerRef>>,
    #[serde(deserialize_with = "some_one_or_many")]
    #[schemars(with = "Vec<WorkerRef>")]
    judge: Option<Vec<WorkerRef>>,
    /// The crew's agents may start workflows and background agents of their CLI's own.
    worker_workflows: bool,
}

impl From<WrittenCrew> for CrewSettings {
    fn from(written: WrittenCrew) -> Self {
        let everyone = CrewSettings::default();
        let follows = |set: Option<Vec<WorkerRef>>, leader_set: bool, default: Vec<WorkerRef>| {
            set.unwrap_or(if leader_set { Vec::new() } else { default })
        };
        let orchestrator_set = written.orchestrator.is_some();
        let reviewers_set = written.reviewers.is_some();
        Self {
            orchestrator: written.orchestrator.unwrap_or_default(),
            workers: follows(written.workers, orchestrator_set, everyone.workers),
            reviewers: written.reviewers.unwrap_or(everyone.reviewers),
            judge: follows(written.judge, reviewers_set, everyone.judge),
            worker_workflows: written.worker_workflows,
        }
    }
}

impl Default for CrewSettings {
    fn default() -> Self {
        Self::of(
            ["claude-code", "codex", "antigravity-cli"]
                .into_iter()
                .map(|id| WorkerRef::new(id, None))
                .collect(),
        )
    }
}

impl CrewSettings {
    /// A crew where everyone listed may work, review and judge, and the main agent
    /// orchestrates.
    pub fn of(everyone: Vec<WorkerRef>) -> Self {
        Self {
            orchestrator: Orchestrator::Main,
            workers: everyone.clone(),
            reviewers: everyone.clone(),
            judge: everyone,
            worker_workflows: false,
        }
    }

    /// Everyone with a seat besides the main agent, once each, in the order the roles list
    /// them.
    pub fn members(&self) -> Vec<WorkerRef> {
        let mut members: Vec<WorkerRef> = Vec::new();
        let orchestrators: &[WorkerRef] = match &self.orchestrator {
            Orchestrator::Main => &[],
            Orchestrator::Models(models) => models,
        };
        let lists = [
            orchestrators,
            self.workers.as_slice(),
            self.reviewers.as_slice(),
            self.judge.as_slice(),
        ];
        for worker in lists.into_iter().flatten() {
            if !members.contains(worker) {
                members.push(worker.clone());
            }
        }
        members
    }

    /// Who may fill one of the five roles, in order. `main` is the agent you talk to, and
    /// stays it. Workers with none of their own follow the orchestrator, and a judge with none
    /// follows the reviewers.
    pub fn seats(&self, role: CrewRole, main: &WorkerRef) -> Vec<WorkerRef> {
        match role {
            CrewRole::Main => vec![main.clone()],
            CrewRole::Orchestrator => match &self.orchestrator {
                Orchestrator::Main => vec![main.clone()],
                Orchestrator::Models(models) => models.clone(),
            },
            CrewRole::Workers if self.workers.is_empty() => {
                self.seats(CrewRole::Orchestrator, main)
            }
            CrewRole::Workers => self.workers.clone(),
            CrewRole::Reviewers => self.reviewers.clone(),
            CrewRole::Judge if self.judge.is_empty() => self.reviewers.clone(),
            CrewRole::Judge => self.judge.clone(),
        }
    }

    /// The role whose seats `role` takes while it has none of its own.
    pub fn followed(&self, role: CrewRole) -> Option<CrewRole> {
        match role {
            CrewRole::Workers if self.workers.is_empty() => Some(CrewRole::Orchestrator),
            CrewRole::Judge if self.judge.is_empty() => Some(CrewRole::Reviewers),
            _ => None,
        }
    }

    /// Adds an agent to every list it isn't in, such as a CLI installed after setup.
    pub fn add(&mut self, worker: WorkerRef) {
        for list in [&mut self.workers, &mut self.reviewers, &mut self.judge] {
            if !list.contains(&worker) {
                list.push(worker.clone());
            }
        }
    }

    /// Leaves every seat to the main agent.
    pub fn clear(&mut self) {
        *self = Self::of(Vec::new());
    }

    pub fn is_empty(&self) -> bool {
        self.members().is_empty()
    }

    pub fn contains(&self, worker: &WorkerRef) -> bool {
        self.members().contains(worker)
    }
}

impl From<Vec<WorkerRef>> for CrewSettings {
    fn from(everyone: Vec<WorkerRef>) -> Self {
        Self::of(everyone)
    }
}

impl FromIterator<WorkerRef> for CrewSettings {
    fn from_iter<I: IntoIterator<Item = WorkerRef>>(everyone: I) -> Self {
        Self::of(everyone.into_iter().collect())
    }
}

/// The five roles a workflow's steps name.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CrewRole {
    /// The agent you talk to.
    Main,
    Orchestrator,
    Workers,
    Reviewers,
    Judge,
}

impl CrewRole {
    pub const ALL: [CrewRole; 5] = [
        CrewRole::Main,
        CrewRole::Orchestrator,
        CrewRole::Workers,
        CrewRole::Reviewers,
        CrewRole::Judge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CrewRole::Main => "main",
            CrewRole::Orchestrator => "orchestrator",
            CrewRole::Workers => "workers",
            CrewRole::Reviewers => "reviewers",
            CrewRole::Judge => "judge",
        }
    }

    /// The role a fine role follows while `roles.<role>.workers` is unset.
    pub fn followed_by(role: RoleName) -> Self {
        match role {
            RoleName::Main | RoleName::Narrator => CrewRole::Main,
            RoleName::Orchestrator => CrewRole::Orchestrator,
            // The phase lead's hat goes round the workers, as it does in a council.
            RoleName::Worker
            | RoleName::PhaseLead
            | RoleName::Drafter
            | RoleName::Restater
            | RoleName::Critic
            | RoleName::Spike
            | RoleName::Consultant => CrewRole::Workers,
            RoleName::Reviewer | RoleName::Differ | RoleName::Mapper => CrewRole::Reviewers,
            RoleName::Judge | RoleName::Adjudicator | RoleName::Approver | RoleName::Retro => {
                CrewRole::Judge
            }
        }
    }

    /// The fine role whose settings (effort, sandbox, fallback, instructions) an agent in this
    /// seat runs under.
    pub fn fine_role(self) -> RoleName {
        match self {
            CrewRole::Main => RoleName::Main,
            CrewRole::Orchestrator => RoleName::Orchestrator,
            CrewRole::Workers => RoleName::Worker,
            CrewRole::Reviewers => RoleName::Reviewer,
            CrewRole::Judge => RoleName::Judge,
        }
    }

    /// Whether its agents change files. The others read.
    pub fn writes(self) -> bool {
        matches!(self, CrewRole::Main | CrewRole::Workers)
    }
}

impl fmt::Display for CrewRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown role {0:?}: a step's role is main, orchestrator, workers, reviewers or judge")]
pub struct CrewRoleError(pub String);

impl FromStr for CrewRole {
    type Err = CrewRoleError;

    /// Takes the singular too, since a script may say `worker` where a file says `workers`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "main" => Ok(CrewRole::Main),
            "orchestrator" => Ok(CrewRole::Orchestrator),
            "workers" | "worker" => Ok(CrewRole::Workers),
            "reviewers" | "reviewer" => Ok(CrewRole::Reviewers),
            "judge" | "judges" => Ok(CrewRole::Judge),
            other => Err(CrewRoleError(other.to_owned())),
        }
    }
}

/// Who plans and delegates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Orchestrator {
    /// The agent you talk to.
    #[default]
    Main,
    /// In order of preference.
    Models(Vec<WorkerRef>),
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
        deserializer.deserialize_any(Seats {
            main_is_a_seat: true,
        })
    }
}

/// A role written as one model or a list of them, kept apart from a role left out.
fn some_one_or_many<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<WorkerRef>>, D::Error> {
    one_or_many(deserializer).map(Some)
}

/// A role's seats, written as one model or a list of them.
fn one_or_many<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<WorkerRef>, D::Error> {
    let seats = deserializer.deserialize_any(Seats {
        main_is_a_seat: false,
    })?;
    match seats {
        Orchestrator::Models(models) => Ok(models),
        Orchestrator::Main => Ok(Vec::new()),
    }
}

struct Seats {
    /// Only the orchestrator's seat may say `"main"`.
    main_is_a_seat: bool,
}

impl<'de> Visitor<'de> for Seats {
    type Value = Orchestrator;

    // Serde's own words for a list, which the settings checker turns into plain ones.
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sequence")
    }

    fn visit_str<E: de::Error>(self, name: &str) -> Result<Self::Value, E> {
        if self.main_is_a_seat && name == MAIN {
            return Ok(Orchestrator::Main);
        }
        let model = name.parse::<WorkerRef>().map_err(E::custom)?;
        Ok(Orchestrator::Models(vec![model]))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut models = Vec::new();
        while let Some(model) = seq.next_element::<WorkerRef>()? {
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

    fn worker(name: &str) -> WorkerRef {
        name.parse().expect("a worker")
    }

    /// Only the orchestrator and the reviewers are set.
    #[test]
    fn what_isnt_set_follows_what_is() {
        let file: File = toml::from_str(
            "[crew]\norchestrator = \"claude-code/opus-5-5\"\nreviewers = [\"astra\"]\n",
        )
        .expect("reads");
        let main = worker("codex");
        assert_eq!(
            file.crew.seats(CrewRole::Workers, &main),
            [worker("claude-code/opus-5-5")]
        );
        assert_eq!(file.crew.seats(CrewRole::Judge, &main), [worker("astra")]);
        assert_eq!(
            file.crew.seats(CrewRole::Main, &main),
            std::slice::from_ref(&main)
        );
        assert_eq!(
            file.crew.followed(CrewRole::Workers),
            Some(CrewRole::Orchestrator)
        );
        assert_eq!(
            file.crew.followed(CrewRole::Judge),
            Some(CrewRole::Reviewers)
        );
        assert_eq!(file.crew.followed(CrewRole::Reviewers), None);
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
            "[crew]\norchestrator = \"main\"\nworkers = [\"codex/gpt-5.6\", \"claude-code\"]\n\
             judge = \"antigravity-cli/gemini-3.6-flash\"\n",
        )
        .expect("reads");
        assert_eq!(file.crew.orchestrator, Orchestrator::Main);
        assert_eq!(
            file.crew.workers,
            [worker("codex/gpt-5.6"), worker("claude-code")]
        );
        assert_eq!(
            file.crew.judge,
            [worker("antigravity-cli/gemini-3.6-flash")]
        );
        assert_eq!(file.crew.reviewers, CrewSettings::default().reviewers);
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
            file.crew.seats(CrewRole::Orchestrator, &main),
            [worker("codex/gpt-5.6")]
        );
        assert_eq!(
            CrewSettings::default().seats(CrewRole::Orchestrator, &main),
            std::slice::from_ref(&main)
        );
        assert!(toml::from_str::<File>("[crew]\nworkers = [\"a b\"]\n").is_err());
    }

    #[test]
    fn members_are_everyone_with_a_seat_once() {
        let crew = CrewSettings {
            orchestrator: Orchestrator::Models(vec![worker("codex")]),
            workers: vec![worker("codex"), worker("claude-code/sonnet")],
            reviewers: vec![worker("antigravity-cli"), worker("codex")],
            judge: vec![worker("antigravity-cli")],
            worker_workflows: false,
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
        assert_eq!(CrewRole::followed_by(RoleName::Drafter), CrewRole::Workers);
        assert_eq!(
            CrewRole::followed_by(RoleName::Adjudicator),
            CrewRole::Judge
        );
        assert_eq!(
            CrewRole::followed_by(RoleName::Reviewer),
            CrewRole::Reviewers
        );
        assert_eq!(
            CrewRole::followed_by(RoleName::PhaseLead),
            CrewRole::Workers
        );
        for role in CrewRole::ALL {
            assert_eq!(CrewRole::followed_by(role.fine_role()), role);
            assert_eq!(role.as_str().parse::<CrewRole>(), Ok(role));
        }
        assert_eq!("reviewer".parse::<CrewRole>(), Ok(CrewRole::Reviewers));
        assert!("owner".parse::<CrewRole>().is_err());
    }
}
