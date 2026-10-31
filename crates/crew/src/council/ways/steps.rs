//! A workflow one task is done by, step by step: the task's worker takes each step of the
//! worker's in its own session, a fresh session of a checker takes each step of a reviewer or a
//! judge, and a step that doesn't accept the work sends it back to the step it names. A worker's
//! step that starts several at once makes that many tries at the task, and the check after it
//! picks the one the task keeps.
//!
//! Draft-and-vote's file plans a whole request with phases, so one task follows a form code
//! gives it: blind drafts, one by each maker set up, then a vote of judges from each maker.

use argonauts_core::settings::crew::CrewRole;
use argonauts_runner::{Count, Library, LibraryError, Loaded, WorkflowFile};

use super::workflow_words;
use crate::engine::host::Host;

/// One step of a workflow as one task follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStep {
    /// The worker builds, or goes again on what it built with the step's words.
    Build { step: String, does: Option<String> },
    /// Tries at the task side by side, each in a worktree of its own from the same base and
    /// none seeing the others: the worker's own session makes the first, fresh sessions the
    /// rest. `Count::All` makes one per worker set up, or one per maker for drafts.
    Tries {
        step: String,
        count: Count,
        by: TriedBy,
    },
    /// The tries that passed their checks are read numbered and unnamed, and the one whose work
    /// the task keeps is picked.
    Pick {
        step: String,
        role: CrewRole,
        by: PickedBy,
    },
    /// A checker reads the change. One it doesn't accept goes back to the step at
    /// `send_back_to`, or to the last build before it.
    Check {
        step: String,
        role: CrewRole,
        send_back_to: Option<usize>,
    },
}

/// Whose fresh sessions make the tries after the worker's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriedBy {
    /// The worker's own model, as Best-of-n tries.
    SameModel,
    /// A worker of each other maker set up, as Draft-and-vote drafts.
    EachMaker,
}

/// Who picks the try the task keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickedBy {
    /// One checker, from another maker than the worker whenever one is set up.
    Checker,
    /// A judge of each maker set up votes, and the try with the most votes is kept.
    Vote,
}

/// The workflow whose drafts and vote one task follows in the form [`drafts_and_vote`] gives.
pub const DRAFT_AND_VOTE: &str = "draft-and-vote";

/// Draft-and-vote for one task: blind drafts, one by a worker of each maker set up, then a
/// vote of judges, one of each maker, on which draft the task keeps.
pub fn drafts_and_vote() -> Vec<TaskStep> {
    vec![
        TaskStep::Tries {
            step: "Drafts".to_owned(),
            count: Count::All,
            by: TriedBy::EachMaker,
        },
        TaskStep::Pick {
            step: "Votes".to_owned(),
            role: CrewRole::Judge,
            by: PickedBy::Vote,
        },
    ]
}

/// The steps of the workflow named `name` in the order one task follows them, or why one task
/// can't be done by it.
pub fn steps_of(host: &Host, name: &str) -> Result<Vec<TaskStep>, String> {
    steps_in(&crate::script::library(host), name)
}

/// [`steps_of`] for the workflow named `name` in `library`.
pub fn steps_in(library: &Library, name: &str) -> Result<Vec<TaskStep>, String> {
    match library.load(name) {
        Ok(Loaded::Steps(file)) => task_steps(name, &file),
        Ok(Loaded::Script(_)) => Err(format!(
            "{} is a script, so a task can't follow its steps.",
            workflow_words(name)
        )),
        // Its phases plan and build a whole request, so they hold nothing one task can follow.
        Err(LibraryError::NoSteps { .. }) if name == DRAFT_AND_VOTE => Ok(drafts_and_vote()),
        Err(LibraryError::NoSteps { .. }) => Err(no_steps(name)),
        Err(LibraryError::Unknown(_)) if shipped_without_steps(name) => match name {
            DRAFT_AND_VOTE => Ok(drafts_and_vote()),
            _ => Err(no_steps(name)),
        },
        Err(err) => Err(err.to_string()),
    }
}

