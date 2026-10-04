//! Changing a running workflow from its next phase on: proposed from outside
//! (`EngineCommand::ProposeWorkflowChange`) or by the lead at a phase end. Both go through the
//! validator and the `change_request` checkpoint, and an accepted one is recorded when it's
//! decided. Also the lead's design for a task, before the run's workflow starts.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::Value;

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::{
    AnswerAction, ChangeRequestPacket, CheckpointDecided, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText,
};
use argonauts_core::events::goal::Criterion;
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunNamed};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::events::workflow::{
    DeclaredPhase, RunNote, WorkflowChanged, WorkflowDeclared, WorkflowEvent,
};
use argonauts_core::log::{Note, NoteLevel};
use argonauts_core::names::{CheckpointId, RoleName, WorkflowId};
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::design::{DesignInput, DesignOutput, DesignWorkflow, WorkflowExample};
use argonauts_core::text::sentence_case;
use argonauts_core::workflow::{
    StandardPhase, StepKind, StepRef, Workflow, WorkflowParams, WorkflowRef,
};

use crate::checkpoints::{self, CheckpointRequest};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::prompts::workflow::DESIGN_WORKFLOW;
use crate::state::RunState;
use crate::workflow::validate::kind_name;
use crate::workflow::{StepAt, brief, builtin, params, runner, shape, yaml};

/// The evidence document that holds the proposed workflow as YAML.
const PROPOSED: &str = "Proposed workflow";

/// Validates the change and opens its `change_request` checkpoint without waiting for it.
/// `Err` holds the validator's errors, one per line. Once you accept it, `on_decided` records
/// `Workflow::Changed` and the runner takes it from the next phase on.
pub async fn propose(
    host: &Host,
    target: Option<WorkflowId>,
    workflow: Workflow,
    reason: String,
) -> Result<CheckpointId, String> {
    let id = target
        .or_else(|| host.read(|state| state.workflows.root.clone()))
        .ok_or_else(|| "no workflow is running".to_owned())?;
    let request = change_request(host, &id, &workflow, &reason, Party::User, None)?;
    let opening = checkpoints::open(host, request).map_err(|err| err.to_string())?;
    Ok(opening.id().clone())
}

/// The checkpoint for a change to workflow `id`, once the change is valid, differs from what
/// runs, and leaves alone what already ran. `Err` holds the reasons, one per line. `phase_end`
/// is where the lead proposed it.
fn change_request(
    host: &Host,
    id: &WorkflowId,
    workflow: &Workflow,
    reason: &str,
    from: Party,
    phase_end: Option<&StepAt>,
) -> Result<CheckpointRequest, String> {
    let current = valid_change(host, id, workflow)?;
    let proposed = yaml::print(workflow)?;
    let running = yaml::print(&current)?;
    let card = CardId::new(id.to_string()).map_err(|err| err.to_string())?;
    Ok(CheckpointRequest {
        at: phase_end.cloned(),
        kind: CheckpointKind::ChangeRequest,
        title: change_title(id, phase_end.map(|at| at.step.phase)),
        card: None,
        packet: CheckpointPacket::ChangeRequest(ChangeRequestPacket {
            from,
            card,
            text: format!(
                "Change workflow `{}` from its next phase on: {reason}",
                current.name
            ),
            paths: Vec::new(),
            why_you: "It changes how the run goes from its next phase on.".to_owned(),
            paused: Vec::new(),
        }),
        facts: EvidenceFacts::default(),
        agent_outputs: vec![reason.to_owned()],
        documents: vec![
            EvidenceDocument {
                label: PROPOSED.to_owned(),
                text: proposed,
                path: None,
                sha256: None,
            },
            EvidenceDocument {
                label: "Running workflow".to_owned(),
                text: running,
                path: None,
                sha256: None,
            },
        ],
        options: None,
        free_text: FreeText::Guidance,
        authors: Vec::new(),
    })
}

