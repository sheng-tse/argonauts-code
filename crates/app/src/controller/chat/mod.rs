//! The chat with the default agent: a new request's triage estimate, `/ask`, `/solo` and
//! `/crew`, and the `StartWork` choice. Every turn runs read-only in the repo root and resumes
//! the chat's vendor session.
//!
//! Text typed while a turn runs waits in a queue and is sent once the turn ends. `lifecycle`
//! runs a turn from start to finish, `offer` asks whether to start work, and `excerpt` primes a
//! session that can't resume the chat. `/clear` starts a new conversation, and `past` reads
//! earlier ones back so `/resume` can carry them on. `vendor` does the same for the main agent
//! CLI's own sessions.
//!
//! The commands every agent CLI has in some form work on the conversation whatever CLI the
//! main agent is: `side` asks and names off the record, compacts, recaps, and reviews; `copies`
//! branches, forks, exports, and copies; `prefs` holds `/fast`, `/output-style`, `/plan`, and
//! `/goal`.

mod copies;
mod crew;
mod excerpt;
mod lifecycle;
#[cfg(test)]
mod mention_tests;
mod offer;
mod past;
mod prefs;
#[cfg(test)]
mod resume_tests;
#[cfg(test)]
mod shared_tests;
mod side;
#[cfg(test)]
mod tests;
mod vendor;
#[cfg(test)]
mod vendor_tests;

pub(crate) use crew::{rows as crew_rows, vendor_sessions as crew_vendor_sessions};
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use argonauts_core::NoteLevel;
use argonauts_core::api::StartRun;
use argonauts_core::app::{AppEvent, AppMode, ChatEvent, ChatSummary, UiCommand};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::run::RunMode;
use argonauts_core::input::SlashCommand;
use argonauts_core::names::{ChatId, ChoiceId, TurnId, WorkerRef};
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::intake::ChangeEstimate;
use argonauts_core::steps::{StepError, StepOutcome};
use argonauts_core::vendor_commands::VendorCommand;
use argonauts_core::vendor_sessions::{VendorSession, VendorSessionRef};
use argonauts_core::views::ContextRow;
use argonauts_core::worker::WorkerEvent;
use tokio_util::sync::CancellationToken;

pub(crate) use self::copies::{branch, copy, export, fork};
use self::excerpt::Exchange;
use self::lifecycle::{begin, on_done, on_event};
use self::offer::Offer;
pub(crate) use self::offer::{answer_choice, owns_choice, set_aside};
use self::past::PastChat;
use self::prefs::{Goal, Prefs};
pub(crate) use self::prefs::{fast, goal, goal_run_started, output_style, plan};
pub(crate) use self::side::{btw, compact, recap, rename, review};
use super::recap::{mode_name, title_from_request};
use super::shell::ShellRan;
use super::turn::TurnStep;
use super::{Ctx, Effect};
use crate::chat_store::{ChatSession, ChatStore};

pub(crate) struct ChatState {
    id: ChatId,
    store: ChatStore,
    title: String,
    turns: u32,
    /// The turn in flight.
    turn: Option<Turn>,
    /// Cancelled turns whose step hasn't ended yet. Their vendor process may still write the
    /// session, so no turn resumes it meanwhile.
    stopping: Vec<TurnId>,
    /// Typed while a turn ran, oldest first, each with the last `!` command that had ended by
    /// then. Each is sent once the turn ends.
    queued: VecDeque<(Queued, u32)>,
    /// Open `StartWork` questions.
    offers: HashMap<ChoiceId, Offer>,
    /// `!` commands run since the lead's last turn, oldest first. Your next message takes
    /// them: to the lead, or to the run going.
    ran: Vec<ShellRan>,
    /// The id of the last `!` command that ended.
    last_shell: u32,
    /// While a message typed during a turn goes out, the last command that had ended when you
    /// typed it. Commands that ended later wait for your next message.
    shell_cutoff: Option<u32>,
    /// Each request and its answer, for priming a fresh session.
    history: Vec<Exchange>,
    session: Option<ChatSession>,
    /// The model of a CLI session this chat carries on, kept while the chat resumes it unless
    /// you set one.
    carried_model: Option<String>,
    context: Option<ContextRow>,
    /// Said once, so a broken journal doesn't flood the transcript.
    journal_warned: bool,
    /// Side questions asked so far, which number their turns apart from the conversation's.
    sides: u32,
    /// Choices for this app session, kept across `/clear` and `/resume`.
    prefs: Prefs,
    goal: Option<Goal>,
}

