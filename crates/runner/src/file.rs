//! A workflow as a file of steps: `[[step]]` entries that each name a role. The same file may
//! carry settings below its steps, which the settings layer reads and this reader leaves alone.

use std::collections::BTreeSet;
use std::fmt;

use argonauts_core::names::ModelRef;
use argonauts_core::settings::crew::CrewRole;
use serde::{Deserialize, Deserializer, Serialize};
use toml::{Table, Value};

pub use argonauts_core::settings::workflows::WORKFLOW_FILE_KEYS as WORKFLOW_KEYS;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowFile {
    pub name: String,
    /// One line on what it's for, shown in the `/` menu and the list.
    pub about: String,
    /// True lets the orchestrator write the steps for each request, keeping who plans, builds,
    /// reviews and signs off as the steps say. False runs the steps exactly as written.
    pub adapt: bool,
    /// Go round the whole workflow until this holds.
    pub goal: Option<String>,
    /// The most rounds a `goal` gets. Without it only the goal, or you, ends the rounds.
    pub max_rounds: Option<u32>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    pub role: CrewRole,
    /// What its agents are asked, with `{request}` and `{task}` filled in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub does: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_each: Option<ForEach>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Count>,
    /// Its agents don't see what the steps before them made.
    #[serde(default, skip_serializing_if = "is_false")]
    pub independent: bool,
    /// Each task goes to an agent that didn't build it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub not_the_author: bool,
    /// Where the work goes when this step doesn't accept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_back_to: Option<String>,
    /// The step that runs next, in place of the one below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<String>,
    /// A goal on this step: its agent keeps going until this is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// The most times the step runs for one task. Without it only acceptance ends a loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    /// A model of its own, in place of the role's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// Who keeps an `until`: the CLI's own goal, or Argonauts Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_by: Option<KeptBy>,
    /// Its workers may each run a workflow of their own, where their CLI has one.
    #[serde(default, skip_serializing_if = "is_false")]
    pub own_workflow: bool,
    /// A step you require in every workflow, added to this one. Never written in a file.
    #[serde(skip_deserializing, skip_serializing_if = "is_false")]
    pub required: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForEach {
    /// One agent per task, side by side, each writer in its own worktree.
    Task,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptBy {
    Cli,
    Argonauts,
}

/// How many agents a step starts: a number, or `"all"` for every model of its role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    Fixed(u32),
    All,
}

impl Serialize for Count {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Count::Fixed(count) => serializer.serialize_u32(*count),
            Count::All => serializer.serialize_str("all"),
        }
    }
}

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Number(u32),
            Word(String),
        }
        match Written::deserialize(deserializer) {
            Ok(Written::Number(count)) => Ok(Count::Fixed(count)),
            Ok(Written::Word(word)) if word == "all" => Ok(Count::All),
            _ => Err(serde::de::Error::custom("count takes a number or \"all\"")),
        }
    }
}

/// Something wrong with a workflow file, in words for the person who wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The step it's about, from 0, when it's about one.
    pub step: Option<usize>,
    pub message: String,
}

impl Problem {
    fn file(message: impl Into<String>) -> Self {
        Self {
            step: None,
            message: message.into(),
        }
    }

