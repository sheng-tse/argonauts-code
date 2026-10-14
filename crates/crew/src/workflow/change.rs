//! Changing a running workflow from its next phase on: proposed from outside
//! (`EngineCommand::ProposeWorkflowChange`) or by the main agent at a phase end. Both go through
//! the validator and the `change_request` approval, and an accepted one is recorded when it's
//! decided. Also the main agent's design for a task, before the run's workflow starts.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalKind, ApprovalOpened, ApprovalPacket,
    ChangeRequestPacket, FreeText,
};
use argonauts_core::events::goal::Criterion;
use argonauts_core::events::run::{
    CheckedInstruction, InstructionOutcome, Party, RunLifecycle, RunMode, RunNamed,
};
use argonauts_core::events::session::{EndReason, InputPart, InputPartKind};
use argonauts_core::events::workflow::{
    DeclaredPhase, RunNote, WorkflowChanged, WorkflowDeclared, WorkflowEvent,
};
use argonauts_core::log::{Note, NoteLevel};
use argonauts_core::names::{ApprovalId, ModelRef, RoleName, SessionKey, WorkflowId};
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::settings::workflows::{OrchestratorDesign, RunStyle};
use argonauts_core::steps::StepError;
use argonauts_core::steps::design::{
    DesignFix, DesignInput, DesignOutput, DesignWorkflow, FixDesign, WorkflowExample,
};
use argonauts_core::steps::intake::InstructionKind;
use argonauts_core::workflow::{
    StandardPhase, StepKind, StepRef, Workflow, WorkflowParams, WorkflowRef,
};
use argonauts_core::workflow_files;