struct Turn {
    id: TurnId,
    step: TurnStep,
    text: String,
    worker: WorkerRef,
    /// Workers that already failed this turn, so a fallback never repeats one.
    tried: Vec<WorkerRef>,
    cancel: CancellationToken,
    started: Instant,
    /// A run started or attached while this turn ran, so its reply starts nothing more.
    run_began: bool,
    /// `/plan <task>`: a run it starts plans first and waits for you.
    plan_first: bool,
    /// The `!` commands this turn took, back for a fallback's try.
    ran: Vec<ShellRan>,
}

#[derive(Debug, Clone, PartialEq)]
enum Queued {
    Request(String),
    Ask(String),
    /// One of the main agent CLI's own commands.
    Vendor {
        name: String,
        args: String,
    },
    Side(String),
    Plan(String),
}

/// From a turn's task to the loop.
#[derive(Debug)]
pub(crate) enum TurnUpdate {
    /// An event of the turn's session, as it happens.
    Event { turn: TurnId, event: WorkerEvent },
    /// The step ended: its output, or why there is none.
    Done {
        turn: TurnId,
        outcome: Result<StepOutcome, StepError>,
    },
}

impl ChatState {
    pub(crate) fn new(ctx: &Ctx) -> anyhow::Result<Self> {
        let chat_dir = ctx.env.chat_dir();
        let id = ChatStore::next_id(&chat_dir);
        let store = ChatStore::open(&chat_dir, &id);
        Ok(Self {
            id,
            store,
            title: String::new(),
            turns: 0,
            turn: None,
            stopping: Vec::new(),
            queued: VecDeque::new(),
            offers: HashMap::new(),
            ran: Vec::new(),
            last_shell: 0,
            shell_cutoff: None,
            history: Vec::new(),
            session: None,
            carried_model: None,
            context: None,
            journal_warned: false,
            sides: 0,
            prefs: Prefs::default(),
            goal: None,
        })
    }

    /// An earlier conversation, carried on: its turns go on numbering from its last, its
    /// answers prime a session that can't resume, and its vendor session resumes when the
    /// same agent takes the next turn.
    fn resumed(ctx: &Ctx, past: &PastChat) -> Self {
        let id = past.summary.chat.clone();
        let store = ChatStore::open(&ctx.env.chat_dir(), &id);
        let session = store.session();
        let history = past
            .since_compacted()
            .iter()
            .filter_map(|turn| Some(Exchange::new(turn.request.clone(), turn.reply.as_ref()?)))
            .collect();
        Self {
            id,
            store,
            title: past.summary.title.clone(),
            turns: past.last_turn_number(),
            turn: None,
            stopping: Vec::new(),
            queued: VecDeque::new(),
            offers: HashMap::new(),
            ran: Vec::new(),
            last_shell: 0,
            shell_cutoff: None,
            history,
            session,
            carried_model: None,
            context: None,
            journal_warned: false,
            sides: 0,
            prefs: Prefs::default(),
            goal: None,
        }
    }