/// The workflow running now, once `workflow` is valid, differs from it, and leaves alone what
/// already ran. `Err` holds the reasons, one per line.
fn valid_change(host: &Host, id: &WorkflowId, workflow: &Workflow) -> Result<Workflow, String> {
    let (current, depth, takes_effect) = host
        .read(|state| {
            let record = state.workflows.by_id.get(id)?;
            Some((
                record.workflow.clone(),
                state.workflows.depth(id),
                record.change_from(),
            ))
        })
        .ok_or_else(|| format!("no workflow {id} is running"))?;
    runner::check(host, workflow, depth).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    if *workflow == current {
        return Err("the change is the workflow already running".to_owned());
    }
    keeps_what_ran(&current, workflow, takes_effect)?;
    Ok(current)
}

fn change_title(id: &WorkflowId, phase_end: Option<u32>) -> String {
    match phase_end {
        Some(phase) => format!("Workflow change · {id} · after phase {}", phase + 1),
        None => format!("Workflow change · {id}"),
    }
}

/// Code opens a crew run with Understand, so a proposal that leaves it out, such as an example
/// by name, keeps it as it ran. Any other phase that ran must be in the proposal as it was.
fn keep_what_ran(current: &Workflow, mut proposed: Workflow, from: u32) -> Workflow {
    let ran = &current.phases[..(from as usize).min(current.phases.len())];
    let only_understand = !ran.is_empty()
        && ran
            .iter()
            .all(|phase| phase.standard == StandardPhase::Understand);
    let left_out = !ran
        .iter()
        .any(|kept| proposed.phases.iter().any(|phase| phase.name == kept.name));
    if only_understand && left_out {
        let mut phases = ran.to_vec();
        phases.append(&mut proposed.phases);
        proposed.phases = phases;
    }
    proposed
}

/// A change takes effect from phase `from`. The phases before it ran, or are running, as they
/// were, so the change must keep them exactly.
fn keeps_what_ran(current: &Workflow, proposed: &Workflow, from: u32) -> Result<(), String> {
    for index in 0..from as usize {
        let Some(kept) = current.phases.get(index) else {
            break;
        };
        match proposed.phases.get(index) {
            Some(phase) if phase == kept => {}
            Some(_) => {
                return Err(format!(
                    "it changes phase {} ({}), which already started; a change may only change the phases after it",
                    index + 1,
                    kept.name
                ));
            }
            None => {
                return Err(format!(
                    "it drops phase {} ({}), which already started",
                    index + 1,
                    kept.name
                ));
            }
        }
    }
    Ok(())
}

/// Records `Workflow::Changed` when the change was accepted and isn't recorded yet. Checked
/// again here, since the run may have moved on while the change waited for you.
fn apply(
    host: &Host,
    id: &WorkflowId,
    workflow: Workflow,
    decided: &CheckpointDecided,
    reason: String,
) -> EngineResult<()> {
    if decided.action != AnswerAction::Approve {
        return Ok(());
    }
    let Some((current, takes_effect, applied)) = host.read(|state| {
        let record = state.workflows.by_id.get(id)?;
        Some((
            record.workflow.clone(),
            record.change_from(),
            record.changes.contains(&decided.id),
        ))
    }) else {
        return Err(EngineError::Refused(format!(
            "no workflow {id} was started"
        )));
    };
    if applied || workflow == current {
        return Ok(());
    }
    keeps_what_ran(&current, &workflow, takes_effect).map_err(|reason| {
        EngineError::Refused(format!(
            "the change to workflow {id} you accepted at {} wasn't made, since the run moved on: {reason}",
            decided.id
        ))
    })?;
    record(host, id, workflow, Some(decided.id.clone()), reason)
}

