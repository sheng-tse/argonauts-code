//! The goal checkpoint. Questions wait for you in every mode when they concern scope, security,
//! data, or cost. With none left, the approval card: approve, approve with edits (rewrite and
//! re-hash), send back (restate and differ once more with your note) while the goal's loop has
//! a round left, abandon, show.

use argonauts_core::PhaseStatus;
use argonauts_core::RunId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointKind, CheckpointPacket, FreeText, GoalPacket,
};
use argonauts_core::events::goal::{GoalEvent, GoalFile, GoalQuestion, GoalSource};
use argonauts_core::events::run::{Party, RunMode, RunOutcome};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::WorkerRef;
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::settings::Settings;
use argonauts_core::steps::goal::DifferOutput;
use argonauts_core::workflow::StepRef;

use crate::checkpoints::{self, CheckpointRequest, options};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::state::goal::SettledGoal;
use crate::workflow::{Flow, StepCall, StepDone, kinds};

use super::materiality::{self, one_line};
use super::{draft, file, round};

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let view = host.read(|state| View::of(state, host.settings(), host.run_id(), &call.at.step));
    if view.approved {
        return Ok(StepDone::done("The goal is already approved"));
    }
    if view.sent_back {
        return Ok(StepDone::done("Sent back for another round"));
    }
    let drafted = view
        .drafted
        .clone()
        .ok_or_else(|| EngineError::Refused("there is no goal draft to check yet".to_owned()))?;

    let questions = &view.settled.questions;
    if !questions.is_empty() {
        let mut answers = Vec::new();
        let mut by = Party::User;
        for (question, decided) in questions.iter().zip(&view.answered) {
            answers.push(answer_text(question, decided)?);
            by = decided.by.clone();
        }
        let asked = answers.len();
        for (index, question) in questions.iter().enumerate().skip(asked) {
            let request = question_request(call, &drafted, &view, index);
            let decided = checkpoints::open_and_wait(host, request).await?;
            answers.push(answer_text(question, &decided)?);
            by = decided.by;
        }
        // Answering the last question approves the goal, with every answer written in.
        let goal = round::write_draft(host, draft::with_answers(&drafted, &answers)).await?;
        approve(host, goal, by)?;
        return Ok(StepDone::done(match answers.len() {
            1 => "You answered 1 question and approved the goal".to_owned(),
            n => format!("You answered {n} questions and approved the goal"),
        }));
    }

    let decided = match view.decided.clone() {
        Some(decided) => decided,
        None => checkpoints::open_and_wait(host, approval_request(call, &drafted, &view)).await?,
    };
    match decided.action {
        AnswerAction::Approve => {
            approve(host, drafted, decided.by)?;
            Ok(StepDone::done("The goal is approved"))
        }
        AnswerAction::ApproveWithEdits => {
            let note = decided.text.unwrap_or_default();
            let goal = round::write_draft(host, file::with_edits(&drafted, &note)).await?;
            approve(host, goal, decided.by)?;
            Ok(StepDone::done("The goal is approved with your edits"))
        }
        AnswerAction::SendBack => Ok(StepDone::done("Sent back for another round")),
        AnswerAction::Abandon => Ok(StepDone {
            end: StepEnd::Done,
            summary: Some("You abandoned the goal".to_owned()),
            flow: Flow::Finish(RunOutcome::Abandoned),
        }),
        other => Err(EngineError::Refused(format!(
            "the goal checkpoint doesn't offer {other:?}"
        ))),
    }
}

