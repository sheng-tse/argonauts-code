//! What Understand's steps share about the current round: the request, the mode, your
//! send-back note, the phase itself, and writing a draft.

use argonauts_core::base::log::{Note, NoteLevel};
use argonauts_core::base::names::ModelRef;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::runs::events::goal::{Assumption, GoalEvent, GoalFile};
use argonauts_core::runs::events::run::{PhaseChange, RunMode, RunPhase};
use argonauts_core::runs::events::session::{InputPart, InputPartKind};
use argonauts_core::workflows::steps::understand::Reading;
use argonauts_core::{PhaseStatus, RunId};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::merge::branch;
use crate::state::RunState;

use super::draft;
use super::file;
use super::materiality::Settled;

pub fn mode(state: &RunState) -> RunMode {
    state
        .run
        .mode
        .or_else(|| state.run.started.as_ref().map(|started| started.mode))
        .unwrap_or(RunMode::Crew)
}

/// Exactly what you typed.
pub fn request(state: &RunState) -> EngineResult<String> {
    state
        .run
        .started
        .as_ref()
        .map(|started| started.request.clone())
        .ok_or_else(|| EngineError::Refused("the run has no request yet".to_owned()))
}

/// Your note from the newest Understand approval, when you sent the draft back and the goal isn't
/// approved yet. The next round's readers and comparers get it.
pub fn guidance(state: &RunState) -> Option<String> {
    if state.goal.approved.is_some() {
        return None;
    }
    let record = state
        .approvals
        .last_decided(ApprovalKind::Understand, None)?;
    let decided = record.decided.as_ref()?;
    (decided.action == AnswerAction::SendBack).then(|| decided.text.clone().unwrap_or_default())
}

/// The round the state holds was sent back and no new round has recorded anything yet, so
/// its readings and draft are stale.
pub fn sent_back(state: &RunState) -> bool {
    let Some(settled_at) = state.goal.settled_at else {
        return false;
    };
    state
        .approvals
        .last_decided(ApprovalKind::Understand, None)
        .filter(|record| record.since > settled_at)
        .and_then(|record| record.decided.as_ref())
        .is_some_and(|decided| decided.action == AnswerAction::SendBack)
}

/// This round's readings, none when the round was sent back.
pub fn readings(state: &RunState) -> Vec<(String, ModelRef, Reading)> {
    if sent_back(state) {
        Vec::new()
    } else {
        state.goal.readings.clone()
    }
}

/// Your note as a prompt part.
pub fn guidance_part(note: Option<&str>) -> Vec<InputPart> {
    let Some(note) = note.map(str::trim) else {
        return Vec::new();
    };
    let text = if note.is_empty() {
        "You sent the last goal draft back for another round without a note.".to_owned()
    } else {
        format!("You sent the last goal draft back for another round with this note:\n\n{note}")
    };
    vec![InputPart {
        kind: InputPartKind::Message,
        label: "your note".to_owned(),
        approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
        text,
        source: Some("Understand approval".to_owned()),
    }]
}

/// Records Understand's start once, before anything of the goal happened.
pub fn begin(host: &Host) -> EngineResult<()> {
    let untouched = host.read(|state| {
        state.run.phase != Some(RunPhase::Understand)
            && state.goal.round == 0
            && state.goal.readings.is_empty()
            && state.goal.drafted.is_none()
            && state.goal.approved.is_none()
    });
    if untouched {
        phase(host, PhaseStatus::Started, None)?;
    }
    Ok(())
}

pub fn phase(host: &Host, status: PhaseStatus, summary: Option<String>) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Phase(PhaseChange {
            phase: RunPhase::Understand,
            status,
            summary,
        }),
    )?;
    Ok(())
}

pub fn note(host: &Host, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(None, None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// A note that says what happened in plain words, with the raw `detail` behind it for Ctrl+O.
pub fn note_with_detail(
    host: &Host,
    level: NoteLevel,
    line: &str,
    detail: &str,
) -> EngineResult<()> {
    let note = Note::with_detail(level, line, detail);
    host.record(None, None, RunEventKind::Note(note))?;
    Ok(())
}

/// Writes the draft and records `Goal::Drafted`, unless this exact draft is already recorded.
/// The run's branch is made here too, so the work that follows starts from it.
pub async fn write_draft(host: &Host, goal: GoalFile) -> EngineResult<GoalFile> {
    branch::ensure_run_branch(host).await?;
    let goal = file::write(host, goal).await?;
    let recorded = host.read(|state| {
        state
            .goal
            .drafted
            .as_ref()
            .is_some_and(|drafted| drafted.sha256 == goal.sha256)
    });
    if !recorded {
        host.record(
            None,
            None,
            RunEventKind::Goal(GoalEvent::Drafted(goal.clone())),
        )?;
    }
    Ok(goal)
}

/// Records what the rules settled and drafts the goal from the readings, or from your
/// prompt when fewer than `enough` came back.
pub async fn settle_and_draft(
    host: &Host,
    run: &RunId,
    request: &str,
    readings: &[(String, ModelRef, Reading)],
    settled: &Settled,
    enough: usize,
) -> EngineResult<GoalFile> {
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Settled {
            agreements: settled.agreements.clone(),
            assumptions: settled.assumptions.clone(),
            questions: settled.questions.clone(),
            dropped: settled.dropped,
        }),
    )?;
    draft_goal(host, run, request, readings, &settled.assumptions, enough).await
}

/// Drafts the goal from the readings and the settled assumptions, or from your prompt when
/// fewer than `enough` readings came back.
pub async fn draft_goal(
    host: &Host,
    run: &RunId,
    request: &str,
    readings: &[(String, ModelRef, Reading)],
    assumptions: &[Assumption],
    enough: usize,
) -> EngineResult<GoalFile> {
    let goal = if readings.len() >= enough {
        draft::from_readings(run, readings, assumptions)
    } else {
        None
    };
    let goal = match goal {
        Some(goal) => goal,
        None => {
            let came_back = match readings.len() {
                0 => "No reading came back".to_owned(),
                1 => "Only 1 reading came back".to_owned(),
                n => format!("Only {n} readings came back"),
            };
            note(
                host,
                NoteLevel::Warn,
                format!("{came_back}, so your prompt is the goal draft."),
            )?;
            draft::from_prompt(run, request)
        }
    };
    write_draft(host, goal).await
}
