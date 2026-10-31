//! How each task of an agreed plan is done: in the seat's own session, by a workflow it runs,
//! or by agents it starts.
//!
//! A way you name in your request binds: the plan marks it "You asked", and code checks the
//! plan against what you asked before work starts. Every other task gets the note keeper's way,
//! picked by a rule of thumb from the ways the setup allows, with a short reason.

mod asked;
mod checker;
mod objections;
mod offer;
mod run;
mod steps;
mod thumb;
mod tries;

pub use asked::{
    Asked, AskedWay, asked_in, attach, binding_line, check, task_named, unmark, why, yours_stands,
};
pub(crate) use checker::of_task as checker_of_task;
pub use checker::{Checker, asks_first, checked_by_its_own_model, ladder, same_maker_line};
pub use objections::{Talk, objection_line, objection_lines, objections};
pub use offer::{Offer, Toward};
pub use run::briefed;
pub(crate) use run::{drive_planned, driven_by_its_way, first_turn, planned, refusal};
pub use steps::{
    DRAFT_AND_VOTE, PickedBy, TaskStep, TriedBy, drafts_and_vote, steps_in, steps_of, task_steps,
};
pub use thumb::{Proposed, RULE_OF_THUMB, agents_misfit, parts, propose, settle};

use argonauts_core::TaskId;
use argonauts_core::runs::events::task::Way;
use argonauts_core::settings::Settings;
use argonauts_core::settings::ways::Ways;
use argonauts_core::workflows::workflow::WorkflowRef;
use argonauts_core::workflows::workflow_files::BUILT_IN;
use argonauts_runner::{Library, Origin};

use crate::engine::host::Host;

/// The workflows that ship with Argonauts Code which a single task can be done by, in the
/// order a plan offers them. Council, Orchestrator and Captain each run a whole request.
pub const TASK_WORKFLOWS: &[&str] = &["maker-checker", "best-of-n", "draft-and-vote"];

/// The built-in workflow a risky task is done by.
pub const MAKER_CHECKER: &str = "maker-checker";

/// Which sheet sets the ways a run's plan may pick from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// `[council.ways]`, for a council's plan.
    Council,
    /// `[crew.ways]`, for every other workflow's plan.
    Orchestrator,
}

impl Setup {
    /// A council's plan follows `[council.ways]`; every other plan follows `[crew.ways]`.
    pub fn of(settings: &Settings) -> Self {
        if settings.workflow == "council" {
            Self::Council
        } else {
            Self::Orchestrator
        }
    }

    pub fn ways(self, settings: &Settings) -> &Ways {
        match self {
            Self::Council => &settings.council.ways,
            Self::Orchestrator => &settings.crew.ways,
        }
    }

    /// The sheet that changes the ways, as its title reads.
    pub fn sheet(self) -> &'static str {
        match self {
            Self::Council => "Set up Council",
            Self::Orchestrator => "Set up Orchestrator",
        }
    }

    /// Whose setup it is, as a sentence names it.
    fn whose(self) -> &'static str {
        match self {
            Self::Council => "this council's setup",
            Self::Orchestrator => "the orchestrator's setup",
        }
    }
}