/// Records a change to workflow `id` from its next phase on: the new definition, its
/// declaration, each phase it adds, and why, so the view moves in place. `checkpoint` is the
/// approval it went through, if it needed one.
pub fn record(
    host: &Host,
    id: &WorkflowId,
    workflow: Workflow,
    checkpoint: Option<CheckpointId>,
    reason: String,
) -> EngineResult<()> {
    let Some(current) = host.read(|state| {
        state
            .workflows
            .by_id
            .get(id)
            .map(|record| record.workflow.clone())
    }) else {
        return Err(EngineError::Refused(format!(
            "no workflow {id} was started"
        )));
    };
    let version = runner::version(&workflow)?;
    let declared = WorkflowDeclared::of(id, &workflow);
    let added: Vec<DeclaredPhase> = declared
        .phases
        .iter()
        .filter(|phase| !current.phases.iter().any(|old| old.name == phase.title))
        .cloned()
        .collect();
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Changed(Box::new(WorkflowChanged {
            id: id.clone(),
            version,
            workflow,
            checkpoint,
            reason: reason.clone(),
        }))),
    )?;
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Declared(Box::new(declared))),
    )?;
    for phase in added {
        host.record(
            None,
            None,
            RunEventKind::Workflow(WorkflowEvent::PhaseAdded {
                id: id.clone(),
                phase,
                why: reason.clone(),
            }),
        )?;
    }
    if !reason.trim().is_empty() {
        host.record(
            None,
            None,
            RunEventKind::Workflow(WorkflowEvent::Note(RunNote {
                text: reason,
                workflow: Some(id.clone()),
            })),
        )?;
    }
    Ok(())
}

/// Called as a workflow change is decided, by you or your policy: an accepted one is recorded
/// at once, whoever waits for it. What goes wrong becomes a note in the log.
pub fn on_decided(host: &Host, opened: &CheckpointOpened, decided: &CheckpointDecided) {
    if !checkpoints::is_workflow_change(opened.kind, opened.card.as_ref())
        || decided.action != AnswerAction::Approve
    {
        return;
    }
    if let Err(err) = apply_opened(host, opened, decided)
        && let Err(unrecorded) = note(host, NoteLevel::Warn, err.to_string())
    {
        tracing::error!(%unrecorded, "a workflow change failed, and so did its note: {err}");
    }
}

fn apply_opened(
    host: &Host,
    opened: &CheckpointOpened,
    decided: &CheckpointDecided,
) -> EngineResult<()> {
    let CheckpointPacket::ChangeRequest(packet) = &opened.packet else {
        return Err(EngineError::Refused(format!(
            "checkpoint {} isn't a workflow change",
            opened.id
        )));
    };
    let id = WorkflowId::parse(packet.card.as_str()).ok_or_else(|| {
        EngineError::Refused(format!(
            "checkpoint {} names no workflow: {}",
            opened.id, packet.card
        ))
    })?;
    let evidence = checkpoints::evidence_of(host, opened)?;
    let proposed = evidence
        .documents
        .iter()
        .find(|document| document.label == PROPOSED)
        .ok_or_else(|| {
            EngineError::Refused(format!(
                "checkpoint {} holds no proposed workflow",
                opened.id
            ))
        })?;
    let workflow = yaml::parse(&proposed.text).map_err(EngineError::Refused)?;
    let reason = evidence.agent_outputs.first().cloned().unwrap_or_default();
    apply(host, &id, workflow, decided, reason)
}

/// When the engine starts: records each accepted workflow change that the log doesn't show as
/// made yet, such as one you accepted while the run was parked.
pub fn apply_decided(host: &Host) -> EngineResult<()> {
    let waiting: Vec<(CheckpointOpened, CheckpointDecided)> = host.read(|state| {
        let made: BTreeSet<&CheckpointId> = state
            .workflows
            .by_id
            .values()
            .flat_map(|record| record.changes.iter())
            .collect();
        state
            .checkpoints
            .by_id
            .values()
            .filter(|record| {
                checkpoints::is_workflow_change(record.opened.kind, record.opened.card.as_ref())
                    && record.withdrawn.is_none()
                    && !made.contains(&record.opened.id)
            })
            .filter_map(|record| Some((record.opened.clone(), record.decided.clone()?)))
            .collect()
    });
    for (opened, decided) in &waiting {
        on_decided(host, opened, decided);
    }
    Ok(())
}

