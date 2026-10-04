//! What you do to a run: answers, guidance, mentions, pauses, stops, reassigns, take-overs, the
//! approval mode, and the run commands and views. Anything that decides or raises autonomy goes
//! through `Approvals`, and without it the app says why it can't.

mod cards;
mod control;
mod views;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use anyhow::Context;
use argonauts_core::api::{Approvals, CheckpointAnswer, DiffScope};
use argonauts_core::app::{AppEvent, NOT_CHANGED, ViewData};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::goal::{GoalAction, LET_THE_LEAD_ANSWER};
use argonauts_core::names::{CheckpointId, ChoiceId};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::{CardId, NoteLevel, RunId};

use super::attach::Stage;
use super::runs::{RunChoice, RunsState};
use super::{Ctx, Effect};

pub(crate) use cards::{checkpoints, reassign, remember_pick, revert, slash_reassign};
pub(crate) use control::{
    cancel_pause, continue_run, guidance, mention, pause, resume, steer, stop, stop_agent,
    take_back,
};
pub(crate) use views::{
    all_repos, ask_narrator, context, save_workflow, status, summary, view, workflow_view,
};

/// Why nothing that decides or raises autonomy can leave this app.
pub(super) const NO_APPROVALS: &str = "This app can't approve anything or change autonomy: it \
     has no terminal, or it runs inside an agent session. Nothing was sent.";
const NO_RUN: &str = "No run is on screen. /resume picks one.";

/// `UiCommand::Answer` for a run's checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CheckpointReply {
    pub run: RunId,
    pub id: CheckpointId,
    pub action: AnswerAction,
    pub text: Option<String>,
    /// From the card the TUI drew, passed on unchanged.
    pub evidence_sha256: Option<String>,
    pub cards: Vec<CardId>,
    /// Save a handoff pick as a replacement rule; the scope question follows.
    pub remember: bool,
}

type Handled = anyhow::Result<Vec<Effect>>;

/// `Approvals`, or a notice saying why there are none.
fn approvals(ctx: &Ctx) -> Option<Arc<dyn Approvals>> {
    let approvals = ctx.services.approvals.clone();
    if approvals.is_none() {
        ctx.notice(NoteLevel::Warn, NO_APPROVALS);
    }
    approvals
}

/// The run on screen, or a notice that there is none.
fn on_screen(ctx: &Ctx, runs: &RunsState) -> Option<RunId> {
    let run = super::runs::attached(runs).cloned();
    if run.is_none() {
        ctx.notice(NoteLevel::Info, NO_RUN);
    }
    run
}

pub(crate) async fn answer(ctx: &mut Ctx, runs: &mut RunsState, reply: CheckpointReply) -> Handled {
    // The card shows its own diff; the engine never sees this action.
    if reply.action == AnswerAction::ShowDiff {
        return Ok(Vec::new());
    }
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    // The engine binds an answer to the evidence you saw, so one without it can't be sent.
    let Some(evidence_sha256) = reply.evidence_sha256 else {
        ctx.notice(
            NoteLevel::Warn,
            format!(
                "The answer to {} carries no evidence hash, so it wasn't sent.",
                reply.id
            ),
        );
        return Ok(Vec::new());
    };
    let pick = match &reply.action {
        AnswerAction::Candidate { worker } => Some(worker.clone()),
        _ => None,
    };
    let lead_answers = matches!(
        &reply.action,
        AnswerAction::Choose { value } if value == LET_THE_LEAD_ANSWER
    );
    let answer = CheckpointAnswer {
        id: reply.id.clone(),
        action: reply.action,
        text: reply.text,
        evidence_sha256,
        cards: reply.cards,
    };
    approvals
        .answer(&reply.run, answer)
        .await
        .with_context(|| format!("answering {} of {}", reply.id, reply.run))?;
    if lead_answers {
        return super::settings::let_the_lead_answer(ctx);
    }
    if reply.remember {
        return cards::remember(ctx, runs, &reply.run, &reply.id, pick);
    }
    Ok(Vec::new())
}

/// Choices this part opened, such as a reassign that overrides a rule, or `/rewind`'s confirm.
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    id: ChoiceId,
    action: AnswerAction,
    text: Option<String>,
) -> Handled {
    // Neither card takes a note.
    let _ = text;
    let Some(choice) = runs.choices.remove(&id) else {
        return Ok(Vec::new());
    };
    if let (RunChoice::Revert { run, card, .. }, AnswerAction::ShowDiff) = (&choice, &action) {
        let diff = ctx
            .services
            .conductor
            .diff(run, card, DiffScope::Card)
            .await
            .with_context(|| format!("the diff of {card} in {run}"));
        // The card stays open, so you can still revert after reading the diff.
        runs.choices.insert(id, choice);
        ctx.send(AppEvent::View {
            data: Box::new(ViewData::Diff { diff: diff? }),
        });
        return Ok(Vec::new());
    }
    let outcome = match (choice, action) {
        (
            RunChoice::OverrideRule {
                run,
                card,
                to,
                rule,
            },
            AnswerAction::Approve,
        ) => {
            let Some(approvals) = approvals(ctx) else {
                close(ctx, id, NOT_CHANGED);
                return Ok(Vec::new());
            };
            let overridden = approvals
                .reassign_overriding(&run, &card, to.clone(), rule.clone())
                .await
                .with_context(|| format!("reassigning {card} of {run}"));
            if let Err(err) = overridden {
                close(ctx, id, NOT_CHANGED);
                return Err(err);
            }
            let name = ctx.names.worker(&to);
            format!("Reassigned {card} to {name}, overriding {rule} for this card.")
        }
        (
            RunChoice::Revert {
                run,
                card,
                checkpoint,
            },
            AnswerAction::Approve,
        ) => {
            let reverted = ctx
                .services
                .conductor
                .revert(&run, &card, checkpoint)
                .await
                .with_context(|| format!("reverting {card} of {run}"));
            if let Err(err) = reverted {
                close(ctx, id, "Not reverted.");
                return Err(err);
            }
            format!(
                "Reverted {card} to handoff-{checkpoint}. The work after it is on \
                 argonauts/{run}/{card}-replaced-{checkpoint}."
            )
        }
        (RunChoice::Revert { .. }, _) => "Not reverted.".to_owned(),
        (RunChoice::OverrideRule { .. }, _) => NOT_CHANGED.to_owned(),
    };
    close(ctx, id, outcome);
    Ok(Vec::new())
}

