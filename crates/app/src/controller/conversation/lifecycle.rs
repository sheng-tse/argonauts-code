//! A turn from start to finish: pick a worker and launch its session, follow its events, then
//! read the reply. Agent trouble hands the turn to the next fallback under the same id.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use argonauts_core::NoteLevel;
use argonauts_core::Spend;
use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, SignedOut, TurnReply};
use argonauts_core::event::{AgentEvent, ErrorCategory};
use argonauts_core::events::run::{CaptainOrCrewDecision, RunMode};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::names::{ModelRef, SessionKey, TurnId};
use argonauts_core::screen::{Drawn, ScreenChange};
use argonauts_core::session_runner::RunnerEvent;
use argonauts_core::steps::intake::{ChangeEstimate, Instruction};
use argonauts_core::steps::{StepError, StepOutcome};
use argonauts_core::views::ContextRow;
use argonauts_crew::captain_or_crew::Asked;

use super::carry;
use super::excerpt::Exchange;
use super::live::{self, Heard};
use super::mirror;
use super::offer::offer_to_start;
use super::{
    ConversationState, Turn, agent, direct, emit, goal, handover, keep, planned, run_request,
    run_worktree, send_queued, settle_mode, side, styled,
};
use crate::controller::captain_or_crew::{Reading, Requested, read_answer, read_reply_with};
use crate::controller::instructions::{self, Command, Roster};
use crate::controller::recap::{self, recap_from_turn};
use crate::controller::shell;
use crate::controller::turn::{self, Launch, Record, TurnStep};
use crate::controller::ultra;
use crate::controller::{Ctx, Effect};
use crate::conversation_store::ConversationSession;
use crate::usage_ledger::{TurnNote, UsageLedger};
use argonauts_conductor::fallback::shortlist::auto_check;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::fallback::FallbackMode;
use argonauts_core::settings::workflows::RunStyle;

/// Starts a new turn with the first worker that can take it. A conversation that carries on an
/// agent's session waits while its run holds the session again.
pub(super) fn begin(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
    step: impl FnOnce(&Record) -> TurnStep,
) -> anyhow::Result<()> {
    if !place_ready(ctx, conversation)? {
        return Ok(());
    }
    carry::sent(conversation);
    conversation.turns += 1;
    let id = TurnId::numbered(conversation.turns);
    let record = Record::now(ctx);
    let step = step(&record);
    try_turn(ctx, conversation, id, text, step, record, Vec::new(), None)
}

/// Starts a turn kept out of the conversation, numbered apart from its turns.
pub(super) fn begin_side(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
    step: TurnStep,
) -> anyhow::Result<()> {
    conversation.sides += 1;
    let id = side::side_turn_id(conversation.sides);
    let record = Record::now(ctx);
    try_turn(ctx, conversation, id, text, step, record, Vec::new(), None)
}

/// Starts the summary a main model on another CLI starts from, numbered apart and unseen.
pub(super) fn begin_carry(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    step: TurnStep,
) -> anyhow::Result<TurnId> {
    conversation.sides += 1;
    let id = carry::carry_turn_id(conversation.sides);
    let record = Record::now(ctx);
    try_turn(
        ctx,
        conversation,
        id.clone(),
        String::new(),
        step,
        record,
        Vec::new(),
        None,
    )?;
    Ok(id)
}

