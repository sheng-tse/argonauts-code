//! A workflow file with `adapt = true`: the orchestrator writes the steps for each request as
//! a script, and keeps who plans, builds, reviews and signs off as the file says. A script
//! that doesn't hold up, or throws when it is rehearsed against stand-in answers, is sent back
//! once, and after that the file's own steps run as written.
//! Steps you sent back at the plan approval are written again with your notes.
//! A script that leaves out a step you require, puts one elsewhere, or changes a task list you
//! fixed is sent back the same way, saying what to change, and after that the file's steps run
//! with your steps placed and your tasks kept.

use std::collections::BTreeSet;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::{EndReason, InputPartKind};
use argonauts_core::events::workflow::{RunNote, WorkflowEvent};
use argonauts_core::names::RoleName;
use argonauts_core::required::{brief, check};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::worker::Access;
use argonauts_runner::{Required, RunInput, Script, WorkflowFile};
use serde_json::{Value, json};

use super::plan::Wrote;
use super::{journal, named};
use crate::card::support::part;
use crate::engine::host::{Host, SessionPlan, StandIns};
use crate::error::{EngineError, EngineResult};
use crate::roles;
use crate::workflow::StepCall;
use crate::workflow_size;

pub(crate) const WRITE_SCRIPT: &str = include_str!("write_script.md");

/// How often a task may go back to its worker, as `review.max_change_rounds` says. A writer
/// sent back more often than that gets no answer.
fn rounds_of_fixes(rounds: u32) -> String {
    let after = "a task its reviewers don't accept comes to the user with their findings";
    match rounds {
        0 => format!("Never send a task back to its worker: {after}."),
        1 => format!("Send a task back to its worker at most once. After that, {after}."),
        n => format!("Send a task back to its worker at most {n} times. After that, {after}."),
    }
}

/// How many times the orchestrator is asked: once, and once more with what was wrong.
const TRIES: u32 = 2;

pub(super) async fn script_for(
    host: &Host,
    call: &StepCall,
    file: &WorkflowFile,
    required: &Required,
    request: &str,
    notes: &[String],
) -> EngineResult<(Script, Wrote)> {
    let available = host.services().workers.available();
    let seats = roles::candidates(host.settings(), RoleName::Orchestrator);
    let orchestrator = seats
        .iter()
        .find(|seat| available.contains(seat))
        .or(seats.first())
        .cloned()
        .ok_or_else(|| EngineError::Refused("nobody is seated as orchestrator".to_owned()))?;
    // A run that picks up again runs what it started with: the script written after this many
    // notes of yours, or the file's own steps when no script could be used.
    match journal::written(host, call, notes.len()) {
        Some(source) if source.is_empty() => return Ok((Script::from_steps(file), Wrote::File)),
        Some(source) => {
            if let Ok(script) = checked(&source, required) {
                return Ok((script, Wrote::Orchestrator(orchestrator)));
            }
        }
        None => {}
    }
    let sent_back = sent_back(host, call, notes);
    let asked_size = workflow_size::asked(request);
    let yours = brief::for_writer(&super::required::shared(required));
    let mut wrong: Option<Unusable> = None;
    for _ in 0..TRIES {
        let mut parts = vec![
            part(InputPartKind::Prompt, "orchestrator", WRITE_SCRIPT, None),
            part(
                InputPartKind::StepInput,
                "the workflow file",
                described(file),
                None,
            ),
            part(
                InputPartKind::Rules,
                "size",
                workflow_size::guideline(host.settings().workflow_size, asked_size),
                None,
            ),
            part(
                InputPartKind::Rules,
                "rounds of fixes",
                rounds_of_fixes(host.settings().review.max_change_rounds),
                None,
            ),
            part(InputPartKind::Goal, "the request", request, None),
        ];
        if let Some(yours) = &yours {
            parts.push(part(
                InputPartKind::Rules,
                "what the user requires",
                yours,
                None,
            ));
        }
        if let Some(sent_back) = &sent_back {
            parts.push(part(InputPartKind::Message, "sent back", sent_back, None));
        }
        if let Some(wrong) = &wrong {
            parts.push(part(
                InputPartKind::Message,
                "sent back",
                wrong.again(),
                None,
            ));
        }
        let plan = SessionPlan {
            at: Some(call.at.clone()),
            role: RoleName::Orchestrator,
            worker: orchestrator.clone(),
            card: None,
            access: Access::ReadOnly,
            step: None,
            workdir: host.repo().base_worktree.clone(),
            parts,
            json_schema: Some(json!({
                "type": "object",
                "properties": {"script": {"type": "string"}},
                "required": ["script"],
                "additionalProperties": false,
            })),
            resume: None,
            slot: 0,
            label: Some("plan:orchestrator".to_owned()),
            reviews: false,
            fresh: false,
        };
        let end = host
            .run_session_in_turn(plan, None, StandIns::Allowed, &|_| Ok(()))
            .await?
            .end;
        let source = end
            .structured
            .as_ref()
            .and_then(|answer| answer.get("script"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| end.result_text.clone());
        let problem = match source {
            Some(source) if end.reason == EndReason::Completed => {
                match usable(host, &source, file, required, request).await {
                    Ok(script) => {
                        journal::keep_written(host, call, notes.len(), &source)?;
                        if let Some(asked) = asked_size {
                            let note = RunNote {
                                text: format!("As you asked: {}", asked.said()),
                                workflow: Some(call.workflow().clone()),
                                left: false,
                            };
                            let note = RunEventKind::Workflow(WorkflowEvent::Note(note));
                            host.record(None, None, note)?;
                        }
                        return Ok((script, Wrote::Orchestrator(orchestrator)));
                    }
                    Err(problem) => problem,
                }
            }
            _ => Unusable::from("it ended without a script".to_owned()),
        };
        wrong = Some(problem);
    }
    let why = wrong.map(|wrong| wrong.why).unwrap_or_default();
    let text = format!(
        "The orchestrator's steps couldn't be used ({why}), so {}'s own steps run as written",
        named(file)
    );
    let note = RunNote {
        text,
        workflow: Some(call.workflow().clone()),
        left: false,
    };
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Note(note)),
    )?;
    // Kept as nothing written, so a run that picks up again doesn't ask a third time.
    journal::keep_written(host, call, notes.len(), "")?;
    Ok((Script::from_steps(file), Wrote::File))
}