    /// A new conversation that carries on a CLI's own session: its next turn resumes that
    /// session, and its turns so far prime a fresh one if another agent takes over.
    fn carrying(ctx: &Ctx, picked: &vendor::Picked, worker: WorkerRef) -> anyhow::Result<Self> {
        let mut chat = Self::new(ctx)?;
        let session = ChatSession {
            worker,
            vendor_session: picked.launch.session.clone(),
        };
        // Saved now, so `/resume chat-<n>` finds the session after a restart too.
        chat.store.save_session(&session)?;
        chat.session = Some(session);
        chat.title = picked.session.title.clone();
        chat.history = picked.history();
        chat.carried_model = picked.launch.model.clone();
        Ok(chat)
    }

    /// The conversation after this one keeps this app session's choices and open questions.
    fn succeed(&mut self, mut next: ChatState) {
        next.offers = std::mem::take(&mut self.offers);
        next.prefs = self.prefs.clone();
        next.last_shell = self.last_shell;
        *self = next;
    }

    /// Typed while a turn runs, to go out once it ends. A message typed again as it goes out
    /// keeps the commands it was typed after.
    fn queue(&mut self, queued: Queued) {
        let after = self.shell_cutoff.unwrap_or(self.last_shell);
        self.queued.push_back((queued, after));
    }
}

/// The main agent CLI's own sessions for this repo, newest first, for `/resume`.
pub(crate) async fn vendor_sessions(ctx: &Ctx) -> Vec<VendorSession> {
    vendor::list(ctx).await
}

/// Earlier conversations in this repo, newest first, for `/resume`.
pub(crate) fn list(ctx: &Ctx) -> Vec<ChatSummary> {
    past::list(&ctx.env.chat_dir())
}

/// `/clear`: a new conversation with the lead. The old one stays in `/resume`, open `StartWork`
/// questions stay answerable, and runs keep going.
pub(crate) fn clear(ctx: &mut Ctx, chat: &mut ChatState) -> anyhow::Result<Vec<Effect>> {
    if !settled(chat) {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /clear starts a new conversation.",
        );
        return Ok(Vec::new());
    }
    let old = chat.id.clone();
    let had_turns = chat.turns > 0;
    chat.succeed(ChatState::new(ctx)?);
    let text = if had_turns {
        format!("Started a new conversation. /resume {old} picks the last one up again.")
    } else {
        "Started a new conversation.".to_owned()
    };
    ctx.notice(NoteLevel::Info, text);
    Ok(vec![Effect::Status])
}

/// `/resume chat-<n>`, or Enter on a conversation in the picker: reprints it condensed and
/// carries it on. The next turn resumes its vendor session when the same agent takes it and
/// can resume, or starts a fresh one primed with the conversation so far.
pub(crate) fn resume(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: &ChatId,
) -> anyhow::Result<Vec<Effect>> {
    let id = past::normalize(id);
    if id == chat.id && chat.turns > 0 {
        ctx.notice(
            NoteLevel::Info,
            format!("{id} is the conversation on screen."),
        );
        return Ok(Vec::new());
    }
    if !settled(chat) {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /resume carries the other one on.",
        );
        return Ok(Vec::new());
    }
    let past = past::load(&ctx.env.chat_dir(), &id).filter(|past| !past.turns.is_empty());
    let Some(past) = past else {
        anyhow::bail!("There's no conversation {id} in this repo. /resume lists them.");
    };
    chat.succeed(ChatState::resumed(ctx, &past));
    let mut replay = past.replay();
    let shown = replay.split_off(replay.len().saturating_sub(past::SHOWN_TURNS));
    ctx.send(AppEvent::ChatResumed {
        chat: id,
        title: past.summary.title.clone(),
        worker: past.summary.worker.clone(),
        earlier: replay,
        turns: shown,
    });
    note_run_on_screen(ctx);
    Ok(vec![Effect::Status])
}

/// After a conversation is carried on while a run is on screen, where plain text goes.
fn note_run_on_screen(ctx: &Ctx) {
    if let AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Parked { run, .. } =
        &ctx.mode
    {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "{run} is on screen, so plain text guides it. /ask talks to this conversation."
            ),
        );
    }
}

