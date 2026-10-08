//! The plan approval of a run the runner runs. Where a plan waits for you, the steps are shown
//! before any agent starts: the workflow's name, what it is for, and its phases in order. You
//! approve them, send them back with a note when the orchestrator wrote them, run the request
//! solo, or abandon the run. Where your settings decide plans nothing opens: a script has no
//! tasks yet for a rule to weigh, and the merge approval follows either way.

use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointKind, FreeText, PlanWorkflow,
};
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::WorkerRef;
use argonauts_core::policy::EvidenceDocument;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::workflow::{Phase, StandardPhase, Workflow, WorkflowParams, WorkflowRef};
use argonauts_runner::Script;

use crate::checkpoints::{self, autopilot, options};
use crate::divide::workflow_plan::{Shown, request_showing};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::workflow::{Flow, StepCall, StepDone, runner};

/// Where a script's steps came from, which decides what the approval says and offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Wrote {
    /// The orchestrator, for this request, keeping the roles of the workflow in use.
    Orchestrator(WorkerRef),
    /// A workflow file or a script, which runs exactly as written.
    File,
}

pub(super) enum Verdict {
    Approved,
    /// The orchestrator writes the steps again with your notes.
    SentBack,
    /// The run goes another way: solo, or it ends here.
    Ended(StepDone),
}

/// Every note you sent the steps back with at this step, oldest first. They come from the
/// run's own record, so a run that picks up again reads the same ones.
pub(super) fn sent_back(host: &Host, call: &StepCall) -> Vec<String> {
    host.read(|state| {
        let since = state
            .workflows
            .progress(&call.at.step)
            .map_or(0, |progress| progress.since);
        let mut decided: Vec<(u64, String)> = state
            .checkpoints
            .by_id
            .values()
            .filter(|record| {
                record.opened.kind == CheckpointKind::Plan
                    && record.withdrawn.is_none()
                    && record.since > since
            })
            .filter_map(|record| {
                let decided = record.decided.as_ref()?;
                (decided.action == AnswerAction::SendBack).then(|| {
                    let note = decided.text.clone().unwrap_or_default();
                    (record.since, note.trim().to_owned())
                })
            })
            .collect();
        decided.sort_by_key(|(since, _)| *since);
        decided.into_iter().map(|(_, note)| note).collect()
    })
}

/// Whether a plan waits for you now: in supervised mode, and in auto preview, which asks you
/// and records what your settings would have said.
fn asks_you(host: &Host) -> bool {
    !autopilot::applies_now(host, CheckpointKind::Plan)
        && matches!(
            checkpoints::mode_now(host, CheckpointKind::Plan),
            ApprovalMode::Supervised | ApprovalMode::AutoPreview
        )
}

/// Shows the steps and waits for your answer, where a plan waits for you. `notes` are what you
/// already sent these steps back with: they are part of what you approve, so steps written
/// again never pick up the answer to the ones before them.
pub(super) async fn approval(
    host: &Host,
    call: &StepCall,
    script: &Script,
    wrote: &Wrote,
    notes: &[String],
) -> EngineResult<Verdict> {
    if !asks_you(host) {
        return Ok(Verdict::Approved);
    }
    let in_use = host.settings().workflow.clone();
    let (summary, authors) = match wrote {
        Wrote::Orchestrator(worker) => (
            format!(
                "The orchestrator wrote these steps for your request, with the roles of {in_use}"
            ),
            vec![worker.clone()],
        ),
        Wrote::File => (format!("The steps of {in_use} run as written"), Vec::new()),
    };
    let mut documents = vec![EvidenceDocument {
        label: "workflow".to_owned(),
        text: script.source.clone(),
        path: None,
        sha256: None,
    }];
    if !notes.is_empty() {
        documents.push(EvidenceDocument {
            label: "sent back with".to_owned(),
            text: notes.join("\n"),
            path: None,
            sha256: None,
        });
    }
    let can_rewrite = matches!(wrote, Wrote::Orchestrator(_));
    let shown = Shown {
        workflow: Some(PlanWorkflow {
            from: Some(WorkflowRef::new(&in_use)),
            why: None,
            workflow: as_phases(script),
        }),
        summary,
        documents,
        options: answer_options(can_rewrite),
        free_text: if can_rewrite {
            FreeText::Guidance
        } else {
            FreeText::None
        },
        authors,
    };
    let request = request_showing(host, call, shown).await;
    let decided = checkpoints::open_and_wait(host, request).await?;
    match decided.action {
        AnswerAction::Approve => Ok(Verdict::Approved),
        AnswerAction::SendBack if can_rewrite => Ok(Verdict::SentBack),
        AnswerAction::SwitchToSolo => Ok(Verdict::Ended(StepDone {
            end: StepEnd::Done,
            summary: Some("You chose to run it solo".to_owned()),
            flow: Flow::Switch(WorkflowRef::new(runner::solo_workflow(host.settings()))),
        })),
        AnswerAction::Abandon => Ok(Verdict::Ended(StepDone {
            end: StepEnd::Done,
            summary: Some("You abandoned the run".to_owned()),
            flow: Flow::Finish(RunOutcome::Abandoned),
        })),
        other => Err(EngineError::Refused(format!(
            "the plan approval of a workflow's steps doesn't offer {other:?}"
        ))),
    }
}

/// The plan approval's options for steps: there are no tasks to edit, and only steps the
/// orchestrator wrote can be written again.
fn answer_options(can_rewrite: bool) -> Vec<AnswerOption> {
    options::standard(CheckpointKind::Plan)
        .into_iter()
        .filter(|option| match option.action {
            AnswerAction::ApproveWithEdits => false,
            AnswerAction::SendBack => can_rewrite,
            _ => true,
        })
        .collect()
}

/// The script's name, what it is for and its phases, in the shape the plan approval draws.
fn as_phases(script: &Script) -> Workflow {
    Workflow {
        name: script.meta.name.clone(),
        description: script.meta.description.clone(),
        based_on: None,
        params: WorkflowParams::default(),
        phases: script
            .meta
            .phases
            .iter()
            .map(|phase| Phase {
                name: phase.title.clone(),
                detail: phase.detail.clone(),
                standard: StandardPhase::Do,
                steps: Vec::new(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(options: &[AnswerOption]) -> String {
        options.iter().map(|option| option.key).collect()
    }

    #[test]
    fn only_steps_the_orchestrator_wrote_can_be_sent_back() {
        assert_eq!(keys(&answer_options(true)), "ybsxd");
        assert_eq!(keys(&answer_options(false)), "ysxd");
    }

    #[test]
    fn a_scripts_phases_are_what_the_approval_shows() {
        let script = Script::parse(
            "export const meta = { name: 'add-due', description: 'Due dates', \
             phases: [{ title: 'Plan', detail: 'split it' }, { title: 'Build' }] }\nreturn 1",
        )
        .expect("reads");
        let shown = as_phases(&script);
        assert_eq!(
            (shown.name.as_str(), shown.description.as_str()),
            ("add-due", "Due dates")
        );
        let phases: Vec<(&str, &str)> = shown
            .phases
            .iter()
            .map(|phase| (phase.name.as_str(), phase.detail.as_str()))
            .collect();
        assert_eq!(phases, [("Plan", "split it"), ("Build", "")]);
    }
}
