//! A turn from start to finish: pick a worker and launch its session, follow its events, then
//! read the reply. Vendor trouble hands the turn to the next fallback under the same id.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use argonauts_core::NoteLevel;
use argonauts_core::Spend;
use argonauts_core::app::{AppEvent, AppMode, ChatEvent, TurnReply};
use argonauts_core::event::{AgentEvent, ErrorCategory};
use argonauts_core::events::run::{RunMode, TriageDecision};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::names::{SessionKey, TurnId, WorkerRef};
use argonauts_core::screen::{Drawn, ScreenChange};
use argonauts_core::steps::intake::{ChangeEstimate, Instruction};
use argonauts_core::steps::{StepError, StepOutcome};
use argonauts_core::views::ContextRow;
use argonauts_core::worker::WorkerEvent;
use argonauts_crew::triage::Asked;

use super::excerpt::{Exchange, excerpt};
use super::live::{self, Heard};
use super::mirror;
use super::offer::offer_to_start;
use super::{
    ChatState, Queued, Turn, agent, direct, emit, keep, planned, run_copy, run_request,
    send_queued, settle_mode, side, styled,
};
use crate::chat_store::ChatSession;
use crate::controller::instructions::{self, Command, Roster};
use crate::controller::recap::{self, recap_from_turn};
use crate::controller::shell;
use crate::controller::triage::{Reading, Requested, read_answer, read_reply_with};
use crate::controller::turn::{self, Launch, Record, TurnStep};
use crate::controller::{Ctx, Effect};
use argonauts_conductor::handoff::shortlist::auto_check;
use argonauts_core::events::session::InputPart;
use argonauts_core::settings::handoff::HandoffMode;
use argonauts_core::settings::workflows::RunStyle;

/// Starts a new turn with the first worker that can take it. A conversation that carries on an
/// agent's session waits while its run holds the session again.
pub(super) fn begin(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
    step: impl FnOnce(&Record) -> TurnStep,
) -> anyhow::Result<()> {
    if !place_ready(ctx, chat)? {
        return Ok(());
    }
    chat.turns += 1;
    let id = TurnId::numbered(chat.turns);
    let record = Record::now(ctx);
    let step = step(&record);
    attempt(ctx, chat, id, text, step, record, Vec::new(), None)
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
    attempt(ctx, chat, id, text, step, record, Vec::new(), None)
}

/// Whether a turn may start where the conversation works. One that carries on an agent's
/// session from a run waits while the run is going again, since its next round resumes the
/// same CLI session. A copy gone since, such as one another app removed when it started, is
/// made again.
pub(super) fn place_ready(ctx: &mut Ctx, chat: &mut ChatState) -> anyhow::Result<bool> {
    let Some(place) = chat.place.as_mut() else {
        return Ok(true);
    };
    if agent::run_holds(ctx, place) {
        let run = &place.agent.run;
        ctx.notice(
            NoteLevel::Info,
            format!(
                "{run} is going again, so it owns {} until it ends, and this conversation \
                 waits. t in its agent view takes it over.",
                place.agent
            ),
        );
        return Ok(false);
    }
    let made_again =
        run_copy::ensure_copy(&ctx.env.repo_root, &place.folder, place.branch.as_deref())
            .with_context(|| format!("{} can't carry on", chat.id))?;
    if made_again {
        place.made_again = true;
        chat.store.save_place(place)?;
        ctx.notice(
            NoteLevel::Info,
            format!(
                "The run's copy in {} was gone, so it was made again from the branch.",
                ctx.env.display(&place.folder)
            ),
        );
    }
    Ok(true)
}

/// A direct turn carried on by the next agent after the last one ran out.
struct Handoff {
    from: WorkerRef,
    /// Who takes it over: the shortlist's pick, or yours on the handoff card.
    to: WorkerRef,
    why: String,
    told: String,
    note: Option<InputPart>,
}

