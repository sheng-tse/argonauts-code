//! The chat with the default agent: a new request's triage estimate, `/ask`, `/solo` and
//! `/crew`, and the `StartWork` choice. Every turn resumes the chat's vendor session in the
//! repo root, read-only but for a direct turn: solo you choose, which `direct` runs in your
//! checkout as the CLI itself would.
//!
//! Text typed while a turn runs waits in a queue and is sent once the turn ends. `lifecycle`
//! runs a turn from start to finish, `offer` asks whether to start work, and `excerpt` primes a
//! session that can't resume the chat. `/clear` starts a new conversation, and `past` reads
//! earlier ones back so `/resume` can carry them on. `vendor` does the same for the main agent
//! CLI's own sessions, and `agent` for the sessions of a run's agents, which carry on as direct
//! conversations in the folder each ran in.
//!
//! The commands every agent CLI has in some form work on the conversation whatever CLI the
//! main agent is: `side` asks and names off the record, compacts, recaps, and reviews; `copies`
//! branches, forks, exports, and copies; `prefs` holds `/fast`, `/output-style`, `/plan`, and
//! `/goal`.

mod agent;
#[cfg(test)]
mod agent_tests;
mod copies;
mod crew;
mod direct;
#[cfg(test)]
mod direct_tests;
mod excerpt;
mod keep;
mod lifecycle;
mod live;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod mention_tests;
mod mirror;
mod offer;
mod past;
mod prefs;
#[cfg(test)]
mod request_tests;
#[cfg(test)]
mod resume_tests;
mod run_copy;
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
use std::path::Path;
use std::time::Instant;

use anyhow::Context as _;

use argonauts_core::NoteLevel;
use argonauts_core::RunId;
use argonauts_core::agent_sessions::{AgentSession, AgentSessionRef, Resume};
use argonauts_core::api::{RunSummary, StartRun};
use argonauts_core::app::{
    AppEvent, AppMode, ChatEvent, ChatSummary, ReplayTurn, UiCommand, ViewData,
};
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::session::VendorWorkflow;
use argonauts_core::input::SlashCommand;
use argonauts_core::names::{ChatId, ChoiceId, SessionKey, TurnId, WorkerRef, WorkflowId};
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::intake::ChangeEstimate;
use argonauts_core::steps::{StepError, StepOutcome};
use argonauts_core::vendor_commands::VendorCommand;
use argonauts_core::vendor_sessions::{VendorSession, VendorSessionRef};
use argonauts_core::views::ContextRow;
use argonauts_core::worker::WorkerEvent;
use tokio_util::sync::CancellationToken;

