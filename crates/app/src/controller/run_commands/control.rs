//! Steering a run: guidance, mentions, steers, pausing, resuming, stopping, taking queued
//! messages back, and `/follow-up`.

use anyhow::Context;
use argonauts_core::api::{ApiError, RunState, StartRun};
use argonauts_core::app::{AppEvent, AppMode};
use argonauts_core::card::title_from_task;
use argonauts_core::events::run::RunOutcome;
use argonauts_core::input::MentionTarget;
use argonauts_core::names::{MessageId, SessionKey};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::worker::SteerSupport;
use argonauts_core::{NoteLevel, RunId};

use super::{Handled, on_screen};
use crate::controller::attach::Stage;
use crate::controller::runs::{self, Queued, RunsState};
use crate::controller::shell::{self, ShellRan};
use crate::controller::{Ctx, Effect};

/// `ran` are the `!` commands you ran since your last message, which go along with it.
pub(crate) async fn guidance(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    text: String,
    ran: &[ShellRan],
) -> Handled {
    let message = with_commands(&text, ran);
    let sent = ctx.services.conductor.guidance(&run, message).await;
    queued(ctx, runs, run, text, sent)
}

pub(crate) async fn mention(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    target: MentionTarget,
    text: String,
    ran: &[ShellRan],
) -> Handled {
    let message = with_commands(&text, ran);
    let sent = ctx.services.conductor.mention(&run, target, message).await;
    queued(ctx, runs, run, text, sent)
}

/// Your message, then the commands you ran before it. The queue shows your words alone.
fn with_commands(text: &str, ran: &[ShellRan]) -> String {
    match shell::context_text(ran) {
        Some(commands) => format!("{text}\n\n{commands}"),
        None => text.to_owned(),
    }
}

/// Keeps the message's id so it can be taken back until it's delivered.
fn queued(
    ctx: &Ctx,
    runs: &mut RunsState,
    run: RunId,
    text: String,
    sent: Result<MessageId, ApiError>,
) -> Handled {
    match sent {
        Ok(id) => {
            runs.queued
                .entry(run)
                .or_default()
                .push(Queued { id, text });
            Ok(Vec::new())
        }
        // A parked run keeps it in its pending answers for the next supervisor.
        Err(ApiError::NotNow(_)) => {
            ctx.notice(NoteLevel::Info, format!("Queued for when {run} resumes."));
            Ok(Vec::new())
        }
        Err(err) => super::refused(ctx, err, || "Couldn't send it to the run".to_owned()),
    }
}

pub(crate) async fn steer(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    session: SessionKey,
    text: String,
) -> Handled {
    let _ = runs;
    match ctx.services.conductor.steer(&run, &session, text).await {
        Ok(ack) => {
            let when = match ack.how {
                SteerSupport::Input => "reads it after its current tool call",
                SteerSupport::InterruptResume => "stops and resumes with it",
                SteerSupport::NextTurnOnly => "gets it at its next turn",
            };
            ctx.notice(
                NoteLevel::Info,
                format!("Sent to {}, which {when}.", ack.session),
            );
            Ok(Vec::new())
        }
        Err(ApiError::NotNow(why)) => {
            ctx.notice(NoteLevel::Info, format!("Nothing was sent, since {why}."));
            Ok(Vec::new())
        }
        Err(err) => super::refused(ctx, err, || format!("Couldn't steer {session}")),
    }
}

pub(crate) async fn pause(ctx: &mut Ctx, runs: &mut RunsState, now: bool) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    if let Some(why) = not_going(runs, &run) {
        ctx.notice(NoteLevel::Info, why);
        return Ok(Vec::new());
    }
    if let Err(err) = ctx.services.conductor.pause(&run, now).await {
        return super::refused(ctx, err, || "Couldn't pause the run".to_owned());
    }
    ctx.set_mode(AppMode::Pausing { run });
    Ok(Vec::new())
}

/// Why a pause or stop has nothing to act on.
fn not_going(runs: &RunsState, run: &RunId) -> Option<String> {
    let attached = runs.attached.as_ref()?;
    match attached.stage {
        _ if attached.crashed => Some(format!("{run} crashed. /resume {run} recovers it.")),
        Stage::Parked => Some(format!("{run} is parked. /resume {run} goes on.")),
        Stage::Finished(_) => Some(format!("{run} has finished.")),
        Stage::Going => None,
    }
}

pub(crate) async fn cancel_pause(ctx: &mut Ctx, runs: &mut RunsState) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    if let Err(err) = ctx.services.conductor.cancel_pause(&run).await {
        return super::refused(ctx, err, || "Couldn't cancel the pause".to_owned());
    }
    ctx.set_mode(AppMode::Working { run });
    Ok(Vec::new())
}

/// The run on screen when `run` is `None` and that run can go on, else the `/resume` picker. A
/// merged run can't, so the picker opens, where its agents' sessions carry on.
pub(crate) async fn resume(ctx: &mut Ctx, runs: &mut RunsState, run: Option<RunId>) -> Handled {
    if let Some(run) = run {
        runs::resume_and_attach(ctx, runs, &run).await?;
        return Ok(vec![Effect::Status]);
    }
    let waiting = runs.attached.as_ref().filter(|attached| {
        attached.stage != Stage::Finished(RunOutcome::Merged)
            && (attached.crashed || attached.stage == Stage::Parked || attached.ended)
    });
    match waiting.map(|attached| attached.run.clone()) {
        Some(run) => {
            runs::resume_and_attach(ctx, runs, &run).await?;
            Ok(vec![Effect::Status])
        }
        None => {
            runs::show_runs(ctx).await?;
            Ok(Vec::new())
        }
    }
}