/// One try at a turn: record it, then start its session. A fallback's try records the turn
/// again under the same id, naming the new worker; a direct turn's handoff records who took it
/// over instead, since the turn goes on.
#[allow(clippy::too_many_arguments)]
fn attempt(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: TurnId,
    text: String,
    step: TurnStep,
    record: Record,
    tried: Vec<WorkerRef>,
    handoff: Option<Handoff>,
) -> anyhow::Result<()> {
    let default = ctx.settings().default_agent.clone();
    // An agent's session carried on from a run stays with that agent, its CLI's own commands
    // too, until it runs out.
    let pinned = chat
        .place
        .as_ref()
        .map(|place| place.worker.clone())
        .filter(|_| tried.is_empty() && !step.own_session());
    let picked = match (&step, &handoff, &pinned) {
        (_, Some(handoff), _) => ctx
            .services
            .workers
            .get(&handoff.to)
            .map(|handle| (handoff.to.clone(), handle)),
        (TurnStep::Review { .. }, None, _) => side::reviewer(ctx, &tried),
        (_, None, Some(worker)) => ctx
            .services
            .workers
            .get(worker)
            .map(|handle| (worker.clone(), handle)),
        (step, None, None) if step.main_agent_only() => turn::pick_main(ctx),
        (_, None, None) => turn::pick(ctx, &tried),
    };
    let (worker, handle) = picked.map_err(|err| {
        anyhow::anyhow!("No agent can read this: {err}. /model picks another default agent.")
    })?;
    let reviewing = matches!(step, TurnStep::Review { .. });
    if tried.is_empty() && worker != default && !reviewing && handoff.is_none() && pinned.is_none()
    {
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
    let mut handoff_note = None;
    let started = match handoff {
        Some(handoff) => {
            handoff_note = handoff.note;
            ChatEvent::HandedOff {
                turn: id.clone(),
                from: handoff.from,
                to: worker.clone(),
                why: handoff.why,
                told: handoff.told,
            }
        }
        None => ChatEvent::TurnStarted {
            turn: id.clone(),
            text: text.clone(),
            worker: worker.clone(),
            read_only: !step.writes(),
            commit: record.commit,
            uncommitted: record.uncommitted,
            settings_sha256: Some(ctx.settings.sha256().to_owned()),
            continues: step.continues(),
        },
    };
    emit(ctx, chat, started);
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
    // Argonauts adds nothing of its own to a direct turn but what it must tell the agent.
    let notes = if step.main_agent_only() {
        Vec::new()
    } else if step.writes() {
        let mut notes = direct::notes(chat);
        notes.extend(handoff_note);
        notes
    } else {
        chat.prefs.notes()
    };
    let ran = if step.hears_shell() {
        super::take_shell(chat)
    } else {
        Vec::new()
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
            shell: shell::context_part(&ran),
            fast: chat.prefs.fast,
            workdir: chat.folder(ctx).to_path_buf(),
        },
    );
    let turn::Launched {
        key,
        cancel,
        control,
    } = match launched {
        Ok(launched) => launched,
        Err(err) => {
            chat.ran.splice(0..0, ran);
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
        chat.workflows.clear();
    }
    if ctx.mode == AppMode::Idle {
        ctx.set_mode(AppMode::Answering { turn: id.clone() });
    }
    let plain_reply = step.plain_reply();
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
        ran,
        direct: None,
        live: live::Live::new(control, plain_reply),
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
    // Claude Code sends nothing while its question waits on a card, which isn't a stall.
    let stall = matches!(
        event,
        WorkerEvent::Agent(AgentEvent::Error {
            category: ErrorCategory::Stalled,
            ..
        })
    );
    if stall && direct::waits_on_you(chat, id) {
        return;
    }
    // Events from a cancelled turn's session may still arrive; they belong to no turn now.
    let Some(turn) = chat.turn.as_mut().filter(|turn| &turn.id == id) else {
        return;
    };
    match event {
        WorkerEvent::Agent(event) => {
            if let Some(running) = turn.direct.as_mut() {
                direct::on_agent(running, &event);
            }
            // The CLI's own model list, so `/model` mirrors it.
            if let AgentEvent::ModelsReported { models } = &event {
                ctx.services
                    .registry
                    .models_reported(&turn.worker.worker, models);
            }
            if !turn.step.own_session() && !live_event(ctx, chat, id, &event) {
                return;
            }
            let Some(turn) = chat.turn.as_mut().filter(|turn| &turn.id == id) else {
                return;
            };
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
            let event = turn.live.shown(event);
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

/// What the session says about your messages and the screen, as the turn's own `Live` reads
/// it. False for an event that's done with here and goes no further.
fn live_event(ctx: &mut Ctx, chat: &mut ChatState, id: &TurnId, event: &AgentEvent) -> bool {
    let heads_up = ctx.settings().ui.heads_up;
    let Some(turn) = chat.turn.as_mut().filter(|turn| &turn.id == id) else {
        return false;
    };
    let heard = match event {
        AgentEvent::Init {
            capabilities,
            plugins,
            ..
        } => {
            if !turn.live.on_init(capabilities, plugins, heads_up) {
                // The mod is off, the CLI is another, or you turned heads-ups off here.
                let nothing = AgentEvent::ScreenChanged {
                    change: ScreenChange::Drawn {
                        drawn: Drawn::default(),
                    },
                };
                emit(
                    ctx,
                    chat,
                    ChatEvent::Agent {
                        turn: id.clone(),
                        event: nothing,
                    },
                );
            }
            return true;
        }
        AgentEvent::MessageState { uuid, state } => turn.live.on_message(uuid, *state),
        AgentEvent::Workflow { workflow } => {
            mirror::seen(&mut chat.workflows, workflow);
            None
        }
        // The controller keeps the list for the `/` menu; the screen and the journal don't.
        AgentEvent::CommandsChanged { .. } => return false,
        AgentEvent::ScreenChanged { change } => {
            if turn.live.on_screen(change) && !std::mem::replace(&mut chat.heads_up_warned, true) {
                ctx.notice(
                    NoteLevel::Info,
                    "Claude Code's heads-ups can't be shown here with this version. They still \
                     show in Claude Code.",
                );
            }
            None
        }
        _ => None,
    };
    match heard {
        Some(Heard::Read(text)) => {
            if let Some(turn) = chat.turn.as_mut() {
                turn.text = format!("{}\n\n{text}", turn.text);
            }
            emit(
                ctx,
                chat,
                ChatEvent::Joined {
                    turn: id.clone(),
                    text,
                },
            );
        }
        Some(Heard::TakenBack(text)) => ctx.send(AppEvent::TakenBack { texts: vec![text] }),
        Some(Heard::AlreadyRead) => {
            ctx.notice(NoteLevel::Info, "Claude already read that message.");
        }
        Some(Heard::NotSent(text)) => chat.queue(Queued::Request(text)),
        None => {}
    }
    true
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
        direct::stopped(ctx, chat, id);
        return Ok(Vec::new());
    }
    let Some(mut turn) = chat.turn.take_if(|turn| &turn.id == id) else {
        return Ok(Vec::new());
    };
    // What the model never read goes first in line for the next turn.
    for text in turn.live.unread().into_iter().rev() {
        chat.queued
            .push_front((Queued::Request(text), chat.last_shell));
    }
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
        TurnStep::Triage { command, .. } => {
            let store = crate::learning::store(&ctx.env);
            // `/solo` and `/crew` are commands, and so is `/plan`, whose crew beats words that ask
            // for solo.
            let command = command.clone().or_else(|| {
                (turn.plan_first || chat.prefs.plan_first).then(|| Command {
                    mode: RunMode::Crew,
                    said: "you typed /plan".to_owned(),
                    style: None,
                })
            });
            let check = |how_to_run: &[Instruction]| {
                let roster = Roster::new(ctx);
                instructions::check(
                    how_to_run,
                    &turn.text,
                    ctx.settings(),
                    &roster,
                    command.as_ref(),
                )
            };
            read_reply_with(&outcome, ctx.settings(), Some(&store), &check)
        }
        step if step.plain_reply() => {
            read_vendor_reply(&outcome).map(|r| (r, Requested::default()))
        }
        _ => read_answer(&outcome).map(|r| (r, Requested::default())),
    };
    match reading {
        Ok((reading, requested)) => Ok(finish(ctx, chat, turn, reading, requested, spend)),
        Err(err) => fail(ctx, chat, turn, &err),
    }
}

fn finish(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    mut turn: Turn,
    reading: Reading,
    requested: Requested,
    spend: Spend,
) -> Vec<Effect> {
    let elapsed = turn.started.elapsed();
    let mut effects = Vec::new();
    let mut offer = None;
    // A run that began meanwhile answers the request; a second would duplicate its work. A
    // command asked for its own, whatever else runs.
    let by_command = matches!(
        &turn.step,
        TurnStep::Triage {
            command: Some(_),
            ..
        }
    );
    let may_start = !turn.run_began
        && (by_command
            || matches!(&ctx.mode, AppMode::Answering { turn: current } if current == &turn.id));
    if !may_start && !matches!(reading, Reading::Answer { .. }) {
        ctx.notice(
            NoteLevel::Info,
            "A run started while this request was read, so it starts no other.",
        );
    }
    let plan_first = turn.plan_first || chat.prefs.plan_first;
    // `/solo` or `/crew` chose the mode, so whatever the estimate made of the request, the
    // command's work starts, with the words it read.
    let command = match &turn.step {
        TurnStep::Triage { command, .. } => command.clone(),
        _ => None,
    };
    let reading = match &command {
        Some(command) => commanded(ctx, reading, command, &requested),
        None => reading,
    };
    // A solo that works in your checkout: what your words change about its turn, and whether
    // plan first has it wait for your go-ahead.
    let mut go_ahead = None;
    let mut ask_first = false;
    let mut work = |estimate: Option<ChangeEstimate>, triage: &TriageDecision| {
        let mut request = run_request(
            ctx,
            turn.text.clone(),
            triage.mode,
            estimate,
            Some(turn.id.clone()),
        );
        request.forced = command.is_some();
        request.triage = Some(triage.clone());
        requested.applied.apply_to(&mut request);
        if let Some(style) = command.as_ref().and_then(|command| command.style) {
            request = styled(request, style);
        }
        if plan_first {
            request = planned(request);
        }
        // `/plan` is a crew command, so only plan first turned on reaches direct solo, where
        // the estimate's reply is the plan to approve.
        if may_start && !turn.plan_first && direct::triage_picks_direct(triage) {
            let main = &ctx.settings().default_agent.worker;
            go_ahead = Some(requested.applied.for_direct(main));
            ask_first = plan_first;
        } else if may_start {
            effects.push(Effect::StartRun(Box::new(request)));
        }
        go_ahead.is_some()
    };
    let reply = match reading {
        // A command's request that the estimate took for a question still starts its work.
        Reading::Answer { markdown } => {
            if let Some(command) = &command {
                work(None, &commanded_triage(ctx, command, &requested, None));
            }
            TurnReply::Answer { markdown }
        }
        Reading::Change {
            markdown,
            estimate,
            triage,
        } => {
            let direct = work(Some(estimate.clone()), &triage);
            // A run's Triage line has the instructions from its start; direct solo has no run.
            let instructions = if direct {
                requested.applied.instructions.clone()
            } else {
                Vec::new()
            };
            TurnReply::Change {
                markdown,
                estimate,
                triage,
                instructions,
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
    match offer {
        Some((estimate, would_pick)) => {
            offer_to_start(ctx, chat, &turn, estimate, would_pick, requested);
        }
        None => keep::offer(ctx, chat, &requested.applied),
    }
    effects.extend(side::after(ctx, chat, &turn.step, &reply));
    // A direct turn's answer already says what changed, so no recap restates it.
    direct::finished(ctx, chat, &mut turn);
    direct::offer_plan(ctx, chat, &turn);
    if !turn.step.kept_out() && !turn.step.in_your_checkout() && recap_due(ctx, elapsed) {
        ctx.send(AppEvent::Recap {
            text: recap_from_turn(&turn.text, &reply),
            model: None,
        });
    }
    settle_mode(ctx, &turn.id);
    // A solo the triage rules picked, run direct: the session that just planned it goes ahead
    // in your checkout.
    match go_ahead {
        Some(words) if ask_first => direct::offer_go_ahead(ctx, chat, &turn, words),
        Some(words) => {
            match direct::go_ahead(ctx, chat, turn.text.clone(), direct::GO_AHEAD, None, words) {
                Ok(started) => effects.extend(started),
                Err(err) => ctx.error(&err),
            }
        }
        None => {}
    }
    effects.extend(send_queued(chat));
    effects
}

/// A reading for `/solo` or `/crew`: the command's mode, with where solo works as your words
/// and `solo.style` say. An unclear request with an estimate is a change now, and one without
/// stays an answer, whose work starts all the same.
fn commanded(ctx: &Ctx, reading: Reading, command: &Command, requested: &Requested) -> Reading {
    match reading {
        Reading::Change {
            markdown, estimate, ..
        }
        | Reading::Unclear {
            markdown,
            estimate: Some(estimate),
            ..
        } => {
            let triage = commanded_triage(ctx, command, requested, Some(&estimate));
            Reading::Change {
                markdown,
                estimate,
                triage,
            }
        }
        Reading::Unclear {
            markdown,
            estimate: None,
            ..
        }
        | Reading::Answer { markdown } => Reading::Answer { markdown },
    }
}

fn commanded_triage(
    ctx: &Ctx,
    command: &Command,
    requested: &Requested,
    estimate: Option<&ChangeEstimate>,
) -> TriageDecision {
    let asked = Asked {
        mode: Some(command.mode),
        solo: requested.applied.asked.solo,
    };
    // The rules read the estimate only when nothing chose the mode, and the command did.
    let none = ChangeEstimate {
        task_type: String::new(),
        summary: String::new(),
        files_likely: Vec::new(),
        file_count: 0,
        risk_labels: Vec::new(),
        tests_exist: false,
        unknowns: Vec::new(),
    };
    argonauts_crew::triage::decide_with(estimate.unwrap_or(&none), ctx.settings(), None, asked)
}

/// Vendor trouble hands the turn to the next fallback; anything else ends it.
fn fail(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: Turn,
    err: &StepError,
) -> anyhow::Result<Vec<Effect>> {
    if turn.step.writes() {
        return fail_direct(ctx, chat, turn, err);
    }
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
            TurnStep::Triage {
                read_instructions,
                command,
                ..
            } => TurnStep::Triage {
                uncommitted_note: record.uncommitted_note(),
                read_instructions,
                command,
            },
            other => other,
        };
        let id = turn.id.clone();
        // The next try tells the lead about them again, ahead of any run since.
        chat.ran.splice(0..0, turn.ran);
        if let Err(err) = attempt(ctx, chat, turn.id, turn.text, step, record, tried, None) {
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
    let mut effects = Vec::new();
    // A command's run starts without the words when no agent could read them, as it did before
    // words were read. Solo in your checkout needs the agent that just failed, so it doesn't.
    if let TurnStep::Triage {
        command: Some(command),
        ..
    } = &turn.step
        && !turn.run_began
        && (command.mode == RunMode::Crew || !direct::solo_is_direct(ctx.settings()))
    {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "No agent could read how your words say to run it, so /{} starts as it is.",
                command
                    .style
                    .map_or_else(|| recap::mode_name(command.mode), RunStyle::as_str)
            ),
        );
        let mut request = run_request(ctx, turn.text.clone(), command.mode, None, None);
        if let Some(style) = command.style {
            request = styled(request, style);
        }
        effects.push(Effect::StartRun(Box::new(request)));
    }
    effects.extend(send_queued(chat));
    Ok(effects)
}

/// A direct turn whose agent ran out goes on in the same folder, as `handoff.mode` says and from
/// the shortlist a run's card would get: in auto mode the first candidate auto may take, and
/// with `ask`, or when only paid APIs are left, the handoff card. Anything else ends it, with a
/// line saying what it changed.
fn fail_direct(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: Turn,
    err: &StepError,
) -> anyhow::Result<Vec<Effect>> {
    let mut tried = turn.tried.clone();
    tried.push(turn.worker.clone());
    let mode = ctx.settings().handoff.mode;
    // Another CLI wouldn't know this one's own command.
    if turn::worth_a_fallback(err) && mode != HandoffMode::Wait && !turn.step.main_agent_only() {
        // Read after the agent's name: "hit its usage limit", "stopped: <reason>".
        let described = turn::describe(err);
        let why = described
            .strip_prefix("it ")
            .map_or_else(|| format!("stopped: {described}"), str::to_owned);
        let candidates = direct::shortlist(ctx, &turn.worker, &tried);
        let auto = candidates
            .iter()
            .find(|candidate| auto_check(candidate).is_ok())
            .filter(|_| mode == HandoffMode::Auto);
        if let Some(next) = auto {
            let to = next.worker.clone();
            return hand_off(ctx, chat, turn, tried, why, to);
        }
        if !candidates.is_empty() {
            let id = turn.id.clone();
            direct::ask_handoff(ctx, chat, turn, tried, why, &candidates);
            settle_mode(ctx, &id);
            return Ok(Vec::new());
        }
    }
    let error = format!("{} couldn't finish: {}", turn.worker, turn::describe(err));
    Ok(end_direct(ctx, chat, turn, error))
}

/// The next agent carries the turn on in the same folder, told what it changed so far.
pub(super) fn hand_off(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    mut turn: Turn,
    tried: Vec<WorkerRef>,
    why: String,
    to: WorkerRef,
) -> anyhow::Result<Vec<Effect>> {
    let (told, note) = direct::handoff(ctx, &turn);
    let handoff = Handoff {
        from: turn.worker.clone(),
        to,
        why,
        told,
        note,
    };
    let running = turn.direct.take();
    chat.ran.splice(0..0, std::mem::take(&mut turn.ran));
    let record = Record::now(ctx);
    let attempted = attempt(
        ctx,
        chat,
        turn.id.clone(),
        turn.text.clone(),
        turn.step.clone(),
        record,
        tried,
        Some(handoff),
    );
    match attempted {
        Ok(()) => {
            if let Some(retry) = chat.turn.as_mut() {
                retry.run_began = turn.run_began;
                retry.direct = running;
            }
            Ok(Vec::new())
        }
        Err(err) => {
            turn.direct = running;
            let error = format!("{err:#}");
            Ok(end_direct(ctx, chat, turn, error))
        }
    }
}

/// A direct turn that ends without its answer: the failure, and one line saying what changed.
pub(super) fn end_direct(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    mut turn: Turn,
    error: String,
) -> Vec<Effect> {
    emit(
        ctx,
        chat,
        ChatEvent::TurnFailed {
            turn: turn.id.clone(),
            error,
        },
    );
    direct::failed(ctx, chat, &mut turn);
    settle_mode(ctx, &turn.id);
    send_queued(chat)
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
    // A conversation that carries on an agent's session keeps that agent's session, whoever
    // took a turn when it ran out.
    if chat
        .place
        .as_ref()
        .is_some_and(|place| place.worker != session.worker)
    {
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