/// Whether a turn may start where the conversation works. One that carries on an agent's
/// session from a run waits while the run is going again, since its next round resumes the
/// same CLI session. A worktree gone since, such as one another app removed when it started, is
/// made again.
pub(super) fn place_ready(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<bool> {
    let Some(place) = conversation.place.as_mut() else {
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
        run_worktree::ensure_worktree(&ctx.env.repo_root, &place.folder, place.branch.as_deref())
            .with_context(|| format!("{} can't carry on", conversation.id))?;
    if made_again {
        place.made_again = true;
        conversation.store.save_place(place)?;
        ctx.notice(
            NoteLevel::Info,
            format!(
                "The run's worktree in {} was gone, so it was made again from the branch.",
                ctx.env.display(&place.folder)
            ),
        );
    }
    Ok(true)
}

/// A direct turn carried on by the next agent after the last one ran out.
struct Fallback {
    from: ModelRef,
    /// Who it falls back to: the shortlist's pick, or yours on the fallback question.
    to: ModelRef,
    why: String,
    told: String,
    note: Option<InputPart>,
    signed_out: Option<SignedOut>,
}

/// What a turn hears when the user's own words plainly ask for a workflow.
const ASKED_FOR_A_WORKFLOW: &str = "The user's own words ask for a workflow: call \
    argonauts_start_workflow with their request and the instructions in it, such as who \
    reviews, rather than doing the work yourself.";

/// What a turn hears when the user's own words name who reviews and ask for no workflow.
const NAMED_A_REVIEWER: &str = "The user's own words name who reviews: call \
    argonauts_start_workflow with their request and that instruction, so you do the work in a \
    checked worktree and the reviewer they named reads the change before it lands, rather than \
    changing their checkout unreviewed.";

/// One try at a turn: record it, then start its session. A fallback's try records the turn
/// again under the same id, naming the new worker; a direct turn's fallback records who it fell
/// back to instead, since the turn goes on.
#[allow(clippy::too_many_arguments)]
fn try_turn(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: TurnId,
    text: String,
    step: TurnStep,
    record: Record,
    tried: Vec<ModelRef>,
    fallback: Option<Fallback>,
) -> anyhow::Result<()> {
    let default = ctx.settings().crew.main.clone();
    // An agent's session carried on from a run stays with that agent, its CLI's own commands
    // too, until it runs out.
    let pinned = conversation
        .place
        .as_ref()
        .map(|place| place.worker.clone())
        .filter(|_| tried.is_empty() && !step.own_session());
    let picked = match (&step, &fallback, &pinned) {
        (_, Some(fallback), _) => ctx
            .services
            .workers
            .get(&fallback.to)
            .map(|handle| (fallback.to.clone(), handle)),
        (TurnStep::Review { .. }, None, _) => side::reviewer(ctx, &tried),
        (TurnStep::Carry { writer, .. }, None, _) => ctx
            .services
            .workers
            .get(writer)
            .map(|handle| (writer.clone(), handle)),
        (_, None, Some(worker)) => ctx
            .services
            .workers
            .get(worker)
            .map(|handle| (worker.clone(), handle)),
        (step, None, None) if step.main_agent_only() => turn::pick_main(ctx),
        (_, None, None) => turn::pick(ctx, &tried),
    };
    let (worker, handle) = picked.map_err(|err| {
        anyhow::anyhow!("No agent can read this: {err}. /model picks another main agent.")
    })?;
    let reviewing = matches!(step, TurnStep::Review { .. });
    let carrying = matches!(step, TurnStep::Carry { .. });
    if tried.is_empty()
        && worker != default
        && !reviewing
        && !carrying
        && fallback.is_none()
        && pinned.is_none()
    {
        let why = if turn::agent_out(ctx, &default) {
            "is out of plan usage or down"
        } else {
            "isn't available"
        };
        let (default, worker) = (ctx.names.worker(&default), ctx.names.worker(&worker));
        ctx.notice(
            NoteLevel::Info,
            format!("{default} {why}, so {worker} reads this."),
        );
    }
    let mut fallback_note = None;
    let started = match fallback {
        Some(fallback) => {
            fallback_note = fallback.note;
            ConversationEvent::FellBack {
                turn: id.clone(),
                from: fallback.from,
                to: worker.clone(),
                why: fallback.why,
                told: fallback.told,
                signed_out: fallback.signed_out,
            }
        }
        None => ConversationEvent::TurnStarted {
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
    emit(ctx, conversation, started);
    // An agent that can't resume gets the conversation so far as a fallback note instead.
    let can_resume = handle.capabilities().resume && !step.own_session();
    let can_fork = handle.capabilities().fork;
    // Another model on the same CLI carries the session on. A session to branch from is only
    // ever copied, so a CLI with no fork of its own starts fresh from the turns so far.
    let resumable = conversation.session.as_ref().filter(|session| {
        ctx.settings().sessions.conversation_resumes
            && can_resume
            && session.worker.agent == worker.agent
            && (!session.fork || can_fork)
    });
    // Two processes must never share one CLI session.
    if resumable.is_some() && !conversation.stopping.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "The stopped turn is still ending, so this one starts a fresh session.",
        );
    }
    let (resume, fork) = match &step {
        // The model being left writes the summary in its own session.
        TurnStep::Carry { resume, .. } => (
            resume.clone().filter(|_| handle.capabilities().resume),
            false,
        ),
        _ => resumable
            .filter(|_| conversation.stopping.is_empty())
            .map_or((None, false), |session| {
                (Some(session.cli_session.clone()), session.fork)
            }),
    };
    // A reviewer judges the changes alone, as a run's reviewers do, and a CLI command must
    // be the whole prompt, or the CLI won't read it as one.
    let excerpt = if resume.is_none() && !reviewing && !carrying && !step.main_agent_only() {
        carry::primer(ctx, conversation, handle.kind())
    } else {
        None
    };
    // Argonauts Code adds nothing of its own to a direct turn but what it must tell the agent.
    let notes = if step.main_agent_only() {
        Vec::new()
    } else if step.writes() {
        let mut notes = direct::notes(ctx, conversation, &step, resume.is_none());
        notes.extend(fallback_note);
        // Only a session that has the start tool can follow the ultracode line.
        notes.extend(
            ultra::standing_line(ctx.settings())
                .filter(|_| step.offers_tool(super::tools::start_workflow::NAME))
                .map(|line| turn::part(InputPartKind::Notes, "ultracode", line.to_owned())),
        );
        // Words that plainly ask for a workflow are told to the session in so many words, so
        // a turn that could do the work itself hands it on as asked.
        // A reviewer named with no workflow asked for is told the same way, so the session
        // keeps the work and has it reviewed before it lands.
        let asks_for_a_workflow =
            crate::controller::instructions::plainly_asks_for_a_workflow(&text);
        let names_a_reviewer = || {
            let roster = crate::controller::instructions::Roster::new(ctx);
            crate::controller::instructions::names_a_reviewer(&text, &roster)
        };
        let your_words = if asks_for_a_workflow {
            Some(ASKED_FOR_A_WORKFLOW)
        } else if names_a_reviewer() {
            Some(NAMED_A_REVIEWER)
        } else {
            None
        };
        notes.extend(
            your_words
                .filter(|_| step.offers_tool(super::tools::start_workflow::NAME))
                .map(|line| turn::part(InputPartKind::Notes, "your words", line.to_owned())),
        );
        // A goal you set is told to a turn in your checkout as well.
        notes.extend(goal::brief(ctx, conversation, &step, &worker, &text));
        notes
    } else {
        conversation.prefs.notes(ctx.settings())
    };
    let ran = if step.hears_shell() {
        super::take_shell(conversation)
    } else {
        Vec::new()
    };
    // A CLI session carried on keeps its model, unless you set one for the agent.
    let mut launch_worker = worker.clone();
    if resume.is_some()
        && turn::conversation_agent(ctx.settings(), &worker)
            .0
            .model
            .is_none()
    {
        launch_worker.model = conversation.carried_model.clone();
    }
    // The turn that carries your words takes your pictures with them.
    let pictures = if step.takes_mentions() {
        std::mem::take(&mut conversation.pictures)
    } else {
        Vec::new()
    };
    let launched = turn::launch(
        ctx,
        Launch {
            conversation: &conversation.id,
            turn: &id,
            step: &step,
            text: &text,
            pictures,
            worker: launch_worker,
            handle,
            resume,
            fork,
            excerpt,
            notes,
            shell: shell::context_part(&ran),
            fast: conversation.prefs.fast,
            workdir: conversation.folder(ctx).to_path_buf(),
        },
    );
    let turn::Launched {
        key,
        cancel,
        control,
    } = match launched {
        Ok(launched) => launched,
        Err(err) => {
            conversation.ran.splice(0..0, ran);
            emit(
                ctx,
                conversation,
                ConversationEvent::TurnFailed {
                    turn: id.clone(),
                    error: format!("{err:#}"),
                },
            );
            settle_mode(ctx, &id);
            return Err(err);
        }
    };
    if !step.own_session() {
        track_context(conversation, key, &worker);
        conversation.workflows.clear();
    }
    if ctx.mode == AppMode::Idle {
        ctx.set_mode(AppMode::Answering { turn: id.clone() });
    }
    let plain_reply = step.plain_reply();
    conversation.turn = Some(Turn {
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
        counted: None,
        signed_out: None,
    });
    if let (Some(turn), Some(detail)) = (&conversation.turn, conversation.context_watch) {
        turn.live.watch_context(Some(detail));
    }
    Ok(())
}

/// The conversation's row for `/context` follows its session.
fn track_context(conversation: &mut ConversationState, key: SessionKey, worker: &ModelRef) {
    let previous = conversation.context.take();
    conversation.context = Some(ContextRow {
        run: None,
        session: key,
        label: "conversation".to_owned(),
        worker: worker.clone(),
        used: previous.as_ref().map_or(0, |row| row.used),
        window: previous.as_ref().and_then(|row| row.window),
        cap: None,
        tool_calls: previous.as_ref().map_or(0, |row| row.tool_calls),
        spent: previous.map(|row| row.spent).unwrap_or_default(),
        rotations: 0,
        estimated: false,
        counts: false,
        categories: Vec::new(),
        recorded: None,
        ended: None,
    });
}

pub(super) fn on_event(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: &TurnId,
    event: RunnerEvent,
) {
    // Claude Code sends nothing while its question waits for your answer, which isn't a stall.
    let stall = matches!(
        event,
        RunnerEvent::Agent(AgentEvent::Error {
            category: ErrorCategory::Stalled,
            ..
        })
    );
    if stall && direct::waits_on_you(conversation, id) {
        return;
    }
    // Events from a cancelled turn's session may still arrive; they belong to no turn now.
    let Some(turn) = conversation.turn.as_mut().filter(|turn| &turn.id == id) else {
        return;
    };
    match event {
        RunnerEvent::Agent(event) => {
            if let Some(running) = turn.direct.as_mut() {
                direct::on_agent(running, &event);
            }
            // The CLI's own model list, so `/model` mirrors it.
            if let AgentEvent::ModelsReported { models } = &event {
                ctx.services
                    .registry
                    .models_reported(&turn.worker.agent, models);
            }
            if !turn.step.own_session() && !live_event(ctx, conversation, id, &event) {
                return;
            }
            // Each patch goes ahead of its edit's end, so the edit is drawn with it.
            let folder = conversation.folder(ctx).to_path_buf();
            for patch in conversation.edits.on_agent(&folder, &event) {
                let turn = id.clone();
                emit(
                    ctx,
                    conversation,
                    ConversationEvent::Agent { turn, event: patch },
                );
            }
            let Some(turn) = conversation.turn.as_mut().filter(|turn| &turn.id == id) else {
                return;
            };
            // Another session's context isn't the conversation's.
            let own = turn.step.own_session();
            if let Some(row) = conversation.context.as_mut().filter(|_| !own) {
                match &event {
                    // A CLI left on its own default says which model that is, so the
                    // ledger and Stats name the model rather than the CLI.
                    AgentEvent::Init {
                        model: Some(model), ..
                    } if row.worker.model.is_none() => row.worker.model = Some(model.clone()),
                    AgentEvent::ToolStarted { .. } => row.tool_calls += 1,
                    AgentEvent::ContextFill { used, size, .. } => {
                        row.used = *used;
                        row.window = size.or(row.window);
                    }
                    // The CLI's own count says more exactly what the window holds.
                    AgentEvent::ContextBreakdown { usage } => {
                        row.used = usage.total_tokens;
                        row.window = row.window.or(Some(usage.window));
                    }
                    _ => {}
                }
            }
            if let AgentEvent::ContextBreakdown { usage } = &event
                && !own
            {
                turn.counted = Some(usage.clone());
                // The count `/context print` waits for is in.
                if conversation.print_wait.is_some() {
                    conversation.print_wait = Some(super::PrintWait::Counted);
                }
            }
            let event = turn.live.shown(event);
            emit(
                ctx,
                conversation,
                ConversationEvent::Agent {
                    turn: id.clone(),
                    event,
                },
            );
        }
        // Saved at once, so a turn cancelled halfway still leaves a session to resume.
        RunnerEvent::Session(SessionEvent::CliSession { id: agent_id, .. })
            if !turn.step.own_session() =>
        {
            let session = ConversationSession {
                worker: turn.worker.clone(),
                cli_session: agent_id,
                fork: false,
            };
            remember_session(ctx, conversation, session);
        }
        RunnerEvent::Session(_) | RunnerEvent::AgentStatus(_) => {}
    }
}

/// What the session says about your messages and the screen, as the turn's own `Live` reads
/// it. False for an event that's done with here and goes no further.
fn live_event(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: &TurnId,
    event: &AgentEvent,
) -> bool {
    let notices = ctx.settings().ui.notices;
    let Some(turn) = conversation.turn.as_mut().filter(|turn| &turn.id == id) else {
        return false;
    };
    turn.live.on_agent(event);
    let heard = match event {
        AgentEvent::Init {
            capabilities,
            plugins,
            tools,
            ..
        } => {
            conversation.mcp_servers = Some(super::own_mcp_servers(tools));
            let Some(turn) = conversation.turn.as_mut().filter(|turn| &turn.id == id) else {
                return false;
            };
            if !turn.live.on_init(capabilities, plugins, notices) {
                // The mod is off, the CLI is another, or you turned notices off here.
                let nothing = AgentEvent::ScreenChanged {
                    change: ScreenChange::Drawn {
                        drawn: Drawn::default(),
                    },
                };
                emit(
                    ctx,
                    conversation,
                    ConversationEvent::Agent {
                        turn: id.clone(),
                        event: nothing,
                    },
                );
            }
            return true;
        }
        AgentEvent::MessageState { uuid, state } => turn.live.on_message(uuid, *state),
        AgentEvent::Workflow { workflow } => {
            mirror::seen(&mut conversation.workflows, workflow);
            None
        }
        // The controller keeps the list for the `/` menu; the screen and the journal don't.
        AgentEvent::CommandsChanged { .. } => return false,
        AgentEvent::ScreenChanged { change } => {
            if turn.live.on_screen(change)
                && !std::mem::replace(&mut conversation.notice_warned, true)
            {
                ctx.notice(
                    NoteLevel::Info,
                    "Claude Code's notices can't be shown here with this version. They still \
                     show in Claude Code.",
                );
            }
            None
        }
        _ => None,
    };
    match heard {
        Some(Heard::Read(text)) => {
            if let Some(turn) = conversation.turn.as_mut() {
                turn.text = format!("{}\n\n{text}", turn.text);
            }
            emit(
                ctx,
                conversation,
                ConversationEvent::Joined {
                    turn: id.clone(),
                    text,
                },
            );
        }
        Some(Heard::TakenBack(text)) => ctx.send(AppEvent::TakenBack { texts: vec![text] }),
        Some(Heard::AlreadyRead) => {
            ctx.notice(NoteLevel::Info, "Claude already read that message.");
        }
        Some(Heard::NotSent(queued)) => conversation.queue(queued),
        None => {}
    }
    true
}

/// The turn's answer didn't fit its format and its agent is asked again, so the screen sets
/// aside what it said so far.
pub(super) fn on_retried(ctx: &mut Ctx, conversation: &mut ConversationState, id: &TurnId) {
    if conversation
        .turn
        .as_ref()
        .is_some_and(|turn| &turn.id == id)
    {
        let turn = id.clone();
        emit(ctx, conversation, ConversationEvent::AnswerRetried { turn });
    }
}

pub(super) fn on_done(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: &TurnId,
    outcome: Result<StepOutcome, StepError>,
) -> anyhow::Result<Vec<Effect>> {
    let end = match &outcome {
        Ok(outcome) => Some(&outcome.end),
        Err(StepError::Session(end)) => Some(end.as_ref()),
        Err(_) => None,
    };
    if let Some(end) = end
        && let Some(row) = conversation.context.as_mut()
    {
        row.spent = plus(&row.spent, &end.spend);
        let turn = TurnNote {
            conversation: Some(conversation.id.as_str()),
            took: end.duration_ms / 1000,
        };
        UsageLedger::new(&ctx.env.paths).turn_ended(
            &ctx.env.repo_root,
            &row.worker,
            &end.spend,
            turn,
            time::OffsetDateTime::now_utc(),
        );
    }
    // A cancelled turn's step ended, so its CLI session is free again. Its reply belongs
    // to no turn now.
    if let Some(at) = conversation
        .stopping
        .iter()
        .position(|stopping| stopping == id)
    {
        conversation.stopping.remove(at);
        direct::stopped(ctx, conversation, id);
        carry::stopped(ctx, conversation, id);
        carry::start_if_due(ctx, conversation);
        return Ok(Vec::new());
    }
    let Some(mut turn) = conversation.turn.take_if(|turn| &turn.id == id) else {
        return Ok(Vec::new());
    };
    if matches!(turn.step, TurnStep::Carry { .. }) {
        return Ok(carry::summary_done(ctx, conversation, turn, &outcome));
    }
    // What the model never read goes first in line for the next turn.
    for queued in turn.live.unread().into_iter().rev() {
        conversation
            .queued
            .push_front((queued, conversation.last_shell));
    }
    if let Some(agent_id) = end
        .and_then(|end| end.cli_session.as_ref())
        .filter(|_| !turn.step.own_session())
    {
        let session = ConversationSession {
            worker: turn.worker.clone(),
            cli_session: agent_id.clone(),
            fork: false,
        };
        remember_session(ctx, conversation, session);
    }
    if !turn.step.own_session() {
        keep_context(ctx, conversation, &mut turn);
    }
    let spend = end.map(|end| end.spend).unwrap_or_default();
    let reading = match &turn.step {
        TurnStep::Estimate { command, .. } => {
            let store = crate::learning::store(&ctx.env);
            // `/captain` and `/crew` are commands, and so is `/plan`, whose crew beats words that
            // ask for Captain.
            let command = command.clone().or_else(|| {
                (turn.plan_first || conversation.prefs.plan_first).then(|| Command {
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
        step if step.plain_reply() => read_agent_reply(&outcome).map(|r| (r, Requested::default())),
        _ => read_answer(&outcome).map(|r| (r, Requested::default())),
    };
    let effects = if turn.live.stopped_turn() {
        Ok(end_stopped(ctx, conversation, turn, reading, spend))
    } else {
        match reading {
            Ok((reading, requested)) => {
                Ok(finish(ctx, conversation, turn, reading, requested, spend))
            }
            Err(err) => fail(ctx, conversation, turn, &err),
        }
    };
    // A switch made while this turn ran has its summary made now.
    carry::start_if_due(ctx, conversation);
    effects
}

/// The record keeps how the turn left its session's context, which a resumed conversation
/// counts from at once. The screen counted it as it came.
fn keep_context(ctx: &Ctx, conversation: &mut ConversationState, turn: &mut Turn) {
    let Some(row) = conversation.context.as_ref() else {
        return;
    };
    let left = ConversationEvent::ContextLeft {
        turn: turn.id.clone(),
        at: time::OffsetDateTime::now_utc(),
        used: row.used,
        window: row.window,
        usage: turn.counted.take(),
    };
    super::record(ctx, conversation, &left);
}

/// A turn you stopped alone ends when its session does, once what it ran in the background is
/// done. What the main agent said after that is its own words and never a request to start
/// work, with or without a verdict. With nothing said it ends as any turn Esc stops. A session
/// that failed says so, and no fallback takes the request you stopped.
fn end_stopped(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    mut turn: Turn,
    reading: Result<(Reading, Requested), StepError>,
    spend: Spend,
) -> Vec<Effect> {
    let said = match reading {
        Ok((
            Reading::Answer { markdown }
            | Reading::Change { markdown, .. }
            | Reading::Unclear { markdown, .. },
            _,
        )) => Ok(markdown),
        Err(StepError::SchemaMismatch { raw, .. }) => Ok(raw.unwrap_or_default()),
        Err(err) => Err(err),
    };
    let err = match said {
        Ok(markdown) => {
            let said = Reading::Answer { markdown };
            return finish(ctx, conversation, turn, said, Requested::default(), spend);
        }
        Err(err) => err,
    };
    let id = turn.id.clone();
    direct::cancelled(ctx, conversation, &mut turn);
    direct::stopped(ctx, conversation, &id);
    let stopped = matches!(
        &err,
        StepError::Session(end) if end.reason == argonauts_core::events::session::EndReason::Stopped
    );
    let ended = if stopped {
        ConversationEvent::TurnCancelled { turn: id.clone() }
    } else {
        let error = format!("{} couldn't answer: {}", turn.worker, turn::describe(&err));
        ConversationEvent::TurnFailed {
            turn: id.clone(),
            error,
        }
    };
    emit(ctx, conversation, ended);
    settle_mode(ctx, &id);
    send_queued(conversation)
}

fn finish(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
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
        TurnStep::Estimate {
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
    let plan_first = turn.plan_first || conversation.prefs.plan_first;
    // `/captain` or `/crew` chose the mode, so whatever the estimate made of the request, the
    // command's work starts, with the words it read.
    let command = match &turn.step {
        TurnStep::Estimate { command, .. } => command.clone(),
        _ => None,
    };
    let reading = match &command {
        Some(command) => commanded(ctx, reading, command, &requested),
        None => reading,
    };
    // A Captain that works in your checkout: what your words change about its turn, and whether
    // plan first has it wait for your go-ahead.
    let mut go_ahead = None;
    let mut ask_first = false;
    let mut work = |estimate: Option<ChangeEstimate>, captain_or_crew: &CaptainOrCrewDecision| {
        let mut request = run_request(
            ctx,
            turn.text.clone(),
            captain_or_crew.mode,
            estimate,
            Some(turn.id.clone()),
        );
        request.forced = command.is_some();
        request.captain_or_crew = Some(captain_or_crew.clone());
        requested.applied.apply_to(&mut request);
        if let Some(style) = command.as_ref().and_then(|command| command.style) {
            request = styled(request, style);
        }
        if plan_first {
            request = planned(request);
        }
        // `/plan` is a crew command, so only plan first turned on reaches direct Captain, where
        // the estimate's reply is the plan to approve.
        if may_start && !turn.plan_first && direct::picks_direct(captain_or_crew) {
            let main = &ctx.settings().crew.main.agent;
            go_ahead = Some(requested.applied.for_direct(main));
            ask_first = plan_first;
        } else if may_start {
            effects.push(handover::start(request));
        }
        go_ahead.is_some()
    };
    let reply = match reading {
        // A command's request that the estimate took for a question still starts its work.
        Reading::Answer { markdown } => {
            if let Some(command) = &command {
                work(None, &commanded_decision(ctx, command, &requested, None));
            }
            TurnReply::Answer { markdown }
        }
        Reading::Change {
            markdown,
            estimate,
            captain_or_crew,
        } => {
            let direct = work(Some(estimate.clone()), &captain_or_crew);
            // A run's start line has the instructions from its start; direct Captain has no run.
            let instructions = if direct {
                requested.applied.instructions.clone()
            } else {
                Vec::new()
            };
            TurnReply::Change {
                markdown,
                estimate,
                captain_or_crew,
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
        conversation
            .history
            .push(Exchange::new(turn.text.clone(), &reply));
    }
    emit(
        ctx,
        conversation,
        ConversationEvent::TurnFinished {
            turn: turn.id.clone(),
            reply: reply.clone(),
            spend,
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        },
    );
    match offer {
        Some((estimate, would_pick)) => {
            let started = offer_to_start(ctx, conversation, &turn, estimate, would_pick, requested);
            effects.extend(started);
        }
        None => keep::offer(ctx, conversation, &requested.applied),
    }
    effects.extend(side::after(ctx, conversation, &turn.step, &reply));
    // A direct turn's answer already says what changed, so no recap repeats it.
    direct::finished(ctx, conversation, &mut turn);
    // On autopilot a plan made first is asked about by nobody, and goes ahead below.
    let mut plan_unasked = direct::offer_plan(ctx, conversation, &turn);
    if !turn.step.kept_out() && !turn.step.in_your_checkout() && recap_due(ctx, elapsed) {
        ctx.send(AppEvent::Recap {
            text: recap_from_turn(&turn.text, &reply),
            model: None,
        });
    }
    settle_mode(ctx, &turn.id);
    // A Captain the Captain-or-crew rules picked, run direct: the session that just planned it goes
    // ahead in your checkout.
    match go_ahead {
        Some(words) if ask_first => {
            plan_unasked = direct::offer_go_ahead(ctx, conversation, &turn, words);
        }
        Some(words) => {
            match direct::go_ahead(
                ctx,
                conversation,
                turn.text.clone(),
                direct::GO_AHEAD,
                None,
                words,
            ) {
                Ok(started) => effects.extend(started),
                Err(err) => ctx.error(&err),
            }
        }
        None => {}
    }
    if let Some(words) = plan_unasked {
        let request = turn.text.clone();
        let said = direct::GO_AHEAD_WITH_PLAN;
        match direct::go_ahead(ctx, conversation, request, said, None, words) {
            Ok(started) => effects.extend(started),
            Err(err) => ctx.error(&err),
        }
    }
    // With a goal set and no run to keep it, the conversation keeps the main agent to it.
    effects.extend(goal::after_turn(
        ctx,
        conversation,
        &turn,
        answer_of(&reply),
    ));
    effects.extend(send_queued(conversation));
    effects
}

/// The agent's own words in a reply.
fn answer_of(reply: &TurnReply) -> &str {
    match reply {
        TurnReply::Answer { markdown }
        | TurnReply::Change { markdown, .. }
        | TurnReply::Unclear { markdown, .. } => markdown,
    }
}

/// A reading for `/captain` or `/crew`: the command's mode, with where Captain works as your words
/// and `captain.kind` say. An unclear request with an estimate is a change now, and one without
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
            let captain_or_crew = commanded_decision(ctx, command, requested, Some(&estimate));
            Reading::Change {
                markdown,
                estimate,
                captain_or_crew,
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

fn commanded_decision(
    ctx: &Ctx,
    command: &Command,
    requested: &Requested,
    estimate: Option<&ChangeEstimate>,
) -> CaptainOrCrewDecision {
    let asked = Asked {
        mode: Some(command.mode),
        captain: requested.applied.asked.captain,
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
    argonauts_crew::captain_or_crew::decide_with(
        estimate.unwrap_or(&none),
        ctx.settings(),
        None,
        asked,
    )
}

/// Agent trouble hands the turn to the next fallback, or with the mode `ask` to the one you
/// pick; anything else ends it. On autopilot nobody is asked: the first agent that may take it
/// by itself answers, and with only paid APIs left the turn ends there.
fn fail(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    mut turn: Turn,
    err: &StepError,
) -> anyhow::Result<Vec<Effect>> {
    if let Some(why) = turn::stopped_by_watchdog(&ctx.names.names(), &turn.worker, err) {
        return Ok(end_by_watchdog(ctx, conversation, turn, why));
    }
    if turn.step.writes() {
        return fail_direct(ctx, conversation, turn, err);
    }
    let mut tried = turn.tried.clone();
    tried.push(turn.worker.clone());
    let fallback_left = turn::candidates(ctx)
        .iter()
        .any(|worker| !tried.contains(worker) && ctx.services.workers.get(worker).is_ok());
    let worth = turn::worth_a_fallback(err) && fallback_left && !turn.step.main_agent_only();
    let asks = turn::fallback_mode(ctx.settings(), &turn.worker) == FallbackMode::Ask;
    let signed_out = turn::signed_out(err, &turn.worker.agent);
    // A CLI that turned out signed out falls back with one line that says so, as a direct
    // turn does, rather than failing the turn and asking again from the start.
    if let Some(out) = &signed_out
        && worth
        && !asks
        && !matches!(turn.step, TurnStep::Review { .. } | TurnStep::Carry { .. })
        && let Ok((to, _)) = turn::pick(ctx, &tried)
    {
        let why = why_signed_out(out);
        turn.signed_out = signed_out.clone();
        return fallback(ctx, conversation, turn, tried, why, to);
    }
    let error = match &signed_out {
        Some(out) => turn::signed_out_words(&ctx.names.cli(&turn.worker.agent), out),
        None => format!("{} couldn't answer: {}", turn.worker, turn::describe(err)),
    };
    emit(
        ctx,
        conversation,
        ConversationEvent::TurnFailed {
            turn: turn.id.clone(),
            error,
        },
    );
    if worth {
        if !asks {
            return read_again(ctx, conversation, turn, tried);
        }
        let candidates = direct::shortlist(ctx, &turn.worker, &tried);
        let autopilot = direct::approval(conversation) == ApprovalMode::Autopilot;
        let auto = candidates
            .iter()
            .find(|candidate| auto_check(candidate).is_ok())
            .filter(|_| autopilot);
        if let Some(next) = auto {
            let to = next.worker.clone();
            return answer_on(ctx, conversation, turn, tried, &to);
        }
        if autopilot && !candidates.is_empty() {
            ctx.notice(NoteLevel::Info, PAID_ONLY_ON_AUTOPILOT);
        } else if !candidates.is_empty() {
            let id = turn.id.clone();
            let why = signed_out
                .as_ref()
                .map_or_else(|| why_it_stopped(err), why_signed_out);
            direct::ask_fallback(ctx, conversation, turn, tried, why, &candidates);
            settle_mode(ctx, &id);
            return Ok(Vec::new());
        }
    }
    settle_mode(ctx, &turn.id);
    let mut effects = Vec::new();
    // A command's run starts without the words when no agent could read them, as it did before
    // words were read. Captain in your checkout needs the agent that just failed, so it doesn't.
    if let TurnStep::Estimate {
        command: Some(command),
        ..
    } = &turn.step
        && !turn.run_began
        && (command.mode == RunMode::Crew || !direct::captain_is_direct(ctx.settings()))
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
        effects.push(handover::start(request));
    }
    effects.extend(send_queued(conversation));
    Ok(effects)
}

/// Said on autopilot when the only agents left to carry a turn on are billed to an API key.
pub(super) const PAID_ONLY_ON_AUTOPILOT: &str = "Decided on autopilot · only an agent billed to an API key \
                                      could carry this on, which is yours to allow, so it stops \
                                      here.";

/// The next agent that hasn't tried reads the turn from its start.
fn read_again(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: Turn,
    tried: Vec<ModelRef>,
) -> anyhow::Result<Vec<Effect>> {
    let record = Record::now(ctx);
    let step = match turn.step {
        TurnStep::Estimate {
            read_instructions,
            command,
            ..
        } => TurnStep::Estimate {
            uncommitted_note: record.uncommitted_note(),
            read_instructions,
            command,
        },
        other => other,
    };
    let id = turn.id.clone();
    // The next try tells the main agent about them again, ahead of any run since.
    conversation.ran.splice(0..0, turn.ran);
    if let Err(err) = try_turn(
        ctx,
        conversation,
        turn.id,
        turn.text,
        step,
        record,
        tried,
        None,
    ) {
        settle_mode(ctx, &id);
        // No turn is in flight now, so what was typed meanwhile goes out at once.
        ctx.error(&err);
        return Ok(send_queued(conversation));
    }
    if let Some(retry) = conversation.turn.as_mut() {
        retry.run_began = turn.run_began;
    }
    Ok(Vec::new())
}

/// The agent you picked reads the turn from its start. Everyone else counts as tried, so it
/// is the one that reads it, and the turn ends if it can't either.
pub(super) fn answer_on(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: Turn,
    mut tried: Vec<ModelRef>,
    to: &ModelRef,
) -> anyhow::Result<Vec<Effect>> {
    for worker in turn::candidates(ctx) {
        if worker != *to && !tried.contains(&worker) {
            tried.push(worker);
        }
    }
    read_again(ctx, conversation, turn, tried)
}

/// Why a signed-out agent stopped, read after its name: "is signed out (its sign-in was
/// revoked)".
fn why_signed_out(out: &SignedOut) -> String {
    if out.why.is_empty() {
        "is signed out".to_owned()
    } else {
        format!("is signed out ({})", out.why)
    }
}

/// Why an agent stopped, read after its name: "hit its usage limit", "stopped: <reason>".
fn why_it_stopped(err: &StepError) -> String {
    let described = turn::describe(err);
    described
        .strip_prefix("it ")
        .map_or_else(|| format!("stopped: {described}"), str::to_owned)
}

/// A direct turn whose agent ran out goes on in the same folder, as `fallback.mode` says and from
/// the shortlist a run's task would get: in auto mode the first candidate auto may take, and
/// with `ask`, or when only paid APIs are left, the fallback question. Anything else ends it, with
/// a line saying what it changed. On autopilot the question is never asked: the first candidate
/// auto may take carries on, and with only paid APIs left the turn ends there.
fn fail_direct(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    mut turn: Turn,
    err: &StepError,
) -> anyhow::Result<Vec<Effect>> {
    let mut tried = turn.tried.clone();
    tried.push(turn.worker.clone());
    let mode = turn::fallback_mode(ctx.settings(), &turn.worker);
    let signed_out = turn::signed_out(err, &turn.worker.agent);
    // Another CLI wouldn't know this one's own command.
    if turn::worth_a_fallback(err) && mode != FallbackMode::Wait && !turn.step.main_agent_only() {
        let why = signed_out
            .as_ref()
            .map_or_else(|| why_it_stopped(err), why_signed_out);
        // Whoever carries it on says the CLI is signed out, now or once you pick them.
        turn.signed_out = signed_out.clone();
        let candidates = direct::shortlist(ctx, &turn.worker, &tried);
        let autopilot = direct::approval(conversation) == ApprovalMode::Autopilot;
        let auto = candidates
            .iter()
            .find(|candidate| auto_check(candidate).is_ok())
            .filter(|_| mode == FallbackMode::Auto || autopilot);
        if let Some(next) = auto {
            let to = next.worker.clone();
            return fallback(ctx, conversation, turn, tried, why, to);
        }
        if autopilot && !candidates.is_empty() {
            ctx.notice(NoteLevel::Info, PAID_ONLY_ON_AUTOPILOT);
        } else if !candidates.is_empty() {
            let id = turn.id.clone();
            direct::ask_fallback(ctx, conversation, turn, tried, why, &candidates);
            settle_mode(ctx, &id);
            return Ok(Vec::new());
        }
    }
    let error = match &signed_out {
        Some(out) => turn::signed_out_words(&ctx.names.cli(&turn.worker.agent), out),
        None => format!("{} couldn't finish: {}", turn.worker, turn::describe(err)),
    };
    Ok(end_direct(ctx, conversation, turn, error))
}

/// The next agent carries the turn on in the same folder, told what it changed so far.
pub(super) fn fallback(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    mut turn: Turn,
    tried: Vec<ModelRef>,
    why: String,
    to: ModelRef,
) -> anyhow::Result<Vec<Effect>> {
    let (told, note) = direct::fallback(ctx, &turn);
    let fallback = Fallback {
        from: turn.worker.clone(),
        to,
        why,
        told,
        note,
        signed_out: turn.signed_out.take(),
    };
    let running = turn.direct.take();
    conversation.ran.splice(0..0, std::mem::take(&mut turn.ran));
    let record = Record::now(ctx);
    let tried = try_turn(
        ctx,
        conversation,
        turn.id.clone(),
        turn.text.clone(),
        turn.step.clone(),
        record,
        tried,
        Some(fallback),
    );
    match tried {
        Ok(()) => {
            if let Some(retry) = conversation.turn.as_mut() {
                retry.run_began = turn.run_began;
                retry.direct = running;
            }
            Ok(Vec::new())
        }
        Err(err) => {
            turn.direct = running;
            let error = format!("{err:#}");
            Ok(end_direct(ctx, conversation, turn, error))
        }
    }
}

/// A direct turn that ends without its answer: the failure, and one line saying what changed.
pub(super) fn end_direct(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    mut turn: Turn,
    error: String,
) -> Vec<Effect> {
    emit(
        ctx,
        conversation,
        ConversationEvent::TurnFailed {
            turn: turn.id.clone(),
            error,
        },
    );
    direct::failed(ctx, conversation, &mut turn);
    settle_mode(ctx, &turn.id);
    send_queued(conversation)
}

/// A watchdog stopped the turn, and one line under it says what it saw. Nothing falls back,
/// since the next agent would get the same request. A turn in your checkout keeps what it
/// changed, and the line after says what that is and what `/rewind` puts back.
fn end_by_watchdog(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    mut turn: Turn,
    why: String,
) -> Vec<Effect> {
    let id = turn.id.clone();
    let kept = direct::kept_after_watchdog(ctx, conversation, &mut turn);
    let why = match kept.changed {
        Some(_) => format!("{why} Its edits stay in your checkout."),
        None => why,
    };
    emit(
        ctx,
        conversation,
        ConversationEvent::TurnStopped {
            turn: id.clone(),
            why,
        },
    );
    kept.say(ctx);
    settle_mode(ctx, &id);
    send_queued(conversation)
}

/// The CLI's own answer to its command, as it wrote it.
fn read_agent_reply(outcome: &Result<StepOutcome, StepError>) -> Result<Reading, StepError> {
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

fn remember_session(ctx: &Ctx, conversation: &mut ConversationState, session: ConversationSession) {
    if conversation.session.as_ref() == Some(&session) {
        return;
    }
    // A copy never has the id of the session it copies. Taking that id as the conversation's
    // own would have the next turn write to the original.
    let is_the_original = conversation
        .session
        .as_ref()
        .is_some_and(|kept| kept.fork && kept.cli_session == session.cli_session);
    if is_the_original {
        return;
    }
    // A conversation that carries on an agent's session keeps that agent's session, whoever
    // took a turn when it ran out.
    if conversation
        .place
        .as_ref()
        .is_some_and(|place| place.worker != session.worker)
    {
        return;
    }
    if let Err(err) = conversation.store.save_session(&session) {
        ctx.notice(
            NoteLevel::Warn,
            format!("The next turn can't resume this session: {err:#}"),
        );
    }
    conversation.session = Some(session);
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