/// What the orchestrator reads when you sent its steps back at the plan approval: the script
/// it wrote last, and every note of yours so far.
fn sent_back(host: &Host, call: &StepCall, notes: &[String]) -> Option<String> {
    let newest = notes.len().checked_sub(1)?;
    let mut text = match journal::written(host, call, newest).filter(|last| !last.is_empty()) {
        Some(last) => format!("You wrote this script for the request:\n\n{last}\n\n"),
        None => String::new(),
    };
    let said: Vec<&String> = notes.iter().filter(|note| !note.is_empty()).collect();
    if said.is_empty() {
        text.push_str("The user sent it back without a note. Write it another way.");
    } else {
        text.push_str("The user sent it back with these notes, the newest last:\n");
        for note in said {
            text.push_str(&format!("- {note}\n"));
        }
        text.push_str("Write it again so it follows them.");
    }
    Some(text)
}

/// Why the orchestrator's script couldn't be used: a few words for the run's note, and what
/// it is told when it writes the script again.
struct Unusable {
    why: String,
    /// What to change, when the script left out or moved what you required.
    ask: Option<String>,
}

impl From<String> for Unusable {
    fn from(why: String) -> Self {
        Self { why, ask: None }
    }
}

impl Unusable {
    fn again(&self) -> String {
        match &self.ask {
            Some(ask) => ask.clone(),
            None => format!(
                "Your last script couldn't be used: {}\nWrite it again.",
                self.why
            ),
        }
    }
}

/// The script, when it also gets through a rehearsal: run against stand-in answers, with the
/// request and the file's roles as in the real run, it mustn't throw, and where you fixed the
/// tasks it must work on those.
async fn usable(
    host: &Host,
    source: &str,
    file: &WorkflowFile,
    required: &Required,
    request: &str,
) -> Result<Script, Unusable> {
    let script = checked(source, required)?;
    let input = RunInput {
        id: "rehearsal".to_owned(),
        args: super::args(host, request, &required.tasks),
        limits: super::limits(host),
        roles: Some(file.roles()),
        ..RunInput::default()
    };
    let load = |name: &str| super::workflow_named(host, name);
    let rehearsal = argonauts_runner::rehearse(&script, input, &load).await?;
    // A script still going when the rehearsal stopped may name the rest later.
    if rehearsal.finished {
        kept_tasks(required, &rehearsal.tasks)?;
        worked(required, &rehearsal.ran)?;
    }
    Ok(script)
}