/// Records `Goal::Approved` with the file's SHA-256. The goal is frozen from here: approving
/// again returns the approved hash, and only a goal change with a higher version replaces it.
pub fn approve(host: &Host, file: GoalFile, by: Party) -> EngineResult<String> {
    if file.sha256 != file::sha256(&file) {
        return Err(EngineError::Refused(
            "the goal file's hash doesn't match what it says".to_owned(),
        ));
    }
    let approved = host.read(|state| state.goal.approved.clone());
    if let Some(approved) = &approved
        && approved.version >= file.version
    {
        return Ok(approved.sha256.clone());
    }
    let summary = match file.source {
        GoalSource::Prompt => "Your prompt is the goal".to_owned(),
        GoalSource::Restatements => match file.criteria.len() {
            1 => "Goal approved with 1 criterion".to_owned(),
            n => format!("Goal approved with {n} criteria"),
        },
    };
    let sha256 = file.sha256.clone();
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Approved { file, by }),
    )?;
    if approved.is_none() {
        round::phase(host, PhaseStatus::Finished, Some(summary))?;
    }
    Ok(sha256)
}

/// One answer as the goal file records it.
pub fn answer_text(question: &GoalQuestion, decided: &CheckpointDecided) -> EngineResult<String> {
    let answer = match &decided.action {
        AnswerAction::Choose { value } => question
            .options
            .iter()
            .find(|option| option.label == *value || option.key.to_string() == *value)
            .map_or_else(|| one_line(value), |option| option.label.clone()),
        AnswerAction::Answer => decided
            .text
            .as_deref()
            .map(one_line)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| EngineError::Refused("your own answer is empty".to_owned()))?,
        other => {
            return Err(EngineError::Refused(format!(
                "a goal question takes an answer, not {other:?}"
            )));
        }
    };
    let asked = one_line(&question.text);
    Ok(format!(
        "{}: {answer}",
        asked.trim_end_matches(['?', '.', ':'])
    ))
}

/// What the checkpoint needs from the state, read once.
#[derive(Debug, Clone)]
pub struct View {
    pub run: RunId,
    pub approved: bool,
    pub sent_back: bool,
    pub drafted: Option<GoalFile>,
    pub settled: SettledGoal,
    /// This round's decided question checkpoints, oldest first.
    pub answered: Vec<CheckpointDecided>,
    /// This round's decided approval card, if it was decided.
    pub decided: Option<CheckpointDecided>,
    /// Reasons the approval card must wait for you even with no question open: differences
    /// in `always_ask` over the question limit, and restatements no differ compared.
    pub waits: u32,
    /// Sending the draft back runs another round, since the goal's loop has one left.
    pub send_back: bool,
    /// The restaters and differs, who must not approve their own goal.
    pub authors: Vec<WorkerRef>,
    /// Each difference the differs tagged, one line each.
    pub tags: Vec<String>,
}

impl View {
    pub fn of(state: &RunState, settings: &Settings, run: &RunId, at: &StepRef) -> Self {
        let goal = &state.goal;
        let mut this_round: Vec<_> = state
            .checkpoints
            .by_id
            .values()
            .filter(|record| {
                record.opened.kind == CheckpointKind::Goal
                    && record.withdrawn.is_none()
                    && goal.settled_at.is_none_or(|at| record.since > at)
            })
            .collect();
        this_round.sort_by_key(|record| record.since);
        let decisions = this_round
            .iter()
            .filter_map(|record| record.decided.clone());
        let (answered, approvals): (Vec<_>, Vec<_>) = decisions.partition(|decided| {
            matches!(
                decided.action,
                AnswerAction::Choose { .. } | AnswerAction::Answer
            )
        });

        let listings: Vec<DifferOutput> = goal
            .compared
            .iter()
            .map(|(_, differences)| DifferOutput {
                agreements: Vec::new(),
                differences: differences.clone(),
            })
            .collect();
        let held_back =
            materiality::settle(&listings, goal.restated.len(), &settings.goal).held_back;
        let unchecked = round::mode(state) == RunMode::Crew
            && goal.restated.len() >= 2
            && goal.compared.is_empty();

        let mut authors: Vec<WorkerRef> = Vec::new();
        let workers = goal
            .restated
            .iter()
            .map(|(_, worker, _)| worker)
            .chain(goal.compared.iter().map(|(worker, _)| worker));
        for worker in workers {
            if !authors.contains(worker) {
                authors.push(worker.clone());
            }
        }
        let mut tags: Vec<String> = Vec::new();
        for (_, differences) in &goal.compared {
            for difference in differences {
                let line = format!(
                    "{}: {}",
                    materiality::tag_name(difference.tag),
                    one_line(&difference.topic)
                );
                if !tags.contains(&line) {
                    tags.push(line);
                }
            }
        }

        Self {
            run: run.clone(),
            approved: goal.approved.is_some(),
            sent_back: round::sent_back(state),
            drafted: goal.drafted.clone(),
            settled: goal.settled.clone().unwrap_or_default(),
            answered,
            decided: approvals.into_iter().last(),
            waits: held_back + u32::from(unchecked),
            send_back: kinds::another_round(state, at),
            authors,
            tags,
        }
    }
}

