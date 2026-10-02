//! A turn from start to finish: pick a worker and launch its session, follow its events, then
//! read the reply. Vendor trouble hands the turn to the next fallback under the same id.

use std::time::{Duration, Instant};

use argonauts_core::NoteLevel;
use argonauts_core::Spend;
use argonauts_core::app::{AppEvent, AppMode, ChatEvent, TurnReply};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::names::{SessionKey, TurnId, WorkerRef};
use argonauts_core::steps::{StepError, StepOutcome};
use argonauts_core::views::ContextRow;
use argonauts_core::worker::WorkerEvent;

use super::excerpt::{Exchange, excerpt};
use super::offer::offer_to_start;
use super::{ChatState, Turn, emit, planned, run_request, send_queued, settle_mode, side};
use crate::chat_store::ChatSession;
use crate::controller::recap::recap_from_turn;
use crate::controller::triage::{Reading, read_answer, read_reply_with};
use crate::controller::turn::{self, Launch, Record, TurnStep};
use crate::controller::{Ctx, Effect};

/// Starts a new turn with the first worker that can take it.
pub(super) fn begin(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
    step: impl FnOnce(&Record) -> TurnStep,
) -> anyhow::Result<()> {
    chat.turns += 1;
    let id = TurnId::numbered(chat.turns);
    let record = Record::now(ctx);
    let step = step(&record);
    attempt(ctx, chat, id, text, step, record, Vec::new())
}

/// Starts a turn kept out of the conversation, numbered apart from its turns.
pub(super) fn begin_side(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
    step: TurnStep,
) -> anyhow::Result<()> {
    chat.sides += 1;
    let id = side::side_turn_id(chat.sides);
    let record = Record::now(ctx);
    attempt(ctx, chat, id, text, step, record, Vec::new())
}

/// One try at a turn: record it, then start its session. A fallback's try records the turn
/// again under the same id, naming the new worker.
fn attempt(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: TurnId,
    text: String,
    step: TurnStep,
    record: Record,
    tried: Vec<WorkerRef>,
) -> anyhow::Result<()> {
    let default = ctx.settings().default_agent.clone();
    let picked = match &step {
        step if step.main_agent_only() => turn::pick_main(ctx),
        TurnStep::Review { .. } => side::reviewer(ctx, &tried),
        _ => turn::pick(ctx, &tried),
    };
    let (worker, handle) = picked.map_err(|err| {
        anyhow::anyhow!("No agent can read this: {err}. /model picks another default agent.")
    })?;
    let reviewing = matches!(step, TurnStep::Review { .. });
    if tried.is_empty() && worker != default && !reviewing {
        let why = if turn::vendor_out(ctx, &default) {
            "is out of quota or down"
        } else {
            "isn't available"
        };
        let (default, worker) = (ctx.names.worker(&default), ctx.names.worker(&worker));
        ctx.notice(
            NoteLevel::Info,
            format!("{default} {why}, so {worker} reads this."),
        );
    }
    emit(
        ctx,
        chat,
        ChatEvent::TurnStarted {
            turn: id.clone(),
            text: text.clone(),
            worker: worker.clone(),
            read_only: true,
            commit: record.commit,
            uncommitted: record.uncommitted,
            settings_sha256: Some(ctx.settings.sha256().to_owned()),
        },
    );
    // A vendor that can't resume gets the chat so far as a handoff note instead.
    let can_resume = handle.capabilities().resume && !step.own_session();
    let resumable = chat.session.as_ref().filter(|session| {
        ctx.settings().sessions.chat_resumes && can_resume && session.worker == worker
    });
    // Two processes must never share one vendor session.
    if resumable.is_some() && !chat.stopping.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "The stopped turn is still ending, so this one starts a fresh session.",
        );
    }
    let resume = resumable
        .filter(|_| chat.stopping.is_empty())
        .map(|session| session.vendor_session.clone());
    // A reviewer judges the changes alone, as a run's reviewers do, and a vendor command must
    // be the whole prompt, or the CLI won't read it as one.
    let excerpt = if resume.is_none() && !reviewing && !step.main_agent_only() {
        excerpt(&chat.history)
    } else {
        None
    };
    let notes = if step.main_agent_only() {
        Vec::new()
    } else {
        chat.prefs.notes()
    };
    // A CLI session carried on keeps its model, unless you set one for the agent.
    let mut launch_worker = worker.clone();
    if resume.is_some() && turn::chat_agent(ctx.settings(), &worker).0.model.is_none() {
        launch_worker.model = chat.carried_model.clone();
    }
    let launched = turn::launch(
        ctx,
        Launch {
            chat: &chat.id,
            turn: &id,
            step: &step,
            text: &text,
            worker: launch_worker,
            handle,
            resume,
            excerpt,
            notes,
            fast: chat.prefs.fast,
        },
    );
    let (key, cancel) = match launched {
        Ok(launched) => launched,
        Err(err) => {
            emit(
                ctx,
                chat,
                ChatEvent::TurnFailed {
                    turn: id.clone(),
                    error: format!("{err:#}"),
                },
            );
            settle_mode(ctx, &id);
            return Err(err);
        }
    };
    if !step.own_session() {
        track_context(chat, key, &worker);
    }
    if ctx.mode == AppMode::Idle {
        ctx.set_mode(AppMode::Answering { turn: id.clone() });
    }
    chat.turn = Some(Turn {
        id,
        step,
        text,
        worker,
        tried,
        cancel,
        started: Instant::now(),
        run_began: false,
        plan_first: false,
    });
    Ok(())
}