/// `/resume claude-code:<id>`, or Enter on a CLI's session in the picker: reprints its last
/// turns condensed, as a resumed conversation does, and carries it on in a new conversation
/// whose next turn goes through the CLI's own resume.
pub(crate) async fn resume_vendor(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    reference: &VendorSessionRef,
) -> anyhow::Result<Vec<Effect>> {
    if !settled(chat) {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /resume carries the other one on.",
        );
        return Ok(Vec::new());
    }
    let picked = vendor::pick(ctx, reference).await?;
    let on_screen = chat
        .session
        .as_ref()
        .is_some_and(|session| session.vendor_session == picked.launch.session);
    let short = vendor::short_id(&picked.session.id).to_owned();
    if on_screen {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "{} session {short} is the conversation on screen.",
                picked.name
            ),
        );
        return Ok(Vec::new());
    }
    let default = ctx.settings().default_agent.clone();
    let worker = picked.worker(&default);
    chat.succeed(ChatState::carrying(ctx, &picked, worker.clone())?);
    let mut replay = picked.replay(&worker);
    let shown = replay.split_off(replay.len().saturating_sub(past::SHOWN_TURNS));
    ctx.send(AppEvent::ChatResumed {
        chat: chat.id.clone(),
        title: chat.title.clone(),
        worker: Some(worker.clone()),
        earlier: replay,
        turns: shown,
    });
    let how = if worker == default {
        format!(
            "Your next message goes to it through {}'s own resume.",
            picked.binary
        )
    } else {
        format!(
            "{} isn't the main agent, so {default} takes the next message, primed with this \
             session's last turns.",
            worker.worker
        )
    };
    ctx.notice(
        NoteLevel::Info,
        format!("Carrying on {} session {short}. {how}", picked.name),
    );
    note_run_on_screen(ctx);
    Ok(vec![Effect::Status])
}

/// `argonauts -c`: the newest conversation in this repo, or a new one when there's none.
pub(crate) fn continue_newest(ctx: &mut Ctx, chat: &mut ChatState) -> anyhow::Result<Vec<Effect>> {
    match list(ctx).first() {
        Some(newest) => {
            let id = newest.chat.clone();
            resume(ctx, chat, &id)
        }
        None => {
            ctx.notice(
                NoteLevel::Info,
                "No earlier conversation in this repo, so this one starts new.",
            );
            Ok(Vec::new())
        }
    }
}

pub(crate) fn title(chat: &ChatState) -> &str {
    &chat.title
}

/// No step of this chat is still running.
pub(crate) fn settled(chat: &ChatState) -> bool {
    chat.turn.is_none() && chat.stopping.is_empty()
}

/// A run started or attached: the turn in flight answers, but starts no run and offers none.
pub(crate) fn run_attached(chat: &mut ChatState) {
    if let Some(turn) = chat.turn.as_mut() {
        turn.run_began = true;
    }
}

/// A `!` command ended. The lead hears of it with your next message, never by itself.
pub(crate) fn shell_ran(chat: &mut ChatState, ran: ShellRan) {
    chat.last_shell = ran.id;
    super::shell::hold(&mut chat.ran, ran);
}

/// The commands the message going out now hears of. One typed while a turn ran hears only of
/// those that had ended when you typed it.
pub(crate) fn take_shell(chat: &mut ChatState) -> Vec<ShellRan> {
    match chat.shell_cutoff {
        None => std::mem::take(&mut chat.ran),
        Some(cutoff) => {
            let (heard, later) = chat.ran.drain(..).partition(|ran| ran.id <= cutoff);
            chat.ran = later;
            heard
        }
    }
}

/// Commands a message took but couldn't deliver wait for the next one.
pub(crate) fn give_back_shell(chat: &mut ChatState, ran: Vec<ShellRan>) {
    chat.ran.splice(0..0, ran);
}