fn packet(drafted: &GoalFile, view: &View, questions: Vec<GoalQuestion>) -> CheckpointPacket {
    CheckpointPacket::Goal(GoalPacket {
        goal: drafted.clone(),
        agreements: view.settled.agreements.clone(),
        assumptions: drafted.assumptions.clone(),
        questions,
    })
}

/// Inline, since `path` names a document stored beside the packet, where the approver reads
/// it, and the goal file lives in the repo.
fn document(drafted: &GoalFile) -> EvidenceDocument {
    EvidenceDocument {
        label: "goal file".to_owned(),
        text: file::render(drafted),
        path: None,
        sha256: Some(drafted.sha256.clone()),
    }
}

/// One card per question, the remaining ones in its packet. A question always waits for you:
/// the recommended answer is a default you accept with one key, never your rules' or your
/// approver's pick.
pub fn question_request(
    call: &StepCall,
    drafted: &GoalFile,
    view: &View,
    index: usize,
) -> CheckpointRequest {
    let remaining = view.settled.questions[index..].to_vec();
    let question = &remaining[0];
    CheckpointRequest {
        at: Some(call.at.clone()),
        kind: CheckpointKind::Goal,
        title: question_title(question.n, question.of),
        card: None,
        options: Some(options::goal_question(question)),
        facts: EvidenceFacts {
            open_questions: Some(u32::try_from(remaining.len()).unwrap_or(u32::MAX)),
            ..EvidenceFacts::default()
        },
        packet: packet(drafted, view, remaining),
        agent_outputs: view.tags.clone(),
        documents: vec![document(drafted)],
        free_text: FreeText::Answer,
        authors: view.authors.clone(),
    }
}

/// The card asks, as the merge card does, and counts only when there's more than one.
fn question_title(n: u8, of: u8) -> String {
    const ASKS: &str = "What should the goal say?";
    if of > 1 {
        format!("{ASKS} · {n} of {of}")
    } else {
        ASKS.to_owned()
    }
}

/// The approval card, with the kind's standard options, less "Send back" when no round is left
/// to send the draft back to. Auto may approve it only when nothing waits for you.
pub fn approval_request(call: &StepCall, drafted: &GoalFile, view: &View) -> CheckpointRequest {
    CheckpointRequest {
        at: Some(call.at.clone()),
        kind: CheckpointKind::Goal,
        title: "Approve this goal?".to_owned(),
        card: None,
        packet: packet(drafted, view, Vec::new()),
        facts: EvidenceFacts {
            open_questions: Some(view.waits),
            ..EvidenceFacts::default()
        },
        agent_outputs: view.tags.clone(),
        documents: vec![document(drafted)],
        options: (!view.send_back).then(|| {
            options::standard(CheckpointKind::Goal)
                .into_iter()
                .filter(|option| option.action != AnswerAction::SendBack)
                .collect()
        }),
        free_text: FreeText::Edits,
        authors: view.authors.clone(),
    }
}