/// `/stop [run]`: the run named, or the one on screen.
pub(crate) async fn stop(ctx: &mut Ctx, runs: &mut RunsState, run: Option<RunId>) -> Handled {
    let Some(run) = run.or_else(|| on_screen(ctx, runs)) else {
        return Ok(Vec::new());
    };
    let shown = runs
        .attached
        .as_ref()
        .filter(|attached| attached.run == run);
    if let Some(Stage::Finished(_)) = shown.map(|attached| attached.stage) {
        ctx.notice(NoteLevel::Info, format!("{run} has finished."));
        return Ok(Vec::new());
    }
    if let Err(err) = ctx.services.conductor.stop(&run).await {
        return super::refused(ctx, err, || "Couldn't stop that run".to_owned());
    }
    ctx.notice(
        NoteLevel::Info,
        format!("Stopping {run}: its agents wrap up and report, and their work is committed."),
    );
    Ok(Vec::new())
}

pub(crate) async fn stop_agent(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    session: SessionKey,
) -> Handled {
    let _ = runs;
    match ctx.services.conductor.stop_session(&run, &session).await {
        Ok(()) => Ok(Vec::new()),
        Err(err) => super::refused(ctx, err, || format!("Couldn't stop {session}")),
    }
}

/// With no ids given, takes back every message this app queued for the run.
pub(crate) async fn take_back(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    messages: Vec<MessageId>,
) -> Handled {
    let ids = if messages.is_empty() {
        runs.queued
            .get(&run)
            .map(|queued| queued.iter().map(|message| message.id.clone()).collect())
            .unwrap_or_default()
    } else {
        messages
    };
    if ids.is_empty() {
        ctx.notice(NoteLevel::Info, "Nothing is waiting to be taken back.");
        return Ok(Vec::new());
    }
    let back = match ctx.services.conductor.take_back(&run, ids.clone()).await {
        Ok(back) => back,
        Err(err) => {
            return super::refused(ctx, err, || "Couldn't take the messages back".to_owned());
        }
    };
    // What didn't come back was delivered, so none of these can be taken back again.
    if let Some(queued) = runs.queued.get_mut(&run) {
        queued.retain(|message| !ids.contains(&message.id));
    }
    let mut texts = Vec::with_capacity(back.len());
    for id in &ids {
        if let Some(message) = back.iter().find(|message| &message.id == id) {
            texts.push(message.text.clone());
        }
    }
    let delivered = ids.len().saturating_sub(texts.len());
    ctx.send(AppEvent::TakenBack { texts });
    match delivered {
        0 => {}
        1 => ctx.notice(
            NoteLevel::Info,
            "1 message was already delivered, so it stays sent.",
        ),
        n => ctx.notice(
            NoteLevel::Info,
            format!("{n} messages were already delivered, so they stay sent."),
        ),
    }
    Ok(Vec::new())
}

/// `/follow-up [run] <request>`: a follow-up run of a merged one.
pub(crate) async fn continue_run(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: Option<RunId>,
    request: String,
) -> Handled {
    let list = ctx
        .services
        .conductor
        .list()
        .await
        .context("listing this repo's runs")?;
    // A follow-up builds on merged work; anything else is carried on with /resume.
    let found = match &run {
        Some(run) => list.iter().find(|summary| summary.run == *run),
        None => list
            .iter()
            .find(|summary| summary.state == RunState::Merged),
    };
    let merged = match (found, &run) {
        (Some(merged), _) if merged.state == RunState::Merged => merged,
        (found, named) => {
            let text = match (found, named) {
                (Some(other), _) => format!(
                    "{} isn't merged, so there's nothing to follow up on yet. /resume carries \
                     it on.",
                    other.run
                ),
                (None, Some(run)) => format!("This repo has no run {run}. /resume lists them."),
                (None, None) => {
                    "No merged run to follow up on. /resume goes on with an unfinished one."
                        .to_owned()
                }
            };
            ctx.notice(NoteLevel::Info, text);
            return Ok(Vec::new());
        }
    };
    let settings = ctx.settings();
    // It goes on the way the merged run went, unless All hands sends every task to the crew.
    let triage = argonauts_crew::triage::all_hands(&settings.triage);
    let mode = triage.as_ref().map_or(merged.mode, |crew| crew.mode);
    let follow_up = StartRun {
        request: request.clone(),
        mode,
        forced: false,
        estimate: None,
        triage,
        chat_turn: None,
        continues: Some(merged.run.clone()),
        overrides: ctx.settings.run_overrides(),
        include_uncommitted: settings.intake.include_uncommitted,
        default_agent: Some(settings.default_agent.clone()),
        approval_mode: (runs.approval_mode == Some(ApprovalMode::Supervised))
            .then_some(ApprovalMode::Supervised),
        front_end: None,
        instructions: Vec::new(),
        request_settings: Vec::new(),
    };
    let handle = match ctx
        .services
        .conductor
        .continue_run(&merged.run, follow_up)
        .await
    {
        Ok(handle) => handle,
        Err(err) => return super::refused(ctx, err, || "Couldn't start the follow-up".to_owned()),
    };
    runs::started(ctx, runs, handle.run, title_from_task(&request), mode).await
}