/// A message typed during a turn goes out now, or has gone: `typed_after` is the last
/// command that had ended when you typed it.
pub(crate) fn sending_queued(chat: &mut ChatState, typed_after: Option<u32>) {
    chat.shell_cutoff = typed_after;
}

/// The commands you ran before a run started were for the conversation, not the run.
pub(crate) fn run_started(chat: &mut ChatState) {
    chat.ran.clear();
}

/// The chat session's row for `/context`.
pub(crate) fn context_row(chat: &ChatState) -> Option<ContextRow> {
    chat.context.clone()
}

/// Records the request and runs `TriageEstimate` in the chat session.
pub(crate) async fn new_request(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    request(ctx, chat, text)
}

/// Plain text while a turn runs: sent as the next turn.
pub(crate) fn queue_turn(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    request(ctx, chat, text)
}

/// A turn at a time: text that arrives while one runs waits for it to end.
fn request(ctx: &mut Ctx, chat: &mut ChatState, text: String) -> anyhow::Result<Vec<Effect>> {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if chat.turn.is_some() {
        chat.queue(Queued::Request(text));
        return Ok(Vec::new());
    }
    let effects = name_the_chat(chat, &text);
    begin(ctx, chat, text, |record| TurnStep::Triage {
        uncommitted_note: record.uncommitted_note(),
    })?;
    Ok(effects)
}

/// A request whose run plans first and waits for you; `/plan <task>`.
fn request_planned(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    if chat.turn.is_some() {
        chat.queue(Queued::Plan(text));
        return Ok(Vec::new());
    }
    let effects = request(ctx, chat, text)?;
    if let Some(turn) = chat.turn.as_mut() {
        turn.plan_first = true;
    }
    Ok(effects)
}

/// `/ask`: `ChatAnswer`, never a run.
pub(crate) async fn ask(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    question: String,
) -> anyhow::Result<Vec<Effect>> {
    let question = question.trim().to_owned();
    if question.is_empty() {
        ctx.notice(NoteLevel::Info, "Put the question after /ask.");
        return Ok(Vec::new());
    }
    if chat.turn.is_some() {
        chat.queue(Queued::Ask(question));
        return Ok(Vec::new());
    }
    let run_status = run_status(ctx).await;
    let effects = name_the_chat(chat, &question);
    begin(ctx, chat, question, |_| TurnStep::Ask { run_status })?;
    Ok(effects)
}

/// One of the main agent CLI's own commands, sent to its session as `prompt`, the command
/// itself. The chat shows it as you typed it, and the reply as an answer.
pub(crate) fn vendor_command(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    command: &VendorCommand,
    args: &str,
    prompt: String,
) -> anyhow::Result<Vec<Effect>> {
    if chat.turn.is_some() {
        chat.queue(Queued::Vendor {
            name: command.name.clone(),
            args: args.trim().to_owned(),
        });
        return Ok(Vec::new());
    }
    let shown = command.invocation(args);
    let effects = name_the_chat(chat, &shown);
    begin(ctx, chat, shown, |_| TurnStep::Vendor { prompt })?;
    Ok(effects)
}

/// `/solo` and `/crew`: a run with no estimate.
pub(crate) async fn start_forced(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    mode: RunMode,
    task: String,
) -> anyhow::Result<Vec<Effect>> {
    let task = task.trim().to_owned();
    if task.is_empty() {
        let command = mode_name(mode);
        ctx.notice(
            NoteLevel::Info,
            format!("Say what to do after /{command}, and a {command} run starts on it."),
        );
        return Ok(Vec::new());
    }
    let mut effects = name_the_chat(chat, &task);
    let mut request = run_request(ctx, task, mode, None, None);
    if chat.prefs.plan_first {
        request = planned(request);
    }
    effects.push(Effect::StartRun(Box::new(request)));
    Ok(effects)
}