/// The file's steps as one task follows them.
pub fn task_steps(name: &str, file: &WorkflowFile) -> Result<Vec<TaskStep>, String> {
    let named = workflow_words(name);
    if file.steps.is_empty() {
        return Err(no_steps(name));
    }
    let mut steps: Vec<TaskStep> = Vec::new();
    for step in &file.steps {
        let task_step = match step.role {
            CrewRole::Worker => match step.count.filter(|count| *count != Count::Fixed(1)) {
                Some(count) => TaskStep::Tries {
                    step: step.name.clone(),
                    count,
                    by: TriedBy::SameModel,
                },
                None => TaskStep::Build {
                    step: step.name.clone(),
                    does: step.does.clone(),
                },
            },
            CrewRole::Reviewer | CrewRole::Judge
                if matches!(steps.last(), Some(TaskStep::Tries { .. })) =>
            {
                if let Some(target) = &step.send_back_to {
                    return Err(format!(
                        "{named}'s {} step picks one of the tries, so it can't send work back \
                         to {target}.",
                        step.name
                    ));
                }
                TaskStep::Pick {
                    step: step.name.clone(),
                    role: step.role,
                    by: PickedBy::Checker,
                }
            }
            CrewRole::Reviewer | CrewRole::Judge => {
                let send_back_to = match &step.send_back_to {
                    Some(target) => Some(
                        file.steps
                            .iter()
                            .position(|other| other.name.eq_ignore_ascii_case(target))
                            .ok_or_else(|| {
                                format!(
                                    "{named}'s {} step sends work back to {target}, which it \
                                     doesn't have.",
                                    step.name
                                )
                            })?,
                    ),
                    None => None,
                };
                TaskStep::Check {
                    step: step.name.clone(),
                    role: step.role,
                    send_back_to,
                }
            }
            other => {
                return Err(format!(
                    "{named} can't do one task: its {} step is the {other}'s.",
                    step.name
                ));
            }
        };
        if let Some(TaskStep::Tries { step, count, .. }) = steps.last()
            && !matches!(task_step, TaskStep::Pick { .. })
        {
            return Err(unpicked(&named, step, *count));
        }
        steps.push(task_step);
    }
    if let Some(TaskStep::Tries { step, count, .. }) = steps.last() {
        return Err(unpicked(&named, step, *count));
    }
    if !steps.iter().any(TaskStep::builds) {
        return Err(format!("{named} has no step that builds."));
    }
    Ok(steps)
}

impl TaskStep {
    /// Whether the step makes or changes the work, rather than reading it.
    pub fn builds(&self) -> bool {
        matches!(self, Self::Build { .. } | Self::Tries { .. })
    }
}

/// A workflow that ships with phases alone, which the library leaves out of those it runs.
fn shipped_without_steps(name: &str) -> bool {
    argonauts_core::workflows::workflow_files::built_in(name)
        .is_some_and(|text| matches!(WorkflowFile::parse(text), Ok(None)))
}

fn no_steps(name: &str) -> String {
    format!("{} has no steps a task can follow.", workflow_words(name))
}