fn note(host: &Host, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(None, None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// With `workflows.adapt_at_phase_ends` on a crew run, runs `DesignWorkflow` in a fresh session
/// with the running workflow as YAML and one line per finished phase or step. A different answer
/// is proposed like any change; an unchanged one does nothing.
pub async fn adapt_at_phase_end(host: &Host, id: &WorkflowId, phase: u32) -> EngineResult<()> {
    if !host.settings().workflows.adapt_at_phase_ends
        || !host.read(|state| runner::crew_run(state, id))
    {
        return Ok(());
    }
    let Some((current, progress, finished)) = host.read(|state| {
        let record = state.workflows.by_id.get(id)?;
        let finished: Vec<u32> = (0..=phase).filter(|p| record.phase_finished(*p)).collect();
        let progress = record
            .steps
            .iter()
            .filter(|step| step.end.is_some() && step.step.phase <= phase)
            .map(|step| {
                let phase_name = record
                    .workflow
                    .phases
                    .get(step.step.phase as usize)
                    .map_or("", |definition| definition.name.as_str());
                format!(
                    "{phase_name}: {} round {}: {}",
                    kind_name(step.kind),
                    step.round,
                    step.summary.as_deref().unwrap_or("done")
                )
            })
            .collect();
        Some((record.workflow.clone(), progress, finished))
    }) else {
        return Err(EngineError::Refused(format!(
            "no workflow {id} was started"
        )));
    };
    // Asked once per phase end. After a crash or a park, the ask replays from the log, and a
    // change you already decided is applied or dropped as you decided, never asked again.
    let title = change_title(id, Some(phase));
    let prior = host.read(|state| {
        state
            .checkpoints
            .by_id
            .values()
            .filter(|record| {
                record.opened.kind == CheckpointKind::ChangeRequest
                    && record.opened.title == title
                    && record.withdrawn.is_none()
            })
            .max_by_key(|record| record.since)
            .and_then(|record| record.decided.clone())
    });
    if prior
        .as_ref()
        .is_some_and(|decided| decided.action != AnswerAction::Approve)
    {
        return Ok(());
    }
    let at = StepAt {
        step: StepRef {
            workflow: id.clone(),
            phase,
            path: Vec::new(),
        },
        round: 1,
    };
    let mut input = design_input(
        host,
        Some(yaml::print_marked(&current, &finished).map_err(EngineError::Refused)?),
        progress,
    );
    let lead = lead(host);
    let examples = examples_part(host);
    for slot in 0..2 {
        let answered = host
            .ask::<DesignWorkflow>(Ask {
                slot,
                parts: vec![examples.clone()],
                ..Ask::new(
                    Some(at.clone()),
                    RoleName::PhaseLead,
                    lead.clone(),
                    DESIGN_WORKFLOW,
                    input.clone(),
                )
            })
            .await?;
        let proposed = match resolve(host, &answered.output) {
            // A phase end changes the plan, never the run's name.
            Ok(Some((_, mut proposed))) => {
                proposed.name.clone_from(&current.name);
                proposed.description.clone_from(&current.description);
                proposed.based_on.clone_from(&current.based_on);
                // The phases that ran stay as they were, so only the checks are reshaped.
                let from = host.read(|state| {
                    state
                        .workflows
                        .by_id
                        .get(id)
                        .map_or(0, |record| record.change_from())
                });
                shape::as_written(host, keep_what_ran(&current, proposed, from))
            }
            Ok(None) => return Ok(()),
            Err(error) => {
                input.errors = vec![error];
                continue;
            }
        };
        if proposed == current {
            return Ok(());
        }
        let why = answered.output.why.clone();
        // Before the go-ahead, a change is part of the lead's design, and the start card
        // approves it as it then stands.
        if prior.is_none() && host.read(|state| before_go_ahead(state, id)) {
            if let Err(errors) = valid_change(host, id, &proposed) {
                input.errors = errors.lines().map(str::to_owned).collect();
                continue;
            }
            let note = match why.trim() {
                "" => "The lead changed the workflow before anything was written".to_owned(),
                why => format!("The lead changed the workflow: {why}"),
            };
            return record(host, id, proposed, None, note);
        }
        let decided = match &prior {
            Some(decided) => decided.clone(),
            None => {
                let from = Party::Agent {
                    session: answered.session.clone(),
                    worker: answered.worker.clone(),
                };
                let request = match change_request(host, id, &proposed, &why, from, Some(&at)) {
                    Ok(request) => request,
                    Err(errors) => {
                        input.errors = errors.lines().map(str::to_owned).collect();
                        continue;
                    }
                };
                let opening =
                    if checkpoints::asks_you_about(host.settings(), &[checkpoints::CHANGE_TOPIC]) {
                        let because = checkpoints::yours_because("workflow changes");
                        checkpoints::open_yours(host, request, because)?
                    } else {
                        checkpoints::open(host, request)?
                    };
                let checkpoint = opening.id().clone();
                match opening.decided(host).await {
                    Ok(decided) => decided,
                    // Withdrawn, such as by a newer proposal for this phase end: no change.
                    Err(EngineError::Refused(reason)) if withdrawn(host, &checkpoint) => {
                        return note(
                            host,
                            NoteLevel::Info,
                            format!(
                                "The lead's change to workflow {id} was withdrawn, so it goes on unchanged: {reason}"
                            ),
                        );
                    }
                    Err(err) => return Err(err),
                }
            }
        };
        return match apply(host, id, proposed, &decided, why) {
            Err(EngineError::Refused(reason)) => note(host, NoteLevel::Warn, reason),
            other => other,
        };
    }
    note(
        host,
        NoteLevel::Warn,
        format!(
            "The lead's change to workflow {id} after phase {} broke the rules twice, so it goes on unchanged: {}",
            phase + 1,
            input.errors.join("; ")
        ),
    )
}

/// Nothing has been built yet, and the checkpoint that gives the go-ahead is still to come, so
/// it will show the workflow as changed.
fn before_go_ahead(state: &RunState, id: &WorkflowId) -> bool {
    fn checkpoint(steps: &[argonauts_core::workflow::Step], kind: CheckpointKind) -> bool {
        steps.iter().any(|step| {
            (step.kind == StepKind::Checkpoint && step.checkpoint == Some(kind))
                || checkpoint(&step.steps, kind)
        })
    }
    let Some(record) = state.workflows.by_id.get(id) else {
        return false;
    };
    let from = record.change_from() as usize;
    // The plan card gives the go-ahead, or the goal checkpoint before the work when no plan
    // card does and the goal isn't approved yet.
    let goal_to_come = state.goal.approved.is_none();
    state.cards.by_id.is_empty()
        && record.workflow.phases.iter().skip(from).any(|phase| {
            checkpoint(&phase.steps, CheckpointKind::Plan)
                || (goal_to_come && checkpoint(&phase.steps, CheckpointKind::Goal))
        })
}

fn withdrawn(host: &Host, checkpoint: &CheckpointId) -> bool {
    host.read(|state| {
        state
            .checkpoints
            .by_id
            .get(checkpoint)
            .is_some_and(|record| record.withdrawn.is_some())
    })
}

/// Before the run's workflow starts, with `workflows.lead_designs` or All hands on a crew run,
/// the lead writes the run's brief (`brief`): its name, description, phases with details,
/// labels, and done-when list, starting from an example (built in, or one of your files) or
/// composed when `allow_composed`. Code shapes it for the run (`shape`): Understand first when
/// it runs, the checkpoints and the checks, and a goal's first attempt. The brief and the
/// workflow are validated, and the errors go back to the lead once. An accepted brief names
/// the run. `None` keeps the preset's workflow, with a note saying why when the lead's answer
/// didn't hold.
pub async fn design_for_task(host: &Host) -> EngineResult<Option<(Option<WorkflowRef>, Workflow)>> {
    let settings = host.settings();
    let solo = host.read(runner::run_mode) == Some(RunMode::Solo);
    if !settings.lead_designs() || solo {
        return Ok(None);
    }
    let mut input = design_input(host, None, Vec::new());
    let lead = lead(host);
    let examples = examples_part(host);
    let names: Vec<String> = self::examples(host)
        .into_iter()
        .map(|example| example.name)
        .collect();
    for slot in 0..2 {
        let answered = host
            .ask::<DesignWorkflow>(Ask {
                slot,
                parts: vec![examples.clone()],
                ..Ask::new(
                    None,
                    RoleName::PhaseLead,
                    lead.clone(),
                    DESIGN_WORKFLOW,
                    input.clone(),
                )
            })
            .await;
        let answered = match answered {
            Ok(answered) => answered,
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(err) => {
                note(
                    host,
                    NoteLevel::Warn,
                    format!(
                        "The lead couldn't design the workflow, so `{}` runs. {}",
                        settings.workflow,
                        sentence(&err.to_string())
                    ),
                )?;
                return Ok(None);
            }
        };
        let brief = &answered.output;
        let (from, mut workflow) = match resolve(host, brief) {
            Ok(Some(resolved)) => resolved,
            Ok(None) => return Ok(None),
            Err(error) => {
                input.errors = vec![error];
                continue;
            }
        };
        if brief.workflow.is_some()
            && brief.based_on.is_none()
            && !settings.workflows.allow_composed
        {
            input.errors = vec!["composing is off; start from one of the examples".to_owned()];
            continue;
        }
        input.errors = brief::problems(brief, &workflow, &names);
        if !input.errors.is_empty() {
            continue;
        }
        brief::apply_identity(&mut workflow, brief);
        workflow.name = crate::workflow::naming::unique_name(host, &workflow.name);
        let first_attempt = match &brief.first_attempt {
            Some(plan) if shape::run_style(host) == RunStyle::Goal => {
                match crate::goal::pursue::first_attempt(plan) {
                    Ok(phase) => Some(phase),
                    Err(error) => {
                        input.errors = vec![format!("`first_attempt`: {error}")];
                        continue;
                    }
                }
            }
            _ => None,
        };
        let workflow = shape::for_run(host, workflow, first_attempt);
        match runner::check(host, &workflow, 0) {
            Ok(()) => {
                name_run(host, &workflow, &brief.done_when)?;
                return Ok(Some((from, workflow)));
            }
            Err(errors) => input.errors = errors.iter().map(ToString::to_string).collect(),
        }
    }
    note(
        host,
        NoteLevel::Warn,
        format!(
            "The lead's workflow broke the rules twice, so `{}` runs. {}",
            settings.workflow,
            sentence(&input.errors.join("; "))
        ),
    )?;
    Ok(None)
}

/// A reason as a sentence of its own, with one closing period. A reason that starts with a
/// step or a key keeps its spelling.
fn sentence(reason: &str) -> String {
    let reason = reason.trim().trim_end_matches('.');
    if reason.is_empty() {
        return String::new();
    }
    format!("{}.", sentence_case(reason))
}

pub(crate) fn lead(host: &Host) -> argonauts_core::names::WorkerRef {
    host.read(|state| crate::lead::phase_lead(state, host.settings()))
        .unwrap_or_else(|| host.settings().default_agent.clone())
}

fn design_input(host: &Host, current: Option<String>, progress: Vec<String>) -> DesignInput {
    let (request, goal) = host.read(|state| {
        let request = state
            .run
            .started
            .as_ref()
            .map(|started| started.request.clone())
            .unwrap_or_default();
        let goal = state
            .goal
            .approved
            .as_ref()
            .and_then(|goal| serde_json::to_value(goal).ok())
            .map(goal_lines)
            .unwrap_or_default();
        (request, goal)
    });
    DesignInput {
        request,
        goal,
        // They follow as their own part, where their files read as YAML.
        examples: Vec::new(),
        step_kinds: STEP_KINDS
            .iter()
            .map(|(kind, line)| format!("{}: {line}", kind_name(*kind)))
            .collect(),
        allow_composed: host.settings().workflows.allow_composed,
        errors: Vec::new(),
        current,
        progress,
        style: Some(shape::run_style(host)).filter(|style| *style == RunStyle::Goal),
    }
}

/// The examples for the lead, best first: one `### name` section each, with what it's for,
/// how it did on similar tasks, and its file.
fn examples_part(host: &Host) -> InputPart {
    let mut text = String::from("## Example workflows\n");
    for example in examples(host) {
        let record = match (example.runs, example.passed) {
            (0, _) => "No similar runs yet.".to_owned(),
            (runs, passed) => format!("Passed {passed} of {runs} similar runs."),
        };
        let _ = write!(
            text,
            "\n### {}\n\n{}\n{record}\n\n```yaml\n{}\n```\n",
            example.name,
            example.description.trim(),
            example.yaml.trim_end()
        );
    }
    InputPart {
        kind: InputPartKind::StepInput,
        label: "example workflows".to_owned(),
        approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
        text,
        source: None,
    }
}

/// Every workflow the lead may pick, by the name `lookup` finds it under: your project's files,
/// then your global ones, then the built-ins. A file that doesn't parse isn't offered;
/// naming it would fail with its error anyway. Each carries its runs and passes from the outcome
/// log's run lines, and they arrive best first.
pub fn examples(host: &Host) -> Vec<WorkflowExample> {
    let stats = crate::retro::outcomes::read_all(&crate::divide::repo_state_dir(host))
        .map(|lines| argonauts_learning::outcomes::workflow_stats(&lines))
        .unwrap_or_else(|err| {
            tracing::warn!("past runs unavailable for the workflow examples: {err}");
            Default::default()
        });
    let mut seen = BTreeSet::new();
    let mut examples = Vec::new();
    let mut offer = |name: &str, text: String, workflow: Workflow| {
        if seen.insert(name.to_owned()) {
            let version = runner::version(&workflow).unwrap_or_default();
            let past = stats
                .get(&(name.to_owned(), version))
                .copied()
                .unwrap_or_default();
            examples.push(WorkflowExample {
                name: name.to_owned(),
                description: workflow.description,
                yaml: text,
                runs: past.runs,
                passed: past.passed,
            });
        }
    };
    for dir in runner::workflow_dirs(host) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<_> = entries
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "yaml")
            })
            .collect();
        files.sort();
        for path in files {
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(workflow) = yaml::parse(&text) {
                offer(name, text, workflow);
            }
        }
    }
    for (name, text) in builtin::BUILTIN {
        if let Ok(workflow) = builtin::load(name) {
            offer(name, (*text).to_owned(), workflow);
        }
    }
    rank_examples(&mut examples);
    examples
}