    fn step(step: usize, message: impl Into<String>) -> Self {
        Self {
            step: Some(step),
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    #[error("{0}")]
    Toml(String),
    #[error("{}", lines(.0))]
    Problems(Vec<Problem>),
}

fn lines(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|problem| problem.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

const STEP_KEYS: &str = "name, role, does, for_each, count, independent, not_the_author, \
                         send_back_to, then, until, max_rounds, model, kept_by, own_workflow";

impl WorkflowFile {
    /// Reads a workflow file. `Ok(None)` is a file with no steps: it only carries settings.
    pub fn parse(text: &str) -> Result<Option<Self>, FileError> {
        let table: Table = text
            .parse()
            .map_err(|err: toml::de::Error| FileError::Toml(err.to_string()))?;
        Self::from_table(&table)
    }

    pub fn from_table(table: &Table) -> Result<Option<Self>, FileError> {
        let Some(written) = table.get("step") else {
            return Ok(None);
        };
        let Value::Array(written) = written else {
            return Err(FileError::Problems(vec![Problem::file(
                "steps are written as [[step]] tables, one per step",
            )]));
        };
        let mut problems = Vec::new();
        let mut steps = Vec::with_capacity(written.len());
        for (index, value) in written.iter().enumerate() {
            match value.clone().try_into::<Step>() {
                Ok(step) => steps.push(step),
                Err(err) => problems.push(Problem::step(index, step_error(index, value, &err))),
            }
        }
        let text = |key: &str| match table.get(key) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            // A `[goal]` table holds the goal's settings, which a workflow file written from a
            // built-in carries, and the settings read; it sets no goal of the workflow's own.
            Some(Value::Table(_)) if key == "goal" => Ok(None),
            Some(other) => Err(Problem::file(format!(
                "{key} can't be {other}; it takes text in quotes"
            ))),
        };
        let mut read = |key: &str| {
            text(key).unwrap_or_else(|problem| {
                problems.push(problem);
                None
            })
        };
        let name = read("name").unwrap_or_default();
        let about = read("about").unwrap_or_default();
        let goal = read("goal");
        let adapt = match table.get("adapt") {
            None => false,
            Some(Value::Boolean(adapt)) => *adapt,
            Some(other) => {
                problems.push(Problem::file(format!(
                    "adapt can't be {other}; it takes true or false"
                )));
                false
            }
        };
        let max_rounds = match table.get("max_rounds") {
            None => None,
            Some(Value::Integer(rounds)) if *rounds >= 1 => u32::try_from(*rounds).ok(),
            Some(other) => {
                problems.push(Problem::file(format!(
                    "max_rounds can't be {other}; it takes a whole number, 1 or more"
                )));
                None
            }
        };
        if !problems.is_empty() {
            return Err(FileError::Problems(problems));
        }
        let file = Self {
            name,
            about,
            adapt,
            goal,
            max_rounds,
            steps,
        };
        let problems = file.problems();
        if problems.is_empty() {
            Ok(Some(file))
        } else {
            Err(FileError::Problems(problems))
        }
    }

    /// Everything that would stop the steps from running as written.
    pub fn problems(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        if self.steps.is_empty() {
            problems.push(Problem::file("a workflow needs at least one [[step]]"));
        }
        let mut seen = BTreeSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            let place = format!("step {} ({})", index + 1, step.name);
            if step.name.trim().is_empty() {
                problems.push(Problem::step(
                    index,
                    format!("step {} needs a name", index + 1),
                ));
            } else if !seen.insert(step.name.as_str()) {
                problems.push(Problem::step(
                    index,
                    format!(
                        "two steps are named {}; a step's name is how others point at it",
                        step.name
                    ),
                ));
            }
            if step.count == Some(Count::Fixed(0)) {
                problems.push(Problem::step(
                    index,
                    format!("{place}: count is 1 or more, or \"all\""),
                ));
            }
            if step.max_rounds == Some(0) {
                problems.push(Problem::step(
                    index,
                    format!("{place}: max_rounds is 1 or more"),
                ));
            }
            for (key, target) in [("send_back_to", &step.send_back_to), ("then", &step.then)] {
                let Some(target) = target else { continue };
                if *target == step.name {
                    problems.push(Problem::step(
                        index,
                        format!("{place}: {key} names the step itself"),
                    ));
                } else if self.index_of(target).is_none() {
                    problems.push(Problem::step(
                        index,
                        format!("{place}: {key} names {target}, and no step is called that"),
                    ));
                }
            }
            if step.not_the_author && !self.has_an_author_before(index) {
                problems.push(Problem::step(
                    index,
                    format!(
                        "{place}: not_the_author needs a step before it where workers build each \
                         task (for_each = \"task\")"
                    ),
                ));
            }
            if step.own_workflow && step.role != CrewRole::Worker {
                problems.push(Problem::step(
                    index,
                    format!(
                        "{place}: own_workflow lets a worker run a workflow of its own, and this \
                         step's role is {}",
                        step.role
                    ),
                ));
            }
            if step.kept_by.is_some() && step.until.is_none() {
                problems.push(Problem::step(
                    index,
                    format!("{place}: kept_by says who keeps a goal, and this step has no until"),
                ));
            }
            if let Some(problem) = self.endless(index) {
                problems.push(Problem::step(index, format!("{place}: {problem}")));
            }
        }
        problems
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.steps.iter().position(|step| step.name == name)
    }

    /// Whether a step runs once per task. A step says so with `for_each`, and the worker and
    /// reviewer steps after it stay per task until a judge or the orchestrator looks at the
    /// whole.
    pub fn per_task(&self, index: usize) -> bool {
        let mut per_task = false;
        for step in self.steps.iter().take(index + 1) {
            per_task = step.for_each == Some(ForEach::Task)
                || (per_task && matches!(step.role, CrewRole::Worker | CrewRole::Reviewer));
        }
        per_task
    }

    /// The step that lists the tasks: the last one before the first per-task step. `None` when
    /// the file starts per task, where the request is the one task.
    pub fn planner(&self) -> Option<usize> {
        let first = (0..self.steps.len()).find(|index| self.per_task(*index))?;
        first.checked_sub(1)
    }

    /// A step written below the one that sends work back to it runs only when work is sent
    /// back, never on the way down.
    pub fn only_when_sent_back(&self, index: usize) -> bool {
        let name = &self.steps[index].name;
        self.steps
            .iter()
            .take(index)
            .any(|step| step.send_back_to.as_ref() == Some(name))
    }

    /// The roles its steps name, which a script the orchestrator writes for it must keep to.
    pub fn roles(&self) -> BTreeSet<CrewRole> {
        self.steps.iter().map(|step| step.role).collect()
    }

    fn has_an_author_before(&self, index: usize) -> bool {
        (0..index).any(|earlier| self.per_task(earlier) && self.steps[earlier].role.writes())
    }

    /// A `then` that goes back up must be able to stop. It can when a step in the loop has
    /// `max_rounds`, or when the `then` step itself runs only on a send-back, so an accepting
    /// reviewer skips it. A goal that holds, or a reviewer that accepts above a step that always
    /// runs, lets the loop go on.
    fn endless(&self, index: usize) -> Option<String> {
        let step = &self.steps[index];
        let target = self.index_of(step.then.as_ref()?)?;
        if target > index {
            return None;
        }
        let can_end = self.only_when_sent_back(index)
            || self.steps[target..=index]
                .iter()
                .any(|inside| inside.max_rounds.is_some() && inside.until.is_none());
        (!can_end).then(|| {
            format!(
                "then = \"{}\" goes round forever: give a step in the loop max_rounds",
                self.steps[target].name
            )
        })
    }
}

/// What serde said about one step, as a sentence that names the step.
fn step_error(index: usize, written: &Value, err: &toml::de::Error) -> String {
    let name = written
        .get("name")
        .and_then(Value::as_str)
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    let message = err.message();
    let said = if let Some(field) = message
        .strip_prefix("unknown field `")
        .and_then(|rest| rest.split('`').next())
    {
        format!("there's no key named {field}; a step takes {STEP_KEYS}")
    } else if let Some(field) = message
        .strip_prefix("missing field `")
        .and_then(|rest| rest.split('`').next())
    {
        format!("it needs a {field}")
    } else {
        message.to_owned()
    };
    format!("step {}{name}: {said}", index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> WorkflowFile {
        WorkflowFile::parse(text)
            .unwrap_or_else(|err| panic!("{err}"))
            .expect("a file with steps")
    }

    fn problems(text: &str) -> String {
        match WorkflowFile::parse(text) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("the file read without a problem"),
        }
    }

    /// A file written from a built-in carries the goal's settings as a `[goal]` table, empty
    /// when every key in it is commented out; that sets no goal of the workflow's own.
    #[test]
    fn a_goal_settings_table_is_no_goal_text() {
        for table in ["[goal]\n", "[goal]\ncheck = \"auto\"\n"] {
            let read = file(&format!(
                "name = \"mine\"\n\n[[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\n\n{table}"
            ));
            assert_eq!(read.goal, None, "{table}");
        }
        assert!(
            WorkflowFile::parse("goal = 3\n[[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\n")
                .is_err()
        );
    }

    #[test]
    fn a_file_without_steps_only_carries_settings() {
        assert_eq!(WorkflowFile::parse("[review]\nper_task = 2\n"), Ok(None));
    }

    #[test]
    fn steps_read_with_their_keys_and_other_tables_are_left_to_the_settings() {
        let read = file(
            "name = \"mine\"\nabout = \"Mine.\"\ngoal = \"every test passes\"\n\n\
             [[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\n\n\
             [[step]]\nname = \"Build\"\nrole = \"worker\"\nfor_each = \"task\"\ncount = \"all\"\n\
             model = \"codex/gpt-5.6\"\nuntil = \"its tests pass\"\nkept_by = \"cli\"\n\n\
             [review]\nper_task = 2\n",
        );
        assert_eq!(read.name, "mine");
        assert!(!read.adapt);
        assert_eq!(read.goal.as_deref(), Some("every test passes"));
        let build = &read.steps[1];
        assert_eq!(build.role, CrewRole::Worker);
        assert_eq!(build.count, Some(Count::All));
        assert_eq!(build.kept_by, Some(KeptBy::Cli));
        assert_eq!(
            build.model.as_ref().map(ToString::to_string).as_deref(),
            Some("codex/gpt-5.6")
        );
        assert_eq!(read.planner(), Some(0));
    }

    #[test]
    fn per_task_carries_on_through_workers_and_reviewers_until_a_judge() {
        let read = file(
            "[[step]]\nname = \"Spec\"\nrole = \"orchestrator\"\n\
             [[step]]\nname = \"Build\"\nrole = \"worker\"\nfor_each = \"task\"\n\
             [[step]]\nname = \"Review\"\nrole = \"reviewer\"\nsend_back_to = \"Revise\"\n\
             [[step]]\nname = \"Revise\"\nrole = \"worker\"\nthen = \"Review\"\n\
             [[step]]\nname = \"Sign off\"\nrole = \"judge\"\n",
        );
        let per_task: Vec<bool> = (0..5).map(|index| read.per_task(index)).collect();
        assert_eq!(per_task, [false, true, true, true, false]);
        assert!(read.only_when_sent_back(3));
        assert!(!read.only_when_sent_back(1));
    }

    #[test]
    fn a_wrong_file_says_which_step_and_what_it_takes() {
        let unknown =
            problems("[[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\nsendback = \"x\"\n");
        assert!(
            unknown.starts_with("step 1 (Plan): there's no key named sendback"),
            "{unknown}"
        );
        let role = problems("[[step]]\nname = \"Plan\"\nrole = \"builder\"\n");
        assert!(
            role.contains("step 1 (Plan)") && role.contains("builder"),
            "{role}"
        );
        let missing = problems("[[step]]\nname = \"Plan\"\n");
        assert_eq!(missing, "step 1 (Plan): it needs a role");
        let target = problems(
            "[[step]]\nname = \"Build\"\nrole = \"worker\"\n\
             [[step]]\nname = \"Review\"\nrole = \"reviewer\"\nsend_back_to = \"Biuld\"\n",
        );
        assert_eq!(
            target,
            "step 2 (Review): send_back_to names Biuld, and no step is called that"
        );
        let twice = problems(
            "[[step]]\nname = \"Build\"\nrole = \"worker\"\n[[step]]\nname = \"Build\"\nrole = \"judge\"\n",
        );
        assert!(twice.contains("two steps are named Build"), "{twice}");
    }

    #[test]
    fn a_loop_nothing_can_end_is_refused() {
        let endless = problems(
            "[[step]]\nname = \"Build\"\nrole = \"worker\"\n\
             [[step]]\nname = \"Polish\"\nrole = \"worker\"\nthen = \"Build\"\n",
        );
        assert!(endless.contains("goes round forever"), "{endless}");
        file(
            "[[step]]\nname = \"Build\"\nrole = \"worker\"\n\
             [[step]]\nname = \"Polish\"\nrole = \"worker\"\nthen = \"Build\"\nmax_rounds = 3\n",
        );
        // A reviewer that accepts lets the loop go on, so sending back doesn't end it.
        let reviewed = problems(
            "[[step]]\nname = \"Build\"\nrole = \"worker\"\n\
             [[step]]\nname = \"Review\"\nrole = \"reviewer\"\nsend_back_to = \"Build\"\n\
             [[step]]\nname = \"Polish\"\nrole = \"worker\"\nthen = \"Build\"\n",
        );
        assert!(reviewed.contains("goes round forever"), "{reviewed}");
        // Nor does a goal that holds: its max_rounds caps its tries, not the loop.
        let goal = problems(
            "[[step]]\nname = \"Build\"\nrole = \"worker\"\nuntil = \"tests pass\"\nmax_rounds = 2\n\
             [[step]]\nname = \"Polish\"\nrole = \"worker\"\nthen = \"Build\"\n",
        );
        assert!(goal.contains("goes round forever"), "{goal}");
    }

    #[test]
    fn only_a_worker_step_lets_its_agents_run_a_workflow_of_their_own() {
        let read = file("[[step]]\nname = \"Build\"\nrole = \"worker\"\nown_workflow = true\n");
        assert!(read.steps[0].own_workflow);
        let wrong =
            problems("[[step]]\nname = \"Review\"\nrole = \"reviewer\"\nown_workflow = true\n");
        assert!(
            wrong.contains(
                "own_workflow lets a worker run a workflow of its own, and this step's role is \
                 reviewer"
            ),
            "{wrong}"
        );
    }

    #[test]
    fn not_the_author_needs_an_author() {
        let none =
            problems("[[step]]\nname = \"Review\"\nrole = \"worker\"\nnot_the_author = true\n");
        assert!(
            none.contains("not_the_author needs a step before it"),
            "{none}"
        );
    }
}