/// Why a step that makes several tries needs a check right after it.
fn unpicked(named: &str, step: &str, count: Count) -> String {
    let tries = match count {
        Count::Fixed(n) => format!("{n} tries"),
        Count::All => "a try per worker".to_owned(),
    };
    format!("{named}'s {step} step makes {tries}, and no step right after it picks one.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> WorkflowFile {
        WorkflowFile::parse(text)
            .expect("a workflow file")
            .expect("one with steps")
    }

    fn built_in(name: &str) -> WorkflowFile {
        file(
            argonauts_core::workflows::workflow_files::built_in(name).expect("a built-in workflow"),
        )
    }

    #[test]
    fn maker_checker_is_a_build_then_a_review_that_sends_it_back() {
        assert_eq!(
            task_steps("maker-checker", &built_in("maker-checker")),
            Ok(vec![
                TaskStep::Build {
                    step: "Build".to_owned(),
                    does: None
                },
                TaskStep::Check {
                    step: "Review".to_owned(),
                    role: CrewRole::Reviewer,
                    send_back_to: Some(0)
                },
            ])
        );
    }

    #[test]
    fn best_of_n_makes_three_tries_and_its_judge_picks_one() {
        assert_eq!(
            task_steps("best-of-n", &built_in("best-of-n")),
            Ok(vec![
                TaskStep::Tries {
                    step: "Try".to_owned(),
                    count: Count::Fixed(3),
                    by: TriedBy::SameModel,
                },
                TaskStep::Pick {
                    step: "Pick".to_owned(),
                    role: CrewRole::Judge,
                    by: PickedBy::Checker,
                },
            ])
        );
    }

    #[test]
    fn draft_and_vote_drafts_by_each_maker_then_votes() {
        let steps = steps_in(&Library::default(), DRAFT_AND_VOTE).unwrap();
        assert_eq!(steps, drafts_and_vote());
        assert!(matches!(
            steps.as_slice(),
            [
                TaskStep::Tries {
                    count: Count::All,
                    by: TriedBy::EachMaker,
                    ..
                },
                TaskStep::Pick {
                    by: PickedBy::Vote,
                    ..
                },
            ]
        ));
    }

    #[test]
    fn tries_need_a_pick_right_after_them() {
        let unpicked = file(
            "name = \"many\"\n[[step]]\nname = \"Try\"\nrole = \"worker\"\ncount = \"all\"\n\
             [[step]]\nname = \"Polish\"\nrole = \"worker\"\n",
        );
        assert_eq!(
            task_steps("many", &unpicked),
            Err(
                "Many's Try step makes a try per worker, and no step right after it picks one."
                    .to_owned()
            )
        );
        let last =
            file("name = \"many\"\n[[step]]\nname = \"Try\"\nrole = \"worker\"\ncount = 2\n");
        assert_eq!(
            task_steps("many", &last),
            Err("Many's Try step makes 2 tries, and no step right after it picks one.".to_owned())
        );
        let sends_back = file(
            "name = \"many\"\n[[step]]\nname = \"Try\"\nrole = \"worker\"\ncount = 2\n\
             [[step]]\nname = \"Pick\"\nrole = \"judge\"\nsend_back_to = \"Try\"\n",
        );
        assert_eq!(
            task_steps("many", &sends_back),
            Err(
                "Many's Pick step picks one of the tries, so it can't send work back to Try."
                    .to_owned()
            )
        );
    }

    #[test]
    fn what_one_task_cant_follow_says_why() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("plan-only.toml"),
            "name = \"plan-only\"\n",
        )
        .unwrap();
        let library = Library {
            project: Some(project.path().to_path_buf()),
            ..Library::default()
        };
        assert_eq!(
            steps_in(&library, "plan-only"),
            Err("Plan-only has no steps a task can follow.".to_owned())
        );
        let planned = file(
            "name = \"plan-first\"\n[[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\n\
             [[step]]\nname = \"Build\"\nrole = \"worker\"\n",
        );
        assert_eq!(
            task_steps("plan-first", &planned),
            Err("Plan-first can't do one task: its Plan step is the orchestrator's.".to_owned())
        );
    }

    #[test]
    fn a_file_of_yours_may_build_twice_and_sign_off() {
        let yours = file(
            "name = \"polish\"\n[[step]]\nname = \"Build\"\nrole = \"worker\"\n\
             [[step]]\nname = \"Polish\"\nrole = \"worker\"\ndoes = \"Tidy what you built.\"\n\
             [[step]]\nname = \"Sign off\"\nrole = \"judge\"\nsend_back_to = \"Polish\"\n",
        );
        let steps = task_steps("polish", &yours).unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(
            steps[2],
            TaskStep::Check {
                step: "Sign off".to_owned(),
                role: CrewRole::Judge,
                send_back_to: Some(1)
            }
        );
    }
}