use crate::approvals::{self, ApprovalRequest};
use crate::engine::host::{Answered, Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::prompts::workflow::{DESIGN_WORKFLOW, FIX_DESIGN};
use crate::state::RunState;
use crate::workflow::brief::Problem;
use crate::workflow::validate::kind_name;
use crate::workflow::{StepAt, brief, builtin, params, runner, shape, sizing, yaml};

/// The evidence document that holds the proposed workflow as YAML.
const PROPOSED: &str = "Proposed workflow";

/// Validates the change and opens its `change_request` approval without waiting for it.
/// `Err` holds the validator's errors, one per line. Once you accept it, `on_decided` records
/// `Workflow::Changed` and the runner takes it from the next phase on.
pub async fn propose(
    host: &Host,
    target: Option<WorkflowId>,
    workflow: Workflow,
    reason: String,
) -> Result<ApprovalId, String> {
    let id = target
        .or_else(|| host.read(|state| state.workflows.root.clone()))
        .ok_or_else(|| "no workflow is running".to_owned())?;
    let request = change_request(host, &id, &workflow, &reason, Party::User, None)?;
    let opening = approvals::open(host, request).map_err(|err| err.to_string())?;
    Ok(opening.id().clone())
}

/// The approval for a change to workflow `id`, once the change is valid, is different from what
/// runs, and leaves alone what already ran. `Err` holds the reasons, one per line. `phase_end`
/// is where the main agent proposed it.
fn change_request(
    host: &Host,
    id: &WorkflowId,
    workflow: &Workflow,
    reason: &str,
    from: Party,
    phase_end: Option<&StepAt>,
) -> Result<ApprovalRequest, String> {
    let current = valid_change(host, id, workflow)?;
    let proposed = yaml::print(workflow)?;
    let running = yaml::print(&current)?;
    let task = TaskId::new(id.to_string()).map_err(|err| err.to_string())?;
    Ok(ApprovalRequest {
        at: phase_end.cloned(),
        kind: ApprovalKind::ChangeRequest,
        title: change_title(id, phase_end.map(|at| at.step.phase)),
        task: None,
        packet: ApprovalPacket::ChangeRequest(ChangeRequestPacket {
            from,
            task,
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

/// The workflow running now, once `workflow` is valid, is different from it, and leaves alone what
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
    decided: &ApprovalDecided,
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
/// declaration, each phase it adds, and why, so the view moves in place. `approval` is the
/// approval it went through, if it needed one.
pub fn record(
    host: &Host,
    id: &WorkflowId,
    workflow: Workflow,
    approval: Option<ApprovalId>,
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
        .filter(|phase| !current.phases.iter().any(|old| old.name == phase.name))
        .cloned()
        .collect();
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Changed(Box::new(WorkflowChanged {
            id: id.clone(),
            version,
            workflow,
            approval,
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
                left: false,
            })),
        )?;
    }
    Ok(())
}

/// Called as a workflow change is decided, by you or your policy: an accepted one is recorded
/// at once, whoever waits for it. What goes wrong becomes a note in the log.
pub fn on_decided(host: &Host, opened: &ApprovalOpened, decided: &ApprovalDecided) {
    if !approvals::is_workflow_change(opened.kind, opened.task.as_ref())
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
    opened: &ApprovalOpened,
    decided: &ApprovalDecided,
) -> EngineResult<()> {
    let ApprovalPacket::ChangeRequest(packet) = &opened.packet else {
        return Err(EngineError::Refused(format!(
            "approval {} isn't a workflow change",
            opened.id
        )));
    };
    let id = WorkflowId::parse(packet.task.as_str()).ok_or_else(|| {
        EngineError::Refused(format!(
            "approval {} names no workflow: {}",
            opened.id, packet.task
        ))
    })?;
    let evidence = approvals::evidence_of(host, opened)?;
    let proposed = evidence
        .documents
        .iter()
        .find(|document| document.label == PROPOSED)
        .ok_or_else(|| {
            EngineError::Refused(format!("approval {} holds no proposed workflow", opened.id))
        })?;
    let workflow = yaml::parse(&proposed.text).map_err(EngineError::Refused)?;
    let reason = evidence.agent_outputs.first().cloned().unwrap_or_default();
    apply(host, &id, workflow, decided, reason)
}

/// When the engine starts: records each accepted workflow change that the log doesn't show as
/// made yet, such as one you accepted while the run was waiting.
pub fn apply_decided(host: &Host) -> EngineResult<()> {
    let waiting: Vec<(ApprovalOpened, ApprovalDecided)> = host.read(|state| {
        let made: BTreeSet<&ApprovalId> = state
            .workflows
            .by_id
            .values()
            .flat_map(|record| record.changes.iter())
            .collect();
        state
            .approvals
            .by_id
            .values()
            .filter(|record| {
                approvals::is_workflow_change(record.opened.kind, record.opened.task.as_ref())
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

/// A note in one plain line, with the raw reasons behind it for Ctrl+O.
fn note_with_detail(host: &Host, level: NoteLevel, line: &str, detail: &str) -> EngineResult<()> {
    let note = Note::with_detail(level, line, detail);
    host.record(None, None, RunEventKind::Note(note))?;
    Ok(())
}

/// With `workflows.adapt_at_phase_ends` on a crew run, runs `DesignWorkflow` in a fresh session
/// with the running workflow as YAML and one line per finished phase or step. A different answer
/// is proposed like any change; an unchanged one does nothing. One code refuses goes back once
/// to the same session.
pub async fn adapt_at_phase_end(host: &Host, id: &WorkflowId, phase: u32) -> EngineResult<()> {
    if !host.settings().workflows.adapt_at_phase_ends
        || !host.read(|state| runner::crew_run(state, id) && !crate::script::runs(state, id))
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
    // Asked once per phase end. After a crash or a wait, the ask replays from the log, and a
    // change you already decided is applied or dropped as you decided, never asked again.
    let title = change_title(id, Some(phase));
    let prior = host.read(|state| {
        state
            .approvals
            .by_id
            .values()
            .filter(|record| {
                record.opened.kind == ApprovalKind::ChangeRequest
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
    let tie_breaker = tie_breaker(host);
    // Where the main agent may only size your workflow, it keeps it here too: it may size what's
    // left of it, never run another workflow in its place, unless your request said how to run it.
    let sizes_only = host.settings().orchestrator_designs() == OrchestratorDesign::Size
        && input.as_you_asked.is_empty();
    let examples = sizing::part(host).unwrap_or_else(|| examples_part(host));
    let names = example_names(host);
    let mut refused: Option<SessionKey> = None;
    for slot in 0..2 {
        let asked = Designing {
            slot,
            at: Some(at.clone()),
            tie_breaker: &tie_breaker,
            refused: refused.as_ref(),
            input: &input,
            examples: &examples,
            names: &names,
        };
        let answered = ask_design(host, asked).await?;
        // A change code refuses goes back to the session that proposed it, and the first time
        // a `›` note says why, as it does for a brief.
        refused = resumable(host, &answered);
        let resumed = refused.is_some();
        let mut send_back = |plainly: &str, errors: Vec<String>| {
            input.errors = errors;
            if slot > 0 {
                return Ok(());
            }
            let what = format!("The main agent's change after phase {}", phase + 1);
            let line = fixing(&what, plainly, resumed);
            let detail = input.errors.join("\n");
            say_once(host, Some(&answered.session), &line, &detail)
        };
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
                let proposed = shape::as_written(host, keep_what_ran(&current, proposed, from));
                if sizes_only && let Err(error) = sizing::keeps(host, &current, &proposed) {
                    send_back("went beyond sizing your workflow", vec![error])?;
                    continue;
                }
                proposed
            }
            Ok(None) => return Ok(()),
            Err(error) => {
                send_back("wrote a workflow code couldn't read", vec![error])?;
                continue;
            }
        };
        if proposed == current {
            return Ok(());
        }
        let why = answered.output.why.clone();
        // Before the go-ahead, a change is part of the main agent's design, and the start question
        // approves it as it then stands.
        if prior.is_none() && host.read(|state| before_go_ahead(state, id)) {
            if let Err(errors) = valid_change(host, id, &proposed) {
                let errors = errors.lines().map(str::to_owned).collect();
                send_back(BROKE, errors)?;
                continue;
            }
            let note = match why.trim() {
                "" => "The main agent changed the workflow before anything was written".to_owned(),
                why => format!("The main agent changed the workflow: {why}"),
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
                        let errors = errors.lines().map(str::to_owned).collect();
                        send_back(BROKE, errors)?;
                        continue;
                    }
                };
                let opening = if approvals::keeps_yours(
                    host,
                    ApprovalKind::ChangeRequest,
                    &[approvals::CHANGE_TOPIC],
                ) {
                    let because = approvals::yours_because("workflow changes");
                    approvals::open_yours(host, request, because)?
                } else {
                    approvals::open(host, request)?
                };
                let approval = opening.id().clone();
                match opening.decided(host).await {
                    Ok(decided) => decided,
                    // Withdrawn, such as by a newer proposal for this phase end: no change.
                    Err(EngineError::Refused(reason)) if withdrawn(host, &approval) => {
                        return note(
                            host,
                            NoteLevel::Info,
                            format!(
                                "The main agent's change to workflow {id} was withdrawn, so it goes on unchanged: {reason}"
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
    let line = format!(
        "The main agent's change after phase {} broke the rules twice, so the workflow goes on \
         unchanged.",
        phase + 1
    );
    note_with_detail(host, NoteLevel::Warn, &line, &input.errors.join("\n"))
}

/// What a change that breaks the workflow's rules did, in plain words.
const BROKE: &str = "wrote a workflow that breaks its rules";

/// Nothing has been built yet, and the approval that gives the go-ahead is still to come, so
/// it will show the workflow as changed.
fn before_go_ahead(state: &RunState, id: &WorkflowId) -> bool {
    fn approval(steps: &[argonauts_core::workflow::Step], kind: ApprovalKind) -> bool {
        steps.iter().any(|step| {
            (step.kind == StepKind::Approval && step.approval == Some(kind))
                || approval(&step.steps, kind)
        })
    }
    let Some(record) = state.workflows.by_id.get(id) else {
        return false;
    };
    let from = record.change_from() as usize;
    // The plan question gives the go-ahead, or the Understand approval before the work when no plan
    // question does and the goal isn't approved yet.
    let goal_to_come = state.goal.approved.is_none();
    state.tasks.by_id.is_empty()
        && record.workflow.phases.iter().skip(from).any(|phase| {
            approval(&phase.steps, ApprovalKind::Plan)
                || (goal_to_come && approval(&phase.steps, ApprovalKind::Understand))
        })
}

fn withdrawn(host: &Host, approval: &ApprovalId) -> bool {
    host.read(|state| {
        state
            .approvals
            .by_id
            .get(approval)
            .is_some_and(|record| record.withdrawn.is_some())
    })
}

/// Before the run's workflow starts, with `workflows.orchestrator_designs` or Ultracode on a crew
/// run, the main agent writes the run's brief (`brief`): its name, description, phases with
/// details, labels, and done-when list, starting from an example (built in, or one of your files)
/// or composed when `allow_composed`. Code shapes it for the run (`shape`): Understand first when
/// it runs, the approvals and the checks, and a goal's first try. The brief and the workflow are
/// validated, and what code refuses goes back once to the same main agent session, which fixes it
/// with what it already read. An accepted brief names the run. `None` keeps the chosen workflow,
/// with a note saying why when the main agent's answer didn't hold.
pub async fn design_for_task(host: &Host) -> EngineResult<Option<(Option<WorkflowRef>, Workflow)>> {
    let settings = host.settings();
    let captain = host.read(runner::run_mode) == Some(RunMode::Captain);
    if settings.orchestrator_designs() == OrchestratorDesign::Off || captain {
        return Ok(None);
    }
    let mut input = design_input(host, None, Vec::new());
    let tie_breaker = tie_breaker(host);
    // Where the main agent may only size your workflow, it sees only the chosen workflow.
    let examples = sizing::part(host).unwrap_or_else(|| examples_part(host));
    let names = example_names(host);
    let mut refused: Option<SessionKey> = None;
    let mut refusals = 0;
    for slot in 0..2 {
        let asked = Designing {
            slot,
            at: None,
            tie_breaker: &tie_breaker,
            refused: refused.as_ref(),
            input: &input,
            examples: &examples,
            names: &names,
        };
        let answered = match ask_design(host, asked).await {
            Ok(answered) => answered,
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            // A session that ended without a brief has nothing to fix, so a new one writes it.
            Err(err) if slot == 0 && ended_early(&err) => {
                let line = "The main agent's session stopped before its brief was done, so a new \
                            session is writing it.";
                // A run's brief is written once, so this is said once in its log.
                say_once(host, None, line, &err.to_string())?;
                continue;
            }
            Err(err) => {
                // Said plainly; what the validator or the session said waits for Ctrl+O.
                let (what, detail) = match &err {
                    EngineError::Step(StepError::SchemaMismatch { errors, .. }) => (
                        "The main agent's design came back incomplete",
                        format!("output didn't match the schema:\n{}", errors.join("\n")),
                    ),
                    other => (
                        "The main agent couldn't design the workflow",
                        other.to_string(),
                    ),
                };
                let line = format!(
                    "{what}, so {} runs as written.",
                    runner::phases_in_use(host)
                );
                note_with_detail(host, NoteLevel::Warn, &line, &detail)?;
                return Ok(None);
            }
        };
        let brief = &answered.output;
        let problems = match accept(host, brief, &names, &input) {
            Ok(Some((sizing, workflow))) => {
                if let Some(line) = sizing::line(&sizing.changes, &brief.why) {
                    note(host, NoteLevel::Info, line)?;
                }
                name_run(host, &workflow, &brief.done_when)?;
                return Ok(Some((sizing.from, workflow)));
            }
            Ok(None) => return Ok(None),
            Err(problems) => problems,
        };
        refusals += 1;
        input.errors = problems
            .iter()
            .map(|problem| problem.line.clone())
            .collect();
        refused = resumable(host, &answered);
        if slot == 0 {
            let plainly = problems
                .first()
                .map_or("broke a rule", |problem| problem.plainly.as_str());
            let line = fixing("The main agent's brief", plainly, refused.is_some());
            let about = Some(&answered.session);
            say_once(host, about, &line, &input.errors.join("\n"))?;
        }
    }
    let line = if refusals > 1 {
        format!(
            "The main agent's workflow broke the rules twice, so {} runs as written.",
            runner::phases_in_use(host)
        )
    } else {
        format!(
            "The main agent's workflow broke the rules, so {} runs as written.",
            runner::phases_in_use(host)
        )
    };
    note_with_detail(host, NoteLevel::Warn, &line, &input.errors.join("\n"))?;
    Ok(None)
}

/// What code makes of the main agent's brief: the workflow it runs, as sized against your workflow;
/// `None` to keep the chosen workflow; or what code refused, for the main agent to fix.
fn accept(
    host: &Host,
    brief: &DesignOutput,
    names: &[String],
    input: &DesignInput,
) -> Result<Option<(sizing::Sizing, Workflow)>, Vec<Problem>> {
    let one = |plainly: &str, line: String| vec![Problem::new(plainly, line)];
    let (from, mut workflow) = match resolve(host, brief) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return Ok(None),
        Err(error) => return Err(one("wrote a workflow code couldn't read", error)),
    };
    if brief.workflow.is_some()
        && brief.based_on.is_none()
        && !host.settings().workflows.allow_composed
    {
        return Err(one(
            "composed a workflow while composing is off",
            "composing is off; start from one of the examples".to_owned(),
        ));
    }
    let problems = brief::problems(brief, &workflow, names);
    if !problems.is_empty() {
        return Err(problems);
    }
    // The run keeps your workflow, sized, unless `workflows.orchestrator_designs = "any"` or your
    // request said how to run it.
    let asked = !input.as_you_asked.is_empty();
    let sizing = sizing::check(host, from.as_ref(), &workflow, asked)
        .map_err(|error| one("went beyond sizing your workflow", error))?;
    brief::apply_identity(&mut workflow, brief);
    workflow.name = crate::workflow::naming::unique_name(host, &workflow.name);
    let first_try = match &brief.first_try {
        Some(plan) if shape::run_style(host) == RunStyle::Goal => {
            Some(crate::goal::pursue::first_try(plan).map_err(|error| {
                one(
                    "planned a first try code couldn't read",
                    format!("`first_try`: {error}"),
                )
            })?)
        }
        _ => None,
    };
    let workflow = shape::for_run(host, workflow, first_try);
    runner::check(host, &workflow, 0).map_err(|errors| {
        errors
            .iter()
            .map(|error| Problem::new(BROKE, error.to_string()))
            .collect::<Vec<_>>()
    })?;
    Ok(Some((sizing, workflow)))
}

/// One ask for the main agent's design.
struct Designing<'a> {
    slot: u32,
    /// The phase end it adapts the workflow at, or `None` for the run's brief.
    at: Option<StepAt>,
    tie_breaker: &'a ModelRef,
    /// The session whose design code refused, when it can carry on.
    refused: Option<&'a SessionKey>,
    input: &'a DesignInput,
    examples: &'a InputPart,
    /// The names the main agent may start from, so `based_on` is read as one of them.
    names: &'a [String],
}

/// The whole prompt the first time. After code refused a design, the same session hears only
/// what it refused and what the brief needs, so it keeps what it read of the repository.
async fn ask_design(host: &Host, asked: Designing<'_>) -> EngineResult<Answered<DesignOutput>> {
    let Designing {
        slot,
        at,
        tie_breaker,
        refused,
        input,
        examples,
        names,
    } = asked;
    let mut answered = match refused {
        Some(session) => {
            let fix = DesignFix {
                errors: input.errors.clone(),
            };
            host.ask::<FixDesign>(Ask {
                slot,
                resume: Some(session.clone()),
                ..Ask::new(
                    at,
                    RoleName::TieBreaker,
                    tie_breaker.clone(),
                    FIX_DESIGN,
                    fix,
                )
            })
            .await?
        }
        None => {
            host.ask::<DesignWorkflow>(Ask {
                slot,
                parts: vec![examples.clone()],
                ..Ask::new(
                    at,
                    RoleName::TieBreaker,
                    tie_breaker.clone(),
                    DESIGN_WORKFLOW,
                    input.clone(),
                )
            })
            .await?
        }
    };
    answered.output.read_based_on(names);
    Ok(answered)
}

/// A `›` note on why the main agent is asked again about what session `about` answered, written
/// once however often a stopped run replays to here.
fn say_once(host: &Host, about: Option<&SessionKey>, line: &str, detail: &str) -> EngineResult<()> {
    if host.read(|state| state.said_after(line, about)) {
        return Ok(());
    }
    note_with_detail(host, NoteLevel::Info, line, detail)
}

/// The note's line: what was refused, in plain words, and who fixes it.
fn fixing(what: &str, plainly: &str, resumed: bool) -> String {
    if resumed {
        format!("{what} {plainly}; it's fixing that.")
    } else {
        format!(
            "{what} {plainly}; a new session is fixing that, since the main agent's session can't be \
             resumed."
        )
    }
}

/// The session that gave a refused design, when its CLI can carry it on.
fn resumable(host: &Host, answered: &Answered<DesignOutput>) -> Option<SessionKey> {
    let resumes = host
        .services()
        .workers
        .get(&answered.worker)
        .is_ok_and(|worker| worker.capabilities().resume);
    (resumes && answered.end.cli_session.is_some()).then(|| answered.session.clone())
}

/// A session that stopped on its own before it answered, which a new one may get past: its
/// context filled up. An agent's failure never ends up here, since the ask goes again or its job
/// falls back by itself, and a stop of yours ends the same way however often it's asked.
fn ended_early(err: &EngineError) -> bool {
    matches!(
        err,
        EngineError::Step(StepError::Session(end)) if end.reason == EndReason::ContextFull
    )
}

/// The tie-breaker of the phase the run is in, and the main agent before it has one.
pub(crate) fn tie_breaker(host: &Host) -> argonauts_core::names::ModelRef {
    host.read(|state| crate::tie_breaker::tie_breaker(state, host.settings()))
        .unwrap_or_else(|| host.settings().crew.main.clone())
}

fn design_input(host: &Host, current: Option<String>, progress: Vec<String>) -> DesignInput {
    let (request, as_you_asked, goal) = host.read(|state| {
        let started = state.run.started.as_ref();
        let request = started
            .map(|started| started.request.clone())
            .unwrap_or_default();
        let as_you_asked = started
            .map(|started| musts(&started.instructions))
            .unwrap_or_default();
        let goal = state
            .goal
            .approved
            .as_ref()
            .and_then(|goal| serde_json::to_value(goal).ok())
            .map(goal_lines)
            .unwrap_or_default();
        (request, as_you_asked, goal)
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
        as_you_asked,
    }
}

/// What the main agent must keep from the request: each followed instruction as the start line
/// said it, and words meant for the main agent as the request put them.
fn musts(instructions: &[CheckedInstruction]) -> Vec<String> {
    instructions
        .iter()
        .filter_map(|instruction| match &instruction.outcome {
            InstructionOutcome::Followed { .. }
                if instruction.kind == InstructionKind::ForTheMainAgent =>
            {
                Some(instruction.says.clone())
            }
            InstructionOutcome::Followed { said, .. } if !said.trim().is_empty() => {
                Some(said.clone())
            }
            _ => None,
        })
        .collect()
}

/// The examples for the main agent, best first: one `### name` section each, with what it's for,
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

/// Every workflow the main agent may pick, by the name `lookup` finds it under: your project's
/// files, then your global ones, then the built-ins. A file that doesn't parse isn't offered;
/// naming it would fail with its error anyway. Each carries its runs and passes from the outcome
/// log's run lines, and they arrive best first.
pub fn examples(host: &Host) -> Vec<WorkflowExample> {
    let stats = crate::run_review::outcomes::read_all(&crate::divide::repo_state_dir(host))
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
            .collect();
        files.sort();
        for path in files {
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if let Some((text, workflow)) = phases_in(&path, name) {
                offer(name, text, workflow);
            }
        }
    }
    for name in builtin::names() {
        if let (Ok(text), Ok(workflow)) = (builtin::example(name), builtin::load(name)) {
            offer(name, text, workflow);
        }
    }
    rank_examples(&mut examples);
    examples
}

/// A file's phases, with the YAML the main agent reads them as: a file of phases as it is
/// written, and the phases of a workflow's own file printed.
fn phases_in(path: &Path, name: &str) -> Option<(String, Workflow)> {
    let text = std::fs::read_to_string(path).ok()?;
    match path.extension()?.to_str()? {
        "yaml" => yaml::parse(&text).ok().map(|workflow| (text, workflow)),
        "toml" => {
            let workflow = workflow_files::phases(&text, name).ok()??;
            Some((yaml::print(&workflow).ok()?, workflow))
        }
        _ => None,
    }
}

/// The names of every workflow the main agent may pick, as `lookup` finds them.
fn example_names(host: &Host) -> Vec<String> {
    examples(host)
        .into_iter()
        .map(|example| example.name)
        .collect()
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
    (StepKind::Read, "each agent reads the request back"),
    (
        StepKind::Compare,
        "compare readings and tag the differences",
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
        "review a diff or a result against criteria, by another provider",
    ),
    (StepKind::Judge, "score candidates, never the judge's own"),
    (StepKind::Critique, "try to disprove a finding or a claim"),
    (StepKind::Synthesize, "combine results into one"),
    (
        StepKind::Build,
        "tasks: a worker in a worktree, then its checks and review",
    ),
    (StepKind::Checks, "run the configured check commands"),
    (
        StepKind::LoopUntil,
        "repeat steps until a condition holds, with a hard maximum",
    ),
    (
        StepKind::Approval,
        "the Understand or plan approval, where you or your policy decide",
    ),
    (StepKind::Merge, "merge and the merge approval"),
    (StepKind::Workflow, "run another workflow as one step"),
];

/// The workflow the main agent's brief names: its own file, or the example it started from with its
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

/// Records the run's name and description from its workflow, once the main agent's brief named it,
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

    /// `orchestrator` at the end of Plan takes a change from phase 3 on. Folding Understand and
    /// Plan together would shift Build onto a finished index, so it's refused.
    #[test]
    fn a_change_keeps_the_phases_that_started() {
        let current = builtin("orchestrator");
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
