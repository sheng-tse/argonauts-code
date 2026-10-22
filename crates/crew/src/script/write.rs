//! A workflow file with `adapt = true`: the orchestrator writes the steps for each request as
//! a script, and keeps who plans, builds, reviews and signs off as the file says. A script
//! that doesn't hold up, or throws when it is rehearsed against placeholder answers, is sent back
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
use argonauts_core::session_runner::Access;
use argonauts_core::settings::crew::CrewRole;
use argonauts_runner::{Required, RunInput, Script, WorkflowFile};
use serde_json::{Value, json};

use super::plan::Wrote;
use super::{journal, named};
use crate::engine::host::{Fallbacks, Host, SessionPlan};
use crate::error::{EngineError, EngineResult};
use crate::roles;
use crate::task::support::part;
use crate::workflow::StepCall;
use crate::workflow_size;

pub(crate) const WRITE_SCRIPT: &str = argonauts_core::template!("prompts/workflow/write-script.md");

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
    let models = roles::candidates(host.settings(), RoleName::Orchestrator);
    let orchestrator = models
        .iter()
        .find(|model| available.contains(model))
        .or(models.first())
        .cloned()
        .ok_or_else(|| EngineError::Refused("no model is set for the orchestrator".to_owned()))?;
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
    // A copy of the main session that ended without its script gives way to the brief alone.
    let mut fork = crate::orchestrated::fork_for(host, &orchestrator);
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
            task: None,
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
            // Its row is named for the workflow it plans, as `plan:council`.
            label: Some(format!("plan:{}", file.name)),
            reviews: false,
            fresh: false,
            independent_of: Vec::new(),
            in_place_of: None,
            fork_of: fork.clone(),
        };
        let end = host
            .run_session_in_turn(plan, None, Fallbacks::Allowed, &|_| Ok(()))
            .await?
            .end;
        if end.reason != EndReason::Completed {
            fork = None;
        }
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

/// The script, when it also gets through a rehearsal: run against placeholder answers, with the
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
    let crew = input.args.get("crew").cloned().unwrap_or_default();
    let rehearsal = argonauts_runner::rehearse(&script, input, &load).await?;
    verdicts(&rehearsal.asked)?;
    // A script still going when the rehearsal stopped may name the rest later.
    if rehearsal.finished {
        kept_tasks(required, &rehearsal.tasks)?;
        worked(required, &rehearsal.ran)?;
        kept_steps(file, &rehearsal.asked, &crew)?;
    }
    Ok(script)
}

/// The answer format of every reviewer and every judge that signs off, as the planner is told
/// to write it.
const VERDICT_SCHEMA: &str = r#"{"type": "object", "properties": {"accept": {"type": "boolean"}, "findings": {"type": "array", "items": {"type": "string"}}}, "required": ["accept", "findings"]}"#;

/// Whether every reviewer and every judge that signs off answers with a yes-or-no `accept`,
/// the one field a verdict is read by.
fn verdicts(asked: &[argonauts_runner::Asked]) -> Result<(), Unusable> {
    let without = argonauts_runner::without_verdicts(asked);
    if without.is_empty() {
        return Ok(());
    }
    let why = format!(
        "a reviewer or a judge that signs off answers without a required yes-or-no accept: {}",
        without.join(", ")
    );
    let ask = format!(
        "Your last script couldn't be used: {why}.\nGive every reviewer, and every judge that \
         signs off, exactly the schema {VERDICT_SCHEMA}, and write it again."
    );
    Err(Unusable {
        why,
        ask: Some(ask),
    })
}

/// Whether the script carries out every step the file keeps, by the roles, access and order of
/// its agents, whatever it names their phases.
fn kept_steps(
    file: &WorkflowFile,
    asked: &[argonauts_runner::Asked],
    crew: &Value,
) -> Result<(), Unusable> {
    let models = |role: CrewRole| {
        crew.get(role.as_str())
            .and_then(Value::as_u64)
            .map_or(1, |count| u32::try_from(count).unwrap_or(u32::MAX))
    };
    let missing = argonauts_runner::left_out(file, asked, &models);
    if missing.is_empty() {
        return Ok(());
    }
    let why = format!(
        "it leaves out what the workflow keeps: {}",
        missing.join("; ")
    );
    let lines: Vec<String> = missing.iter().map(|line| format!("- {line}")).collect();
    let ask = format!(
        "Your last script couldn't be used: it doesn't carry out every step the workflow file \
         keeps. These are missing:\n{}\nWrite it again with them, under phase names of your own.",
        lines.join("\n")
    );
    Err(Unusable {
        why,
        ask: Some(ask),
    })
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
    let phases: Vec<&str> = script
        .meta
        .phases
        .iter()
        .map(|phase| phase.name.as_str())
        .collect();
    let found = check::steps(&required, &phases);
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

/// A model that wraps its script, or a JSON answer, in a Markdown fence still wrote it.
pub(super) fn strip_fence(source: &str) -> &str {
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

    /// A workflow inside a workflow is yours to write in your own files: the orchestrator is
    /// never taught `workflow()`, so a script it writes runs one level.
    #[test]
    fn the_orchestrator_is_not_taught_to_start_workflows() {
        assert!(WRITE_SCRIPT.contains("`agent(prompt, opts)`"));
        assert!(!WRITE_SCRIPT.contains("workflow("), "{WRITE_SCRIPT}");
    }

    #[test]
    fn a_fenced_script_is_still_the_script() {
        let plain = "export const meta = { name: 'a', description: 'b' }\nreturn 1";
        assert_eq!(strip_fence(plain), plain);
        assert_eq!(strip_fence(&format!("```js\n{plain}\n```\n")), plain);
        assert_eq!(strip_fence(&format!("```\n{plain}\n```")), plain);
    }

    #[test]
    fn a_script_is_used_only_when_it_reads_and_has_every_required_phase() {
        let good = "export const meta = { name: 'a', description: 'b', phases: [{ name: 'Benchmark' }] }\nreturn 1";
        let required = Required {
            steps: vec![argonauts_runner::RequiredStep {
                name: "Benchmark".to_owned(),
                role: argonauts_core::settings::crew::CrewRole::Worker,
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
                role: CrewRole::Reviewer,
                does: None,
                when: None,
            }],
            tasks: Vec::new(),
        };
        let did = BTreeSet::from([("security review".to_owned(), CrewRole::Reviewer)]);
        assert!(worked(&required, &did).is_ok());
        // The phase is there, with a worker in it, and no reviewer at all.
        let named_only = BTreeSet::from([
            ("Build".to_owned(), CrewRole::Worker),
            ("Security review".to_owned(), CrewRole::Worker),
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
                role: argonauts_core::settings::crew::CrewRole::Worker,
                does: None,
                when: Some("last".to_owned()),
            }],
            tasks: Vec::new(),
        };
        let phases = |listed: &str| {
            format!(
                "export const meta = {{ name: 'a', description: 'b', \
                 phases: [{listed}] }}\nreturn 1"
            )
        };
        let first = phases("{ name: 'Benchmark' }, { name: 'Build' }");
        let wrong = checked(&first, &required).expect_err("Benchmark must come last");
        assert_eq!(
            wrong.why,
            "it has a required step where it mustn't be: Benchmark"
        );
        assert!(wrong.again().contains("(in the wrong place)"));
        let last = phases("{ name: 'Build' }, { name: 'Benchmark' }");
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