/// Best first: more passes, then a better pass rate over the runs. Workflows with no runs keep
/// their place after them, so the order is still yours, then global, then built in.
pub fn rank_examples(examples: &mut [WorkflowExample]) {
    let rate =
        |example: &WorkflowExample| f64::from(example.passed + 1) / f64::from(example.runs + 2);
    examples.sort_by(|a, b| {
        (b.runs > 0)
            .cmp(&(a.runs > 0))
            .then(b.passed.cmp(&a.passed))
            .then(rate(b).total_cmp(&rate(a)))
    });
}

/// The goal's summary and each criterion, one line each.
fn goal_lines(goal: Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(summary) = goal.get("summary").and_then(Value::as_str) {
        lines.push(summary.to_owned());
    }
    for criterion in goal
        .get("criteria")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match criterion.get("text").and_then(Value::as_str) {
            Some(text) => lines.push(text.to_owned()),
            None => lines.push(criterion.to_string()),
        }
    }
    lines
}

const STEP_KINDS: &[(StepKind, &str)] = &[
    (StepKind::Restate, "each agent restates the goal"),
    (
        StepKind::Differ,
        "compare restatements and tag the differences",
    ),
    (
        StepKind::Draft,
        "one or more agents write a plan or an answer, blind to each other if set",
    ),
    (StepKind::Map, "quote-checked comparison of drafts"),
    (
        StepKind::FanOut,
        "the same task to several workers or roles, in parallel",
    ),
    (
        StepKind::ForEach,
        "one round of sub-steps per item of an earlier step's output",
    ),
    (
        StepKind::Review,
        "review a diff or a result against criteria, by another family",
    ),
    (StepKind::Judge, "score candidates, never the judge's own"),
    (StepKind::Refute, "try to disprove a finding or a claim"),
    (StepKind::Synthesize, "combine results into one"),
    (
        StepKind::Build,
        "cards: an owner in a worktree, then its gates and review",
    ),
    (StepKind::Gate, "run the configured gate commands"),
    (
        StepKind::LoopUntil,
        "repeat steps until a condition holds, with a hard maximum",
    ),
    (
        StepKind::Checkpoint,
        "the goal or plan checkpoint, where you or your policy decide",
    ),
    (StepKind::Merge, "integration and the merge checkpoint"),
    (StepKind::Workflow, "run another workflow as one step"),
];