pub(crate) use self::agent::SessionCache;
pub(crate) use self::copies::{branch, copy, export, fork};
pub(crate) use self::direct::{rewind, set_approval};
use self::excerpt::Exchange;
use self::lifecycle::{begin, on_done, on_event};
use self::offer::Offer;
use self::past::PastChat;
use self::prefs::{Goal, Prefs};
pub(crate) use self::prefs::{fast, goal, goal_run_started, output_style, plan};
pub(crate) use self::run_copy::tidy_copies;
pub(crate) use self::side::{btw, compact, recap, rename, review};
#[cfg(test)]
pub(crate) use self::tests::checked_solo;
use super::instructions::{self, Command, Roster};
use super::recap::{mode_name, title_from_request};
use super::shell::ShellRan;
use super::turn::{DirectWords, TurnStep};
use super::{Ctx, Effect};
use crate::chat_store::{ChatPlace, ChatSession, ChatStore};
use argonauts_core::events::checkpoint::AnswerAction;

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
    /// Open "Keep this for later requests?" questions, each with what it would keep.
    keeps: HashMap<ChoiceId, keep::Kept>,
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
    /// Set when the chat carries on an agent's session from a run: its turns are direct, with
    /// that agent, in the folder it ran in.
    place: Option<ChatPlace>,
    context: Option<ContextRow>,
    /// Said once, so a broken journal doesn't flood the transcript.
    journal_warned: bool,
    /// Said once: Claude Code's heads-ups stopped reading in this version.
    heads_up_warned: bool,
    /// The workflows the last turn's CLI ran itself, as it reported them.
    workflows: Vec<VendorWorkflow>,
    /// Side questions asked so far, which number their turns apart from the conversation's.
    sides: u32,
    /// Choices for this app session, kept across `/clear` and `/resume`.
    prefs: Prefs,
    goal: Option<Goal>,
    direct: direct::DirectState,
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
    /// A direct turn's listener, snapshot, and what it ran.
    direct: Option<direct::Running>,
    /// What the session takes while it works: your messages, send now, and the screen.
    live: live::Live,
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
    /// `/solo <task>`.
    Solo(String),
    /// `/crew <task>`, whose words are read before the run starts.
    Crew(String),
    /// `/workflow <task>`, read the same way.
    Workflow(String),
    /// `/goal <objective>`, read the same way.
    Goal(String),
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
    /// A direct turn's CLI asks you something, and waits for the answer.
    Asked { turn: TurnId, asked: direct::Asked },
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
            keeps: HashMap::new(),
            ran: Vec::new(),
            last_shell: 0,
            shell_cutoff: None,
            history: Vec::new(),
            session: None,
            carried_model: None,
            place: None,
            context: None,
            journal_warned: false,
            heads_up_warned: false,
            workflows: Vec::new(),
            sides: 0,
            prefs: Prefs::default(),
            goal: None,
            direct: direct::DirectState::default(),
        })
    }

    /// An earlier conversation, carried on: its turns go on numbering from its last, its
    /// answers prime a session that can't resume, and its vendor session resumes when the
    /// same agent takes the next turn.
    fn resumed(ctx: &Ctx, past: &PastChat) -> Self {
        let id = past.summary.chat.clone();
        let store = ChatStore::open(&ctx.env.chat_dir(), &id);
        let session = store.session();
        let place = store.place();
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
            keeps: HashMap::new(),
            ran: Vec::new(),
            last_shell: 0,
            shell_cutoff: None,
            history,
            session,
            carried_model: None,
            place,
            context: None,
            journal_warned: false,
            heads_up_warned: false,
            workflows: Vec::new(),
            sides: 0,
            prefs: Prefs::default(),
            goal: None,
            direct: direct::DirectState::default(),
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

    /// A new conversation that carries on an agent's session from a run, through its CLI's own
    /// resume, with that agent, in `place.folder`.
    fn continuing(ctx: &Ctx, found: &agent::Found, place: ChatPlace) -> anyhow::Result<Self> {
        let mut chat = Self::new(ctx)?;
        let vendor_session = found
            .vendor_session
            .clone()
            .context("the session left no id to resume")?;
        let session = ChatSession {
            worker: place.worker.clone(),
            vendor_session,
        };
        // Saved now, so `/resume chat-<n>` carries it on in the same folder after a restart.
        chat.store.save_session(&session)?;
        chat.store.save_place(&place)?;
        chat.session = Some(session);
        chat.title = found
            .card_title
            .clone()
            .unwrap_or_else(|| format!("{} {}", found.role.as_str(), found.run));
        chat.carried_model = found.worker.model.clone();
        chat.place = Some(place);
        Ok(chat)
    }

    /// The conversation after this one keeps this app session's choices and open questions.
    /// What `/rewind` can put back stays only while both work in the same folder, and a copy
    /// made again for this one goes once the next one works elsewhere.
    fn succeed(&mut self, ctx: &Ctx, mut next: ChatState) {
        next.offers = std::mem::take(&mut self.offers);
        next.prefs = self.prefs.clone();
        next.direct = self.direct.carry();
        let same_folder = next.place.as_ref().map(|place| &place.folder)
            == self.place.as_ref().map(|place| &place.folder);
        if !same_folder {
            next.direct.forget_turns();
        }
        next.last_shell = self.last_shell;
        let left = self
            .place
            .take()
            .filter(|place| place.made_again && !same_folder);
        *self = next;
        if let Some(place) = left {
            run_copy::leave(ctx, &place);
        }
    }

    /// Where its turns work: the folder of the agent's session it carries on, or your checkout.
    pub(super) fn folder<'a>(&'a self, ctx: &'a Ctx) -> &'a Path {
        self.place
            .as_ref()
            .map_or(ctx.env.repo_root.as_path(), |place| place.folder.as_path())
    }

    /// Whether `turn` runs in the conversation's own session, on its CLI, so what that session
    /// reports of itself, such as its command list, is the conversation's. A review, a side
    /// question, or another agent answering in its place reports its own.
    pub(crate) fn speaks_for_the_conversation(&self, ctx: &Ctx, turn: &TurnId) -> bool {
        self.turn
            .as_ref()
            .filter(|running| &running.id == turn)
            .is_some_and(|running| {
                !running.step.own_session() && running.worker.worker == self.cli(ctx)
            })
    }

    /// The CLI its turns go to, such as `claude-code`: the agent's whose session it carries on,
    /// or the main agent's.
    pub(super) fn cli<'a>(&'a self, ctx: &'a Ctx) -> &'a str {
        self.place
            .as_ref()
            .map_or(ctx.settings().default_agent.worker.as_str(), |place| {
                place.worker.worker.as_str()
            })
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

/// The sessions of the newest runs' agents, for `/resume` to list under each run, and which
/// runs those are.
pub(crate) async fn agent_sessions(
    ctx: &Ctx,
    runs: &[RunSummary],
) -> (Vec<AgentSession>, Vec<RunId>) {
    agent::list(ctx, runs).await
}

/// One run's agents' sessions, for a run `/resume` listed without them.
pub(crate) async fn run_agent_sessions(
    ctx: &Ctx,
    run: &RunId,
) -> anyhow::Result<Vec<AgentSession>> {
    agent::list_run(ctx, run).await
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
    chat.succeed(ctx, ChatState::new(ctx)?);
    ctx.send(AppEvent::ChatCleared);
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
/// can resume, or starts a fresh one primed with the conversation so far. One that carries on
/// an agent's session from a run is checked again first, since its run may be going again.
pub(crate) async fn resume(
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
    let mut resumed = ChatState::resumed(ctx, &past);
    // It carries on an agent's session, whose run may be going again since, and whose copy may
    // be gone again.
    let mut made_again = false;
    if let Some(place) = resumed.place.as_mut() {
        if let Some(why) = agent::place_cannot(ctx, place).await {
            anyhow::bail!("{id} can't carry on: {why}.");
        }
        made_again =
            run_copy::ensure_copy(&ctx.env.repo_root, &place.folder, place.branch.as_deref())
                .with_context(|| format!("{id} can't carry on"))?;
        if made_again && !place.made_again {
            place.made_again = true;
            resumed.store.save_place(place)?;
        }
    }
    let where_it_works = resumed
        .place
        .as_ref()
        .map(|place| where_it_works(ctx, place, made_again));
    chat.succeed(ctx, resumed);
    let mut replay = past.replay();
    let shown = replay.split_off(replay.len().saturating_sub(past::SHOWN_TURNS));
    ctx.send(AppEvent::ChatResumed {
        chat: id,
        title: past.summary.title.clone(),
        worker: past.summary.worker.clone(),
        earlier: replay,
        turns: shown,
    });
    if let Some(text) = where_it_works {
        ctx.notice(NoteLevel::Info, text);
    }
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
    chat.succeed(ctx, ChatState::carrying(ctx, &picked, worker.clone())?);
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

/// `/resume run-<n>/s-<n>`, or `c` on a finished agent: carries the agent's session on in a new
/// conversation whose turns are direct, through its CLI's own resume, in the folder it ran in.
/// That's the run's copy, made again from its branch when the run merged and the copy is gone.
/// Only a run that ended lets go of its sessions: a session still running is taken over
/// instead, one whose run may still start it again waits for the run, and one that can't
/// resume shows its transcript.
pub(crate) async fn resume_agent(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    reference: &AgentSessionRef,
) -> anyhow::Result<Vec<Effect>> {
    if !settled(chat) {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /resume carries the other one on.",
        );
        return Ok(Vec::new());
    }
    let (found, resume) = agent::pick(ctx, reference).await?;
    let who = found.who();
    match resume {
        Resume::Ready => {}
        Resume::Working => {
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "{reference} is still working. t in its agent view takes it over, and once \
                     its run ends, its session carries on here."
                ),
            );
            return Ok(Vec::new());
        }
        Resume::RunOwns => {
            ctx.notice(NoteLevel::Info, format!("{}.", agent::owned_by(reference)));
            return Ok(Vec::new());
        }
        Resume::Cannot { why } => {
            ctx.notice(
                NoteLevel::Info,
                format!("{reference} can't carry on, since {why}. Here is its transcript."),
            );
            let raw = ctx
                .services
                .conductor
                .raw(&reference.run, &reference.session)
                .await
                .with_context(|| format!("the transcript of {reference}"))?;
            ctx.send(AppEvent::View {
                data: Box::new(ViewData::Raw { raw }),
            });
            return Ok(Vec::new());
        }
    }
    // A branch of it works in the same place on a session of its own.
    let on_screen = chat
        .place
        .as_ref()
        .is_some_and(|place| &place.agent == reference)
        && chat.session.as_ref().map(|session| &session.vendor_session)
            == found.vendor_session.as_ref();
    if on_screen {
        ctx.notice(
            NoteLevel::Info,
            format!("{reference} is the conversation on screen."),
        );
        return Ok(Vec::new());
    }
    let repo = ctx.env.repo_root.clone();
    let (folder, made_again) = {
        let found = found.clone();
        tokio::task::spawn_blocking(move || agent::place(&repo, &found))
            .await
            .context("making the run's copy again")?
            .with_context(|| format!("{reference} can't carry on"))?
    };
    let place = ChatPlace {
        agent: reference.clone(),
        worker: found.worker.clone(),
        folder,
        branch: found.branch.clone(),
        made_again,
    };
    chat.succeed(ctx, ChatState::continuing(ctx, &found, place.clone())?);
    let worker = place.worker.clone();
    let turns = found
        .last_message
        .iter()
        .map(|text| ReplayTurn {
            from: Party::Agent {
                session: SessionKey::numbered(0),
                worker: worker.clone(),
            },
            text: text.clone(),
        })
        .collect();
    ctx.send(AppEvent::ChatResumed {
        chat: chat.id.clone(),
        title: chat.title.clone(),
        worker: Some(worker.clone()),
        earlier: Vec::new(),
        turns,
    });
    let agent = ctx.names.worker(&worker);
    let binary = agent::binary(ctx, &worker.worker);
    let mut text = format!(
        "Carrying on {who}, {agent}. Your messages go to it through {binary}'s own resume, and \
         it can change files. "
    );
    text.push_str(&where_it_works(ctx, &place, made_again));
    ctx.notice(NoteLevel::Info, text);
    note_run_on_screen(ctx);
    Ok(vec![Effect::Status])
}

/// Which files a carried-on agent works on, said plainly: the run's copy, not your checkout.
fn where_it_works(ctx: &Ctx, place: &ChatPlace, made_again: bool) -> String {
    let folder = ctx.env.display(&place.folder);
    if place.folder == ctx.env.repo_root {
        return "It works in your checkout.".to_owned();
    }
    let on_branch = place
        .branch
        .as_deref()
        .map_or_else(String::new, |branch| format!(" on {branch}"));
    let mut text = format!("It works on the run's copy in {folder}{on_branch}, not your checkout.");
    if made_again {
        text.push_str(" That copy was gone, so it was made again from the branch.");
    }
    text
}

/// `argonauts -c`: the newest conversation in this repo, or a new one when there's none.
pub(crate) async fn continue_newest(
    ctx: &mut Ctx,
    chat: &mut ChatState,
) -> anyhow::Result<Vec<Effect>> {
    match list(ctx).first() {
        Some(newest) => {
            let id = newest.chat.clone();
            resume(ctx, chat, &id).await
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

/// Plain text while a turn runs: into the running session at once where its CLI takes it
/// there, as Claude Code does, and otherwise sent as the next turn.
pub(crate) fn queue_turn(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    let joined = chat.turn.as_mut().is_some_and(|turn| {
        let trimmed = text.trim();
        !turn.step.own_session()
            && !trimmed.is_empty()
            && turn.live.takes_messages()
            && turn.live.send(trimmed.to_owned())
    });
    if joined {
        return Ok(Vec::new());
    }
    request(ctx, chat, text)
}

/// Ctrl+Enter: what you sent the running turn goes to the model now.
pub(crate) fn send_now(chat: &mut ChatState) {
    if let Some(turn) = chat.turn.as_mut() {
        turn.live.send_now();
    }
}

/// ↑ on your waiting messages: the ones not sent yet come back at once, and the session is
/// asked for the ones the model hasn't read, which come back as it lets them go.
pub(crate) fn take_back(ctx: &mut Ctx, chat: &mut ChatState) {
    let texts: Vec<String> = chat
        .queued
        .iter()
        .filter_map(|(queued, _)| match queued {
            Queued::Request(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    chat.queued
        .retain(|(queued, _)| !matches!(queued, Queued::Request(_)));
    if let Some(turn) = chat.turn.as_mut() {
        turn.live.take_back();
    }
    if !texts.is_empty() {
        ctx.send(AppEvent::TakenBack { texts });
    }
}

/// The workflow view of one the conversation's CLI runs itself.
pub(crate) fn workflow_view(ctx: &Ctx, chat: &ChatState, workflow: Option<&WorkflowId>) {
    mirror::show(ctx, chat, workflow);
}

/// A button above the input, pressed. A press for a session that has ended does nothing.
pub(crate) fn press_heads_up(ctx: &Ctx, chat: &mut ChatState, plugin: &str, handle: u64) {
    let pressed = chat
        .turn
        .as_mut()
        .is_some_and(|turn| turn.live.press(plugin, handle));
    if !pressed {
        ctx.notice(
            NoteLevel::Info,
            "Claude Code takes that while it's working on your message.",
        );
    }
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
    // The solo preset, or an agent's session carried on: your message goes to the session as
    // its CLI takes it, with no estimate first. With plan first on, the session plans before it
    // changes anything.
    if direct::preset_is_direct(ctx.settings()) || chat.place.is_some() {
        let mut effects = effects;
        if chat.prefs.plan_first {
            effects.extend(direct::plan(ctx, chat, text, DirectWords::default())?);
        } else {
            effects.extend(direct::begin(ctx, chat, text, None)?);
        }
        return Ok(effects);
    }
    let read_instructions = ctx.settings().triage.read_instructions;
    begin(ctx, chat, text, |record| TurnStep::Triage {
        uncommitted_note: record.uncommitted_note(),
        read_instructions,
        command: None,
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
    // Direct solo, and an agent's session carried on, plan in the session that then goes ahead,
    // and change nothing until you say.
    if direct::preset_is_direct(ctx.settings()) || chat.place.is_some() {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let mut effects = name_the_chat(chat, &text);
        effects.extend(direct::plan(ctx, chat, text, DirectWords::default())?);
        return Ok(effects);
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
    let mut effects = name_the_chat(chat, &shown);
    // The solo preset, or an agent's session carried on: the command works where the
    // conversation does, as in the CLI itself.
    if direct::preset_is_direct(ctx.settings()) || chat.place.is_some() {
        effects.extend(direct::begin_vendor(ctx, chat, shown, prompt)?);
        return Ok(effects);
    }
    begin(ctx, chat, shown, |_| TurnStep::Vendor {
        prompt,
        direct: None,
    })?;
    Ok(effects)
}

pub(crate) fn owns_choice(chat: &ChatState, id: &ChoiceId) -> bool {
    offer::owns_choice(chat, id) || keep::owns(chat, id)
}

/// The `StartWork` choice, or where to keep what a request said "from now on".
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    action: AnswerAction,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if keep::owns(chat, &id) {
        return keep::answer(ctx, chat, id, action);
    }
    offer::answer_choice(ctx, chat, id, action, text).await
}

pub(crate) fn set_aside(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
) -> anyhow::Result<Vec<Effect>> {
    if keep::owns(chat, &id) {
        keep::set_aside(ctx, chat, id);
        return Ok(Vec::new());
    }
    offer::set_aside(ctx, chat, id)
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
    let direct = mode == RunMode::Solo && direct::solo_is_direct(ctx.settings());
    // Your words are read first when they may say how to run, as a request's are: the command
    // keeps its mode, and the rest of what they say is checked and followed. Words that can't
    // say how leave the command as quick as it always was.
    let reads = ctx.settings().triage.read_instructions
        && !(direct && chat.prefs.plan_first)
        && instructions::might_say_how(&task, &Roster::new(ctx));
    if (direct || reads) && chat.turn.is_some() {
        chat.queue(match mode {
            RunMode::Solo => Queued::Solo(task),
            RunMode::Crew => Queued::Crew(task),
        });
        return Ok(Vec::new());
    }
    let mut effects = name_the_chat(chat, &task);
    if reads {
        let command = Command {
            mode,
            said: format!("you typed /{}", mode_name(mode)),
            style: None,
        };
        begin(ctx, chat, task, |record| TurnStep::Triage {
            uncommitted_note: record.uncommitted_note(),
            read_instructions: true,
            command: Some(command),
        })?;
        return Ok(effects);
    }
    // Solo you choose is a direct turn, unless something asks for the checked copy. With plan
    // first on, it plans before it changes anything.
    if direct {
        if chat.prefs.plan_first {
            effects.extend(direct::plan(ctx, chat, task, DirectWords::default())?);
        } else {
            effects.extend(direct::begin(ctx, chat, task, None)?);
        }
        return Ok(effects);
    }
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
            Queued::Solo(task) => format!("/solo {task}"),
            Queued::Crew(task) => format!("/crew {task}"),
            Queued::Workflow(task) => format!("/workflow {task}"),
            Queued::Goal(objective) => format!("/goal {objective}"),
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
        TurnUpdate::Asked { turn, asked } => direct::on_asked(ctx, chat, &turn, asked),
    }
}

/// A new default agent: another vendor can't resume the old session, so the next turn starts
/// a fresh one primed with an excerpt of the chat so far. A conversation that carries on an
/// agent's session from a run stays with that agent and its session.
pub(crate) fn fresh_session(ctx: &mut Ctx, chat: &mut ChatState) -> anyhow::Result<Vec<Effect>> {
    if let Some(place) = &chat.place {
        let agent = ctx.names.worker(&place.worker);
        let main = ctx.names.worker(&ctx.settings().default_agent);
        ctx.notice(
            NoteLevel::Info,
            format!(
                "This conversation carries on {} with {agent}, so it stays with that agent. \
                 /clear starts one with {main}.",
                place.agent
            ),
        );
        return Ok(Vec::new());
    }
    chat.session = None;
    chat.carried_model = None;
    chat.store.forget_session()?;
    Ok(Vec::new())
}

/// The screen is closing: a turn in flight is cancelled. The loop then waits a little for
/// every stopping step to end. A copy made again for the conversation goes too, unless its
/// agent may still be writing it; the next start removes it then.
pub(crate) fn close(ctx: &mut Ctx, chat: &mut ChatState) {
    let in_flight = stop_turn(ctx, chat).is_some() || !chat.stopping.is_empty();
    if let Some(place) = chat
        .place
        .as_ref()
        .filter(|place| place.made_again && !in_flight)
    {
        run_copy::leave(ctx, place);
    }
}

fn stop_turn(ctx: &mut Ctx, chat: &mut ChatState) -> Option<TurnId> {
    let mut turn = chat.turn.take()?;
    turn.cancel.cancel();
    direct::cancelled(ctx, chat, &mut turn);
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
        instructions: Vec::new(),
        request_settings: Vec::new(),
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

/// `/workflow <task>`: a crew run that's a workflow. Words that may say how to run it are read
/// first, as `/crew` reads them: the command keeps its mode and style, and the rest is checked
/// and followed.
pub(crate) async fn start_workflow(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    task: String,
) -> anyhow::Result<Vec<Effect>> {
    let task = task.trim().to_owned();
    let reads = ctx.settings().triage.read_instructions
        && instructions::might_say_how(&task, &Roster::new(ctx));
    if reads && chat.turn.is_some() {
        chat.queue(Queued::Workflow(task));
        return Ok(Vec::new());
    }
    let mut effects = name_the_chat(chat, &task);
    if reads {
        let command = Command {
            mode: RunMode::Crew,
            said: "you typed /workflow".to_owned(),
            style: Some(RunStyle::Workflow),
        };
        begin(ctx, chat, task, |record| TurnStep::Triage {
            uncommitted_note: record.uncommitted_note(),
            read_instructions: true,
            command: Some(command),
        })?;
        return Ok(effects);
    }
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
                Queued::Solo(task) => UiCommand::Slash {
                    command: SlashCommand::Solo { task },
                },
                Queued::Crew(task) => UiCommand::Slash {
                    command: SlashCommand::Crew { task },
                },
                Queued::Workflow(task) => UiCommand::Slash {
                    command: SlashCommand::Workflow { task: Some(task) },
                },
                Queued::Goal(objective) => UiCommand::Slash {
                    command: SlashCommand::Goal {
                        text: Some(objective),
                    },
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
    let passing = matches!(&event, ChatEvent::Agent { event, .. } if live::is_passing(event));
    if !passing
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
        | ChatEvent::TurnCancelled { turn }
        | ChatEvent::HandedOff { turn, .. }
        | ChatEvent::Joined { turn, .. } => turn,
    }
}