/// Esc during a turn: cancels the step, which asks the vendor process to stop. The turn stays
/// stopping until its step ends. Anything queued behind it goes back into the input box.
pub(crate) fn cancel_turn(ctx: &mut Ctx, chat: &mut ChatState) -> anyhow::Result<Vec<Effect>> {
    let Some(id) = stop_turn(ctx, chat) else {
        return Ok(Vec::new());
    };
    settle_mode(ctx, &id);
    let texts: Vec<String> = chat
        .queued
        .drain(..)
        .map(|(queued, _)| match queued {
            Queued::Request(text) => text,
            Queued::Ask(question) => format!("/ask {question}"),
            Queued::Vendor { name, args } if args.is_empty() => format!("/{name}"),
            Queued::Vendor { name, args } => format!("/{name} {args}"),
            Queued::Side(question) => format!("/btw {question}"),
            Queued::Plan(task) => format!("/plan {task}"),
        })
        .collect();
    if !texts.is_empty() {
        ctx.send(AppEvent::TakenBack { texts });
    }
    Ok(Vec::new())
}

pub(crate) async fn on_update(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    update: TurnUpdate,
) -> anyhow::Result<Vec<Effect>> {
    match update {
        TurnUpdate::Event { turn, event } => {
            on_event(ctx, chat, &turn, event);
            Ok(Vec::new())
        }
        TurnUpdate::Done { turn, outcome } => on_done(ctx, chat, &turn, outcome),
    }
}

/// A new default agent: another vendor can't resume the old session, so the next turn starts
/// a fresh one primed with an excerpt of the chat so far.
pub(crate) fn fresh_session(ctx: &mut Ctx, chat: &mut ChatState) -> anyhow::Result<Vec<Effect>> {
    let _ = ctx;
    chat.session = None;
    chat.carried_model = None;
    chat.store.forget_session()?;
    Ok(Vec::new())
}

/// The screen is closing: a turn in flight is cancelled. The loop then waits a little for
/// every stopping step to end.
pub(crate) fn close(ctx: &mut Ctx, chat: &mut ChatState) {
    stop_turn(ctx, chat);
}

fn stop_turn(ctx: &mut Ctx, chat: &mut ChatState) -> Option<TurnId> {
    let turn = chat.turn.take()?;
    turn.cancel.cancel();
    chat.stopping.push(turn.id.clone());
    emit(
        ctx,
        chat,
        ChatEvent::TurnCancelled {
            turn: turn.id.clone(),
        },
    );
    Some(turn.id)
}

/// The first request names the chat, and the status line shows it.
fn name_the_chat(chat: &mut ChatState, text: &str) -> Vec<Effect> {
    if !chat.title.is_empty() {
        return Vec::new();
    }
    chat.title = title_from_request(text);
    vec![Effect::Status]
}

/// A run you picked, forced like `/solo` and `/crew`.
fn run_request(
    ctx: &Ctx,
    request: String,
    mode: RunMode,
    estimate: Option<ChangeEstimate>,
    chat_turn: Option<TurnId>,
) -> StartRun {
    let settings = ctx.settings();
    StartRun {
        request,
        mode,
        forced: true,
        estimate,
        triage: None,
        chat_turn,
        continues: None,
        overrides: ctx.settings.run_overrides(),
        include_uncommitted: settings.intake.include_uncommitted,
        default_agent: Some(settings.default_agent.clone()),
        approval_mode: None,
        front_end: None,
    }
}

/// The same run, planned first: the crew plans, and the plan waits for you whatever the
/// approvals say. A solo run has no plan, so it becomes a crew run.
fn planned(mut request: StartRun) -> StartRun {
    request.mode = RunMode::Crew;
    request
        .overrides
        .push(format!("{PLAN_MODE_KEY}=\"supervised\""));
    request
}

/// The plan checkpoint's mode, which `/plan` sets for one run.
const PLAN_MODE_KEY: &str = "approvals.plan.mode";