/// The workflow the lead's brief names: its own file, or the example it started from with its
/// params filled, before code frames it. `None` when it named neither.
fn resolve(
    host: &Host,
    output: &DesignOutput,
) -> Result<Option<(Option<WorkflowRef>, Workflow)>, String> {
    let mut values = WorkflowParams::default();
    for setting in &output.params {
        let value: Value = serde_saphyr::from_str(&setting.value)
            .unwrap_or_else(|_| Value::String(setting.value.clone()));
        values.0.insert(setting.name.clone(), value);
    }
    let from = output.based_on.as_deref().map(WorkflowRef::new);
    let workflow = match (&output.workflow, &output.based_on) {
        (Some(text), _) => yaml::parse(text)?,
        (None, Some(name)) => runner::lookup(host, name).map_err(|err| err.to_string())?,
        (None, None) => return Ok(None),
    };
    let filled = params::fill(&workflow, &values).map_err(|errors| errors.join("\n"))?;
    Ok(Some((from, filled)))
}

/// Records the run's name and description from its workflow, once the lead's brief named it,
/// unless the run already goes by that name.
fn name_run(host: &Host, workflow: &Workflow, done_when: &[Criterion]) -> EngineResult<()> {
    let (named, style) = host.read(|state| {
        (
            state.run.named.clone(),
            state.run.named.as_ref().map(|named| named.style),
        )
    });
    if named.as_ref().is_some_and(|named| {
        named.name == workflow.name
            && named.description == workflow.description
            && named.done_when == done_when
    }) {
        return Ok(());
    }
    let style = style.unwrap_or_else(|| crate::workflow::naming::style(host));
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::Named(RunNamed {
            name: workflow.name.clone(),
            description: workflow.description.clone(),
            style,
            based_on: workflow.based_on.clone(),
            done_when: done_when.to_vec(),
        })),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin(name: &str) -> Workflow {
        builtin::load(name).unwrap_or_else(|err| panic!("{err}"))
    }

    /// `coordinated-crew` at the end of Plan takes a change from phase 3 on. Folding Goal and
    /// Plan together would shift Build onto a finished index, so it's refused.
    #[test]
    fn a_change_keeps_the_phases_that_started() {
        let current = builtin("coordinated-crew");
        let mut folded = current.clone();
        let plan = folded.phases.remove(1);
        folded.phases[0].steps.extend(plan.steps);
        let mut check = folded.phases[1].clone();
        check.name = "Extra check".to_owned();
        folded.phases.insert(2, check);
        let refused = keeps_what_ran(&current, &folded, 2).unwrap_err();
        assert!(refused.contains("phase 1 (Understand)"), "{refused}");

        let mut dropped = current.clone();
        dropped.phases.truncate(1);
        let refused = keeps_what_ran(&current, &dropped, 2).unwrap_err();
        assert!(refused.contains("drops phase 2 (Plan)"), "{refused}");

        let mut later = current.clone();
        let mut check = later.phases[2].clone();
        check.name = "Extra check".to_owned();
        later.phases.insert(3, check);
        assert_eq!(keeps_what_ran(&current, &later, 2), Ok(()));
        // Once Build has started too, the same change still keeps it; one that edits Build
        // no longer does.
        assert_eq!(keeps_what_ran(&current, &later, 3), Ok(()));
        let mut edited = current.clone();
        edited.phases[2].name = "Build again".to_owned();
        assert_eq!(keeps_what_ran(&current, &edited, 2), Ok(()));
        assert!(keeps_what_ran(&current, &edited, 3).is_err());
    }
}