/// Whether each step you require started an agent of its role in its phase: a phase that only
/// carries the step's name does none of its work.
fn worked(required: &Required, ran: &BTreeSet<(String, CrewRole)>) -> Result<(), Unusable> {
    let shared = super::required::shared(required);
    let idle: Vec<String> = shared
        .steps
        .iter()
        .filter(|step| {
            !ran.iter().any(|(phase, role)| {
                *role == step.role && argonauts_core::required::same_name(phase, &step.name)
            })
        })
        .map(|step| step.name.clone())
        .collect();
    if idle.is_empty() {
        return Ok(());
    }
    let found = check::Steps {
        missing: idle.clone(),
        misplaced: Vec::new(),
    };
    Err(Unusable {
        why: format!(
            "its phase for a step that is required starts no agent of that step's role: {}",
            idle.join(", ")
        ),
        ask: check::ask_to_place(&shared, &found),
    })
}

/// The script, when it reads as one and has a phase for every step you require, where it must
/// be.
fn checked(source: &str, required: &Required) -> Result<Script, Unusable> {
    let script = Script::parse(strip_fence(source)).map_err(|err| err.to_string())?;
    let required = super::required::shared(required);
    let titles: Vec<&str> = script
        .meta
        .phases
        .iter()
        .map(|phase| phase.title.as_str())
        .collect();
    let found = check::steps(&required, &titles);
    let Some(ask) = check::ask_to_place(&required, &found) else {
        return Ok(script);
    };
    let why = if found.missing.is_empty() {
        format!(
            "it has a required step where it mustn't be: {}",
            found.misplaced.join(", ")
        )
    } else {
        format!(
            "it has no phase for a step that is required: {}",
            found.missing.join(", ")
        )
    };
    Err(Unusable {
        why,
        ask: Some(ask),
    })
}

/// Whether the tasks a rehearsal saw, `planned`, are the list you fixed.
fn kept_tasks(required: &Required, planned: &[String]) -> Result<(), Unusable> {
    let required = super::required::shared(required);
    let planned: Vec<&str> = planned.iter().map(String::as_str).collect();
    let Some(changes) = check::tasks(&required, &planned) else {
        return Ok(());
    };
    match check::ask_to_keep_tasks(&required, &changes) {
        None => Ok(()),
        Some(ask) => Err(Unusable {
            why: "it changed the task list you fixed".to_owned(),
            ask: Some(ask),
        }),
    }
}