/// The conversation's row for `/context` follows its session.
fn track_context(chat: &mut ChatState, key: SessionKey, worker: &WorkerRef) {
    let previous = chat.context.take();
    chat.context = Some(ContextRow {
        run: None,
        session: key,
        label: "chat".to_owned(),
        worker: worker.clone(),
        used: previous.as_ref().map_or(0, |row| row.used),
        window: previous.as_ref().and_then(|row| row.window),
        cap: None,
        tool_calls: previous.as_ref().map_or(0, |row| row.tool_calls),
        spent: previous.map(|row| row.spent).unwrap_or_default(),
        rotations: 0,
    });
}

pub(super) fn on_event(ctx: &mut Ctx, chat: &mut ChatState, id: &TurnId, event: WorkerEvent) {
    // Events from a cancelled turn's session may still arrive; they belong to no turn now.
    let Some(turn) = chat.turn.as_ref().filter(|turn| &turn.id == id) else {
        return;
    };
    match event {
        WorkerEvent::Agent(event) => {
            // Another session's context isn't the conversation's.
            let own = turn.step.own_session();
            if let Some(row) = chat.context.as_mut().filter(|_| !own) {
                match &event {
                    AgentEvent::ToolStarted { .. } => row.tool_calls += 1,
                    AgentEvent::ContextFill { used, size } => {
                        row.used = *used;
                        row.window = size.or(row.window);
                    }
                    _ => {}
                }
            }
            emit(
                ctx,
                chat,
                ChatEvent::Agent {
                    turn: id.clone(),
                    event,
                },
            );
        }
        // Saved at once, so a turn cancelled halfway still leaves a session to resume.
        WorkerEvent::Session(SessionEvent::VendorSession { id: vendor, .. })
            if !turn.step.own_session() =>
        {
            let session = ChatSession {
                worker: turn.worker.clone(),
                vendor_session: vendor,
            };
            remember_session(ctx, chat, session);
        }
        WorkerEvent::Session(_) | WorkerEvent::Vendor(_) => {}
    }
}

pub(super) fn on_done(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: &TurnId,
    outcome: Result<StepOutcome, StepError>,
) -> anyhow::Result<Vec<Effect>> {
    let end = match &outcome {
        Ok(outcome) => Some(&outcome.end),
        Err(StepError::Session(end)) => Some(end.as_ref()),
        Err(_) => None,
    };
    if let Some(end) = end
        && let Some(row) = chat.context.as_mut()
    {
        row.spent = plus(&row.spent, &end.spend);
    }
    // A cancelled turn's step ended, so its vendor session is free again. Its reply belongs
    // to no turn now.
    if let Some(at) = chat.stopping.iter().position(|stopping| stopping == id) {
        chat.stopping.remove(at);
        return Ok(Vec::new());
    }
    let Some(turn) = chat.turn.take_if(|turn| &turn.id == id) else {
        return Ok(Vec::new());
    };
    if let Some(vendor) = end
        .and_then(|end| end.vendor_session.as_ref())
        .filter(|_| !turn.step.own_session())
    {
        let session = ChatSession {
            worker: turn.worker.clone(),
            vendor_session: vendor.clone(),
        };
        remember_session(ctx, chat, session);
    }
    let spend = end.map(|end| end.spend).unwrap_or_default();
    let reading = match &turn.step {
        TurnStep::Triage { .. } => {
            let store = crate::learning::store(&ctx.env);
            read_reply_with(&outcome, ctx.settings(), Some(&store))
        }
        TurnStep::Vendor { .. } => read_vendor_reply(&outcome),
        _ => read_answer(&outcome),
    };
    match reading {
        Ok(reading) => Ok(finish(ctx, chat, turn, reading, spend)),
        Err(err) => fail(ctx, chat, turn, &err),
    }
}