pub(crate) fn set_aside(ctx: &mut Ctx, runs: &mut RunsState, id: ChoiceId) -> Handled {
    let outcome = match runs.choices.remove(&id) {
        Some(RunChoice::OverrideRule { .. }) => NOT_CHANGED,
        Some(RunChoice::Revert { .. }) => "Not reverted.",
        None => return Ok(Vec::new()),
    };
    close(ctx, id, outcome);
    Ok(Vec::new())
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}

pub(crate) async fn take_over(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    card: CardId,
) -> Handled {
    let _ = runs;
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    // The engine answers once it has paused the card, which can take as long as the agent's
    // current tool call, so the screen goes on meanwhile.
    let place = ctx.settings().ui.takeover;
    let out = ctx.out.clone();
    tokio::spawn(async move {
        match approvals.take_over(&run, &card).await {
            Ok(ticket) => out.send(AppEvent::TakeOver { ticket, place, run }),
            Err(err) => out.send(AppEvent::Notice {
                level: NoteLevel::Error,
                text: format!("taking over {card} of {run}: {err}"),
            }),
        }
    });
    Ok(Vec::new())
}

pub(crate) async fn take_over_ended(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    card: CardId,
    exit_code: Option<i32>,
) -> Handled {
    let _ = runs;
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    approvals
        .take_over_ended(&run, &card)
        .await
        .with_context(|| format!("handing {card} of {run} back"))?;
    let how = match exit_code {
        Some(0) | None => String::new(),
        Some(code) => format!("The vendor's UI exited with code {code}. "),
    };
    ctx.notice(
        NoteLevel::Info,
        format!(
            "{how}Your changes to {card} are committed, and its return card decides who goes on."
        ),
    );
    Ok(Vec::new())
}

/// Shift+Tab: for the run on screen, and for runs started later in this app session.
pub(crate) async fn approval_mode(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    mode: ApprovalMode,
) -> Handled {
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    runs.approval_mode = Some(mode);
    if let Some(attached) = runs
        .attached
        .as_mut()
        .filter(|attached| !matches!(attached.stage, Stage::Finished(_)))
    {
        approvals
            .set_approval_mode(&attached.run, mode)
            .await
            .with_context(|| format!("setting the approval mode of {}", attached.run))?;
        attached.approval_mode = Some(mode);
    }
    Ok(vec![Effect::Status])
}

/// Your verdict on an auto-approval. A right one can raise autonomy, so it needs you at the
/// terminal, as an approval does.
/// The goal view's keys: clear the goal, turn the run into one, or change an assumption. Only
/// you do these, so an app that can't approve can't either.
pub(crate) async fn goal(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: Option<RunId>,
    action: GoalAction,
) -> Handled {
    if approvals(ctx).is_none() {
        return Ok(Vec::new());
    }
    let Some(run) = run.or_else(|| on_screen(ctx, runs)) else {
        return Ok(Vec::new());
    };
    let done = match &action {
        GoalAction::Clear { land: true } => {
            "Stopped pursuing the goal. Merge lands what holds once every check passes."
        }
        GoalAction::Clear { land: false } => {
            "Stopped pursuing the goal. The run stops and keeps its branch."
        }
        GoalAction::TurnIntoGoal => {
            "The run becomes a goal: its check judges this workflow when it ends, and the crew \
             keeps attempting what's unmet."
        }
        GoalAction::ChangeAssumption { .. } => {
            "Changed the assumption. The agents working now are told, and the next attempt \
             plans with it."
        }
    };
    ctx.services
        .conductor
        .goal(&run, action)
        .await
        .with_context(|| format!("changing {run}'s goal"))?;
    ctx.notice(NoteLevel::Info, done);
    Ok(Vec::new())
}

pub(crate) async fn spot_check(
    ctx: &mut Ctx,
    run: RunId,
    id: CheckpointId,
    correct: bool,
) -> Handled {
    if approvals(ctx).is_none() {
        return Ok(Vec::new());
    }
    ctx.services
        .conductor
        .spot_check(&run, &id, correct)
        .await
        .with_context(|| format!("marking {id} of {run}"))?;
    let verdict = if correct {
        "right, so it counts toward autonomy"
    } else {
        "wrong, so that decision point goes back to shadow"
    };
    ctx.notice(NoteLevel::Info, format!("Marked {id} of {run} {verdict}."));
    Ok(Vec::new())
}