/// The same run as a workflow or a goal, whatever `workflows.style` says: the run's own command
/// wins over every setting, and the run keeps the style it starts with.
pub(super) fn styled(mut request: StartRun, style: RunStyle) -> StartRun {
    request.mode = RunMode::Crew;
    request
        .overrides
        .push(format!("{STYLE_KEY}=\"{}\"", style.as_str()));
    request
}

const STYLE_KEY: &str = "workflows.style";

/// `/workflow <task>`: a crew run that's a workflow.
pub(crate) async fn start_workflow(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    task: String,
) -> anyhow::Result<Vec<Effect>> {
    let task = task.trim().to_owned();
    let mut effects = name_the_chat(chat, &task);
    let mut request = styled(
        run_request(ctx, task, RunMode::Crew, None, None),
        RunStyle::Workflow,
    );
    if chat.prefs.plan_first {
        request = planned(request);
    }
    effects.push(Effect::StartRun(Box::new(request)));
    Ok(effects)
}

/// The run on screen's zero-token summary, for `/ask` during a run.
async fn run_status(ctx: &Ctx) -> Option<String> {
    let run = match &ctx.mode {
        AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Parked { run, .. } => run,
        AppMode::Idle | AppMode::Answering { .. } => return None,
    };
    match ctx.services.conductor.status(run).await {
        Ok(summary) => Some(argonauts_crew::status::render(&summary)),
        Err(err) => {
            ctx.notice(
                NoteLevel::Warn,
                format!("Asking without the run's status: {err}"),
            );
            None
        }
    }
}

/// Back to `Idle`, unless something else changed the mode meanwhile, such as a run starting.
fn settle_mode(ctx: &mut Ctx, turn: &TurnId) {
    if matches!(&ctx.mode, AppMode::Answering { turn: current } if current == turn) {
        ctx.set_mode(AppMode::Idle);
    }
}

/// What was typed during the turn, handled now as if just sent, so it goes by the mode now.
fn send_queued(chat: &mut ChatState) -> Vec<Effect> {
    chat.queued
        .drain(..)
        .map(|(queued, typed_after)| {
            let command = match queued {
                Queued::Request(text) => UiCommand::Submit { text },
                Queued::Ask(question) => UiCommand::Slash {
                    command: SlashCommand::Ask { question },
                },
                Queued::Vendor { name, args } => UiCommand::Slash {
                    command: SlashCommand::Vendor { name, args },
                },
                Queued::Side(question) => UiCommand::Slash {
                    command: SlashCommand::Btw {
                        question: Some(question),
                    },
                },
                Queued::Plan(task) => UiCommand::Slash {
                    command: SlashCommand::Plan { task: Some(task) },
                },
            };
            Effect::Queued {
                command: Box::new(command),
                typed_after,
            }
        })
        .collect()
}

/// Sends the event and records it, except the streamed text, which the finished turn holds,
/// and side questions, which stay out of the conversation.
fn emit(ctx: &Ctx, chat: &mut ChatState, event: ChatEvent) {
    let streamed = matches!(
        event,
        ChatEvent::Agent {
            event: AgentEvent::TextDelta { .. },
            ..
        }
    );
    if !streamed
        && !side::is_side_turn(event_turn(&event))
        && let Err(err) = chat.store.append(&event)
        && !chat.journal_warned
    {
        chat.journal_warned = true;
        ctx.notice(
            NoteLevel::Warn,
            format!(
                "The chat isn't being recorded in {}: {err:#}",
                ctx.env.display(chat.store.journal_path())
            ),
        );
    }
    ctx.send(AppEvent::Chat { event });
}

fn event_turn(event: &ChatEvent) -> &TurnId {
    match event {
        ChatEvent::TurnStarted { turn, .. }
        | ChatEvent::Agent { turn, .. }
        | ChatEvent::TurnFinished { turn, .. }
        | ChatEvent::TurnFailed { turn, .. }
        | ChatEvent::TurnCancelled { turn } => turn,
    }
}