fn finish(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: Turn,
    reading: Reading,
    spend: Spend,
) -> Vec<Effect> {
    let elapsed = turn.started.elapsed();
    let mut effects = Vec::new();
    let mut offer = None;
    // A run that began meanwhile answers the request; a second would duplicate its work.
    let may_start = !turn.run_began
        && matches!(&ctx.mode, AppMode::Answering { turn: current } if current == &turn.id);
    if !may_start && !matches!(reading, Reading::Answer { .. }) {
        ctx.notice(
            NoteLevel::Info,
            "A run started while this request was read, so it starts no other.",
        );
    }
    let plan_first = turn.plan_first || chat.prefs.plan_first;
    let reply = match reading {
        Reading::Answer { markdown } => TurnReply::Answer { markdown },
        Reading::Change {
            markdown,
            estimate,
            triage,
        } => {
            let mut request = run_request(
                ctx,
                turn.text.clone(),
                triage.mode,
                Some(estimate.clone()),
                Some(turn.id.clone()),
            );
            request.forced = false;
            request.triage = Some(triage.clone());
            if plan_first {
                request = planned(request);
            }
            if may_start {
                effects.push(Effect::StartRun(Box::new(request)));
            }
            TurnReply::Change {
                markdown,
                estimate,
                triage,
            }
        }
        Reading::Unclear {
            markdown,
            estimate,
            would_pick,
        } => {
            offer = may_start.then(|| (estimate.clone(), would_pick));
            TurnReply::Unclear {
                markdown,
                estimate,
                would_pick,
            }
        }
    };
    if !turn.step.kept_out() {
        chat.history.push(Exchange::new(turn.text.clone(), &reply));
    }
    emit(
        ctx,
        chat,
        ChatEvent::TurnFinished {
            turn: turn.id.clone(),
            reply: reply.clone(),
            spend,
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        },
    );
    if let Some((estimate, would_pick)) = offer {
        offer_to_start(ctx, chat, &turn, estimate, would_pick);
    }
    effects.extend(side::after(ctx, chat, &turn.step, &reply));
    if !turn.step.kept_out() && recap_due(ctx, elapsed) {
        ctx.send(AppEvent::Recap {
            text: recap_from_turn(&turn.text, &reply),
            model: None,
        });
    }
    settle_mode(ctx, &turn.id);
    effects.extend(send_queued(chat));
    effects
}

/// Vendor trouble hands the turn to the next fallback; anything else ends it.
fn fail(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: Turn,
    err: &StepError,
) -> anyhow::Result<Vec<Effect>> {
    let error = format!("{} couldn't answer: {}", turn.worker, turn::describe(err));
    emit(
        ctx,
        chat,
        ChatEvent::TurnFailed {
            turn: turn.id.clone(),
            error,
        },
    );
    let mut tried = turn.tried;
    tried.push(turn.worker.clone());
    let fallback_left = turn::candidates(ctx.settings())
        .iter()
        .any(|worker| !tried.contains(worker) && ctx.services.workers.get(worker).is_ok());
    if turn::worth_a_fallback(err) && fallback_left && !turn.step.main_agent_only() {
        let record = Record::now(ctx);
        let step = match turn.step {
            TurnStep::Triage { .. } => TurnStep::Triage {
                uncommitted_note: record.uncommitted_note(),
            },
            other => other,
        };
        let id = turn.id.clone();
        if let Err(err) = attempt(ctx, chat, turn.id, turn.text, step, record, tried) {
            settle_mode(ctx, &id);
            // No turn is in flight now, so what was typed meanwhile goes out at once.
            ctx.error(&err);
            return Ok(send_queued(chat));
        }
        if let Some(retry) = chat.turn.as_mut() {
            retry.run_began = turn.run_began;
        }
        return Ok(Vec::new());
    }
    settle_mode(ctx, &turn.id);
    Ok(send_queued(chat))
}

/// The CLI's own answer to its command, as it wrote it.
fn read_vendor_reply(outcome: &Result<StepOutcome, StepError>) -> Result<Reading, StepError> {
    let end = &outcome.as_ref().map_err(Clone::clone)?.end;
    let markdown = end
        .result_text
        .clone()
        .or_else(|| end.last_message.clone())
        .unwrap_or_default();
    Ok(Reading::Answer { markdown })
}

fn recap_due(ctx: &Ctx, elapsed: Duration) -> bool {
    let ui = &ctx.settings().ui;
    if !ui.recaps {
        return false;
    }
    match ui.recap_after.parse() {
        Ok(Some(after)) => elapsed >= after,
        Ok(None) => false,
        Err(err) => {
            ctx.notice(
                NoteLevel::Warn,
                format!("ui.recap_after isn't a span: {err}"),
            );
            false
        }
    }
}

fn remember_session(ctx: &Ctx, chat: &mut ChatState, session: ChatSession) {
    if chat.session.as_ref() == Some(&session) {
        return;
    }
    if let Err(err) = chat.store.save_session(&session) {
        ctx.notice(
            NoteLevel::Warn,
            format!("The next turn can't resume this session: {err:#}"),
        );
    }
    chat.session = Some(session);
}

fn plus(a: &Spend, b: &Spend) -> Spend {
    Spend {
        input_tokens: a.input_tokens.saturating_add(b.input_tokens),
        output_tokens: a.output_tokens.saturating_add(b.output_tokens),
        cached_tokens: a.cached_tokens.saturating_add(b.cached_tokens),
        cost_usd: a.cost_usd + b.cost_usd,
        cost_partial: a.cost_partial || b.cost_partial,
        cost_estimated: a.cost_estimated || b.cost_estimated,
    }
}