/// A model that wraps its script in a Markdown fence still wrote the script.
fn strip_fence(source: &str) -> &str {
    let trimmed = source.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let body = rest.split_once('\n').map_or(rest, |(_, body)| body);
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

/// The file's steps as the orchestrator reads them, with the steps you require marked. The
/// task list you fixed is told with them, in its own part.
fn described(file: &WorkflowFile) -> String {
    let mut text = format!("The workflow file, {}:\n\n", named(file));
    for (index, step) in file.steps.iter().enumerate() {
        let does = step.does.as_deref().unwrap_or("its role's usual work");
        let must = if step.required {
            " This step is required: keep it as a phase of this name."
        } else {
            ""
        };
        text.push_str(&format!(
            "{}. {} · role {} · {} · {does}{must}\n",
            index + 1,
            step.name,
            step.role,
            argonauts_runner::how(file, index),
        ));
    }
    if let Some(goal) = &file.goal {
        text.push_str(&format!(
            "\nThe whole workflow goes round until this holds: {goal}\n"
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fenced_script_is_still_the_script() {
        let plain = "export const meta = { name: 'a', description: 'b' }\nreturn 1";
        assert_eq!(strip_fence(plain), plain);
        assert_eq!(strip_fence(&format!("```js\n{plain}\n```\n")), plain);
        assert_eq!(strip_fence(&format!("```\n{plain}\n```")), plain);
    }

    #[test]
    fn a_script_is_used_only_when_it_reads_and_has_every_required_phase() {
        let good = "export const meta = { name: 'a', description: 'b', phases: [{ title: 'Benchmark' }] }\nreturn 1";
        let required = Required {
            steps: vec![argonauts_runner::RequiredStep {
                name: "Benchmark".to_owned(),
                role: argonauts_core::settings::crew::CrewRole::Workers,
                does: None,
                when: None,
            }],
            tasks: Vec::new(),
        };
        assert!(checked(good, &required).is_ok());
        let without = "export const meta = { name: 'a', description: 'b' }\nreturn 1";
        let wrong = checked(without, &required).expect_err("no Benchmark phase");
        assert_eq!(
            wrong.why,
            "it has no phase for a step that is required: Benchmark"
        );
        // The orchestrator is told once what to place and where, in Talk's words.
        let again = wrong.again();
        assert!(
            again.starts_with("The workflow you wrote doesn't have every step"),
            "{again}"
        );
        assert!(
            again.contains("- Benchmark, before the sign-off, by the workers (missing)"),
            "{again}"
        );
        let unreadable = checked("const x = 1", &Required::default()).err();
        assert!(unreadable.is_some_and(|wrong| wrong.ask.is_none()));
    }

    #[test]
    fn a_required_phase_that_starts_no_agent_of_its_role_is_missing() {
        let required = Required {
            steps: vec![argonauts_runner::RequiredStep {
                name: "Security review".to_owned(),
                role: CrewRole::Reviewers,
                does: None,
                when: None,
            }],
            tasks: Vec::new(),
        };
        let did = BTreeSet::from([("security review".to_owned(), CrewRole::Reviewers)]);
        assert!(worked(&required, &did).is_ok());
        // The phase is there, with a worker in it, and no reviewer at all.
        let named_only = BTreeSet::from([
            ("Build".to_owned(), CrewRole::Workers),
            ("Security review".to_owned(), CrewRole::Workers),
        ]);
        let wrong = worked(&required, &named_only).expect_err("no reviewer ran it");
        assert_eq!(
            wrong.why,
            "its phase for a step that is required starts no agent of that step's role: \
             Security review"
        );
        assert!(
            wrong
                .again()
                .contains("- Security review, before the sign-off, by the reviewers (missing)"),
            "{}",
            wrong.again()
        );
    }

    #[test]
    fn a_required_step_in_the_wrong_place_is_sent_back_too() {
        let required = Required {
            steps: vec![argonauts_runner::RequiredStep {
                name: "Benchmark".to_owned(),
                role: argonauts_core::settings::crew::CrewRole::Workers,
                does: None,
                when: Some("last".to_owned()),
            }],
            tasks: Vec::new(),
        };
        let phases = |titles: &str| {
            format!(
                "export const meta = {{ name: 'a', description: 'b', \
                 phases: [{titles}] }}\nreturn 1"
            )
        };
        let first = phases("{ title: 'Benchmark' }, { title: 'Build' }");
        let wrong = checked(&first, &required).expect_err("Benchmark must come last");
        assert_eq!(
            wrong.why,
            "it has a required step where it mustn't be: Benchmark"
        );
        assert!(wrong.again().contains("(in the wrong place)"));
        let last = phases("{ title: 'Build' }, { title: 'Benchmark' }");
        assert!(checked(&last, &required).is_ok());
    }

    #[test]
    fn a_script_that_plans_other_tasks_than_the_ones_you_fixed_is_sent_back() {
        let required = Required {
            steps: Vec::new(),
            tasks: vec!["migrate the schema".to_owned(), "switch reads".to_owned()],
        };
        let kept = ["build:migrate-schema".to_owned(), "switch-reads".to_owned()];
        assert!(kept_tasks(&required, &kept).is_ok());
        let merged = ["migrate the schema and switch reads".to_owned()];
        let wrong = kept_tasks(&required, &merged).expect_err("one task, not two");
        assert_eq!(wrong.why, "it changed the task list you fixed");
        let again = wrong.again();
        assert!(
            again.starts_with("The user fixed the task list, and your plan changed it."),
            "{again}"
        );
        assert!(
            again.contains("1. migrate the schema\n2. switch reads\n"),
            "{again}"
        );
        assert!(
            kept_tasks(&Required::default(), &merged).is_ok(),
            "nothing fixed"
        );
    }
}