/// Every workflow a task could be done by: the shipped ones, then `yours` (the project's and
/// your own, in the order the library lists them), each name once.
pub fn task_workflows(yours: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = TASK_WORKFLOWS
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    for name in yours {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Every workflow a task of this run could be done by: the shipped ones, then the project's
/// and yours.
pub fn workflows_for(host: &Host) -> Vec<String> {
    workflows_in(&crate::script::library(host))
}

/// Every workflow a task could be done by from `library`: the shipped ones, then the project's
/// and yours that read without a problem. A file named after a workflow that runs a whole
/// request, such as `council`, is left out.
pub fn workflows_in(library: &Library) -> Vec<String> {
    let whole_runs: Vec<&str> = BUILT_IN
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !TASK_WORKFLOWS.contains(name))
        .collect();
    let yours = library
        .list()
        .into_iter()
        .filter(|listed| listed.origin != Origin::BuiltIn && listed.problem.is_none())
        .map(|listed| listed.name)
        .filter(|name| !whole_runs.contains(&name.as_str()));
    task_workflows(yours)
}

/// The ways this run's plan may pick from.
pub fn offer_for(host: &Host) -> Offer {
    offer_in(host.settings(), &crate::script::library(host))
}

/// The ways a plan under `settings` may pick from, of the workflows in `library` those a task
/// can follow. The approval's ←/→ steps through them.
pub fn offer_in(settings: &Settings, library: &Library) -> Offer {
    let followed: Vec<String> = workflows_in(library)
        .into_iter()
        .filter(|name| steps_in(library, name).is_ok())
        .collect();
    Offer::for_run(settings, &followed)
}

/// A way as the plan's How column and a run row write it: "Own session", "Maker-checker",
/// "3 agents".
pub fn words(way: &Way) -> String {
    match way {
        Way::OwnSession => "Own session".to_owned(),
        Way::Workflow { name } => workflow_words(name.as_str()),
        Way::CliWorkflow => "Its CLI's own".to_owned(),
        Way::Agents { count } => format!("{count} agents"),
    }
}

/// How a sentence says a task is done: "in its own session", "by Maker-checker", "by 3 agents".
pub fn done_by(way: &Way) -> String {
    match way {
        Way::OwnSession => "in its own session".to_owned(),
        Way::CliWorkflow => "by its CLI's own workflow".to_owned(),
        way => format!("by {}", words(way)),
    }
}

/// A workflow's name as a sentence writes it: `maker-checker` reads "Maker-checker".
pub fn workflow_words(name: &str) -> String {
    let mut letters = name.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// The words an edit line gives a way, which [`parse`] reads back.
pub fn edit_words(way: &Way) -> String {
    match way {
        Way::OwnSession => "own session".to_owned(),
        Way::Workflow { name } => name.as_str().to_owned(),
        Way::CliWorkflow => "its cli's own".to_owned(),
        Way::Agents { count } => format!("{count} agents"),
    }
}

/// The edit that sets a task's way, as ←/→ at the approval writes it into your note.
pub fn edit_line(task: &TaskId, way: &Way) -> String {
    format!("{task} way {}", edit_words(way))
}

/// Reads a way as you write it: "own session", "maker-checker" or any workflow's name, "3
/// agents", "its cli's own". A workflow's name is taken as written; whether it exists and is
/// allowed is the offer's to say.
pub fn parse(text: &str) -> Result<Way, String> {
    let said = text.trim().to_lowercase();
    let words: Vec<&str> = said
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|word| !word.is_empty())
        .collect();
    match words.as_slice() {
        [] => Err("A way needs a name, such as own session, maker-checker or 3 agents.".into()),
        ["own", "session"] => Ok(Way::OwnSession),
        ["its", "cli's", "own"] | ["cli's", "own"] | ["cli", "own"] => Ok(Way::CliWorkflow),
        [count, "agents"] => match count.parse::<u8>() {
            Ok(count @ 2..=4) => Ok(Way::Agents { count }),
            _ => Err(format!("\"{}\": a task starts 2 to 4 agents.", text.trim())),
        },
        _ if said.contains(char::is_whitespace) => Err(format!(
            "\"{}\" isn't a way: name own session, a workflow, or 2 to 4 agents.",
            text.trim()
        )),
        _ => Ok(Way::Workflow {
            name: WorkflowRef::new(&said),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow(name: &str) -> Way {
        Way::Workflow {
            name: WorkflowRef::new(name),
        }
    }

    #[test]
    fn each_way_reads_as_the_plan_writes_it() {
        assert_eq!(words(&Way::OwnSession), "Own session");
        assert_eq!(words(&workflow("maker-checker")), "Maker-checker");
        assert_eq!(words(&workflow("best-of-n")), "Best-of-n");
        assert_eq!(words(&Way::Agents { count: 3 }), "3 agents");
        assert_eq!(words(&Way::CliWorkflow), "Its CLI's own");
        assert_eq!(done_by(&Way::OwnSession), "in its own session");
        assert_eq!(done_by(&workflow("maker-checker")), "by Maker-checker");
        assert_eq!(done_by(&Way::Agents { count: 2 }), "by 2 agents");
    }

    #[test]
    fn an_edit_line_reads_back_as_the_way_it_wrote() {
        let task = TaskId::numbered(4);
        for way in [
            Way::OwnSession,
            workflow("maker-checker"),
            workflow("release-notes"),
            Way::CliWorkflow,
            Way::Agents { count: 2 },
        ] {
            let line = edit_line(&task, &way);
            let said = line.strip_prefix("task-004 way ").expect(&line);
            assert_eq!(parse(said), Ok(way), "{line}");
        }
        assert_eq!(parse("Own-Session"), Ok(Way::OwnSession));
        assert_eq!(parse(" Maker-checker "), Ok(workflow("maker-checker")));
    }

    #[test]
    fn a_way_that_isnt_one_says_why() {
        assert!(parse("").is_err());
        assert_eq!(
            parse("5 agents"),
            Err("\"5 agents\": a task starts 2 to 4 agents.".to_owned())
        );
        assert!(parse("one agent").is_err());
        assert!(parse("by hand please").is_err());
    }

    #[test]
    fn yours_follow_the_shipped_workflows_once_each() {
        let names = task_workflows(["release-notes".to_owned(), "best-of-n".to_owned()]);
        assert_eq!(
            names,
            [
                "maker-checker",
                "best-of-n",
                "draft-and-vote",
                "release-notes"
            ]
        );
    }

    #[test]
    fn the_offer_holds_the_workflows_one_task_can_follow() {
        let dir = tempfile::tempdir().unwrap();
        let polish = "name = \"polish\"\n[[step]]\nname = \"Build\"\nrole = \"worker\"\n\
                      [[step]]\nname = \"Review\"\nrole = \"reviewer\"\n";
        let council = "name = \"council\"\n[[step]]\nname = \"Build\"\nrole = \"worker\"\n";
        let plan_first = "name = \"plan-first\"\n[[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\n\
                          [[step]]\nname = \"Build\"\nrole = \"worker\"\n";
        std::fs::write(dir.path().join("polish.toml"), polish).unwrap();
        std::fs::write(dir.path().join("council.toml"), council).unwrap();
        std::fs::write(dir.path().join("plan-first.toml"), plan_first).unwrap();
        let library = Library {
            project: Some(dir.path().to_path_buf()),
            ..Library::default()
        };
        assert_eq!(
            workflows_in(&library),
            [
                "maker-checker",
                "best-of-n",
                "draft-and-vote",
                "plan-first",
                "polish"
            ]
        );
        let offered: Vec<String> = offer_in(&Settings::default(), &library)
            .ways()
            .iter()
            .filter(|way| matches!(way, Way::Workflow { .. }))
            .map(words)
            .collect();
        // Draft-and-vote in its form for one task; Plan-first plans a whole request.
        assert_eq!(
            offered,
            ["Maker-checker", "Best-of-n", "Draft-and-vote", "Polish"]
        );
    }

    #[test]
    fn a_council_reads_its_own_ways_and_every_other_plan_the_crews() {
        let mut settings = Settings {
            workflow: "council".to_owned(),
            ..Settings::default()
        };
        settings.council.ways.own_session = false;
        assert_eq!(Setup::of(&settings), Setup::Council);
        assert!(!Setup::of(&settings).ways(&settings).own_session);
        settings.workflow = "orchestrator".to_owned();
        assert_eq!(Setup::of(&settings), Setup::Orchestrator);
        assert!(Setup::of(&settings).ways(&settings).own_session);
        assert_eq!(Setup::Orchestrator.sheet(), "Set up Orchestrator");
    }
}
