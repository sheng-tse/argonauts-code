//! The conversation with the main agent: a new request's estimate, `/ask`, `/captain` and
//! `/crew`, and the `StartWork` choice. Every turn resumes the conversation's CLI session in the
//! repo root, read-only but for a direct turn: Captain you choose, which `direct` runs in your
//! checkout as the CLI itself would.
//!
//! Text typed while a turn runs waits in a queue and is sent once the turn ends. `lifecycle`
//! runs a turn from start to finish, `offer` asks whether to start work, `excerpt` primes a
//! session that can't resume the conversation, and `carry` makes a new main model on another CLI
//! its summary. `/clear` starts a new conversation, and `past` reads earlier ones back so `/resume`
//! can carry them on. `agent` does the same for the main agent CLI's own sessions, and `agent` for
//! the sessions of a run's agents, which carry on as direct conversations in the folder each ran
//! in.
//!
//! The commands every agent CLI has in some form work on the conversation whatever CLI the
//! main agent is: `side` asks and names off the record, compacts, recaps, and reviews; `copies`
//! branches, forks, exports, and copies; `prefs` holds `/fast`, `/output-style`, `/plan`, and
//! `/goal`.

mod agent;
#[cfg(test)]
mod agent_tests;
#[cfg(test)]
mod autopilot_tests;
mod carry;
#[cfg(test)]
mod carry_tests;
mod cli;
#[cfg(test)]
mod cli_tests;
mod copies;
mod crew;
mod direct;
#[cfg(test)]
mod direct_kept_out_tests;
#[cfg(test)]
mod direct_outside_tests;
#[cfg(test)]
mod direct_tests;
mod edits;
mod excerpt;
mod fork;
mod goal;
mod handover;
pub(crate) mod in_cli;
#[cfg(test)]
mod in_cli_tests;
mod keep;
mod lifecycle;
mod live;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod main_session_tests;
#[cfg(test)]
mod mention_tests;
mod mirror;
mod offer;
mod past;
#[cfg(test)]
mod picture_tests;
mod prefs;
pub(crate) mod redraw;
#[cfg(test)]
mod request_tests;
#[cfg(test)]
mod resume_tests;
mod run_news;
mod run_worktree;
#[cfg(test)]
mod shared_tests;
mod side;
mod standing;
pub(crate) mod starting;
#[cfg(test)]
mod starting_tests;
mod talk;
#[cfg(test)]
mod tests;
mod tools;

pub(crate) use crew::{cli_sessions as crew_cli_sessions, rows as crew_rows};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;

use argonauts_core::NoteLevel;
use argonauts_core::RunId;
use argonauts_core::agent_sessions::{AgentSessionRef, Resume};
use argonauts_core::api::{RunSummary, StartRun};
use argonauts_core::app::{
    AppEvent, AppMode, CommandReply, ConversationEvent, ConversationSummary, ReplayTurn, UiCommand,
    ViewData,
};
use argonauts_core::cli_commands::CliCommand;
use argonauts_core::cli_sessions::{CliSession, CliSessionRef};
use argonauts_core::events::context::{ContextDetail, ContextUsage};
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::session::CliWorkflowRun;
use argonauts_core::input::SlashCommand;
use argonauts_core::names::{ChoiceId, ConversationId, ModelRef, SessionKey, TurnId, WorkflowId};
use argonauts_core::pictures::Picture;
use argonauts_core::session_runner::RunnerEvent;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::intake::ChangeEstimate;
use argonauts_core::steps::{StepError, StepOutcome};
use argonauts_core::views::{ContextRow, InnerAgent};
use tokio_util::sync::CancellationToken;

pub(crate) use self::agent::SessionCache;
pub(crate) use self::copies::{branch, copy, export, fork};
pub(crate) use self::direct::{rewind, set_approval};
use self::excerpt::Exchange;
pub(crate) use self::goal::{
    Checked as GoalChecked, goal, goal_run_started, on_checked as goal_checked,
};
use self::lifecycle::{begin, on_done, on_event, on_retried};
use self::offer::Offer;
use self::past::PastConversation;
use self::prefs::Prefs;
pub(crate) use self::prefs::{fast, output_style, plan, saved};
pub(crate) use self::run_news::{
    hear as hear_runs, run_not_started, run_started as run_started_here,
};
pub(crate) use self::run_worktree::tidy_worktrees;
pub(crate) use self::side::{btw, compact, recap, rename, review};
pub(crate) use self::talk::conversation_steps;
#[cfg(test)]
pub(crate) use self::tests::checked_captain;
use super::instructions::{self, Command, Roster};
use super::recap::{mode_name, title_from_request};
use super::shell::ShellRan;
use super::turn::{DirectWords, TurnStep};
use super::ultra;
use super::{Ctx, Effect};
use crate::conversation_store::{
    ConversationPlace, ConversationRun, ConversationSession, ConversationStore,
};
use crate::model_memory;
use argonauts_core::events::approval::AnswerAction;

pub(crate) struct ConversationState {
    id: ConversationId,
    store: ConversationStore,
    title: String,
    turns: u32,
    /// The turn in flight.
    turn: Option<Turn>,
    /// Cancelled turns whose step hasn't ended yet. Their agent process may still write the
    /// session, so no turn resumes it meanwhile.
    stopping: Vec<TurnId>,
    /// Typed while a turn ran, oldest first, each with the last `!` command that had ended by
    /// then. Each is sent once the turn ends.
    queued: VecDeque<(Queued, u32)>,
    /// Open `StartWork` questions.
    offers: HashMap<ChoiceId, Offer>,
    /// `!` commands run since the main agent's last turn, oldest first. Your next message takes
    /// them: to the main agent, or to the run going.
    ran: Vec<ShellRan>,
    /// The id of the last `!` command that ended.
    last_shell: u32,
    /// While a message typed during a turn goes out, the last command that had ended when you
    /// typed it. Commands that ended later wait for your next message.
    shell_cutoff: Option<u32>,
    /// Each request and its answer, for priming a fresh session.
    history: Vec<Exchange>,
    /// What a switch of the main model to another CLI carries over.
    carry: carry::Carried,
    session: Option<ConversationSession>,
    /// The model of a CLI session this conversation carries on, kept while the conversation resumes
    /// it unless you set one.
    carried_model: Option<String>,
    /// Set when the conversation carries on an agent's session from a run: its turns are direct,
    /// with that agent, in the folder it ran in.
    place: Option<ConversationPlace>,
    context: Option<ContextRow>,
    /// Said once, so a broken journal doesn't flood the transcript.
    journal_warned: bool,
    /// Said once: Claude Code's notices stopped reading in this version.
    notice_warned: bool,
    /// The workflows the last turn's CLI ran itself, as it reported them.
    workflows: Vec<CliWorkflowRun>,
    /// Side questions asked so far, which number their turns apart from the conversation's.
    sides: u32,
    /// Choices for this app session, kept across `/clear` and `/resume`.
    prefs: Prefs,
    /// What `/goal` keeps for this conversation beyond the setting that holds the goal.
    goal: goal::Kept,
    direct: direct::DirectState,
    /// Something on screen shows the conversation's context by category, asked with this
    /// detail. Each turn's session is told as it starts.
    context_watch: Option<ContextDetail>,
    /// `/context print` asked the session at work to count itself, and prints once it has.
    print_wait: Option<PrintWait>,
    /// What the main session's setup, guide and lookup tools hold between calls.
    talk: talk::TalkState,
    /// The runs the conversation started, which the main session hears of.
    run_news: run_news::Watched,
    /// The MCP servers the main session's CLI brought from its own settings, by name, once a
    /// session has said.
    mcp_servers: Option<Vec<String>>,
    /// The pictures of the message just sent, for the turn that carries your words.
    pictures: Vec<Picture>,
    /// The files Codex's edits change, read around each edit for its patch.
    edits: edits::Edits,
    /// The request the main agent took alone while a crew workflow was only the default, which
    /// a bare `/crew` hands to the crew. Your next request replaces it.
    taken_alone: Option<String>,
    /// The main session's start as last worked out for the Context tab.
    start: starting::KeptStart,
}

struct Turn {
    id: TurnId,
    step: TurnStep,
    text: String,
    worker: ModelRef,
    /// Workers that already failed this turn, so a fallback never repeats one.
    tried: Vec<ModelRef>,
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
    /// What the session may reach outside its folder: for the line a refusal of a path on the
    /// never-list leaves, which holds only where the turn works outside, and for the writes
    /// there turned away from a turn that reads outside but writes only its folder.
    outside: argonauts_core::outside::Outside,
    /// What the session takes while it works: your messages, send now, and the screen.
    live: live::Live,
    /// The CLI's last count of the session's context, which the record keeps as the turn ends.
    counted: Option<Box<ContextUsage>>,
    /// The turn starts its session afresh and its start isn't kept yet.
    fresh_start: Option<starting::Fresh>,
    /// Its agent's CLI turned out signed out, so whoever carries it on says so.
    signed_out: Option<argonauts_core::app::SignedOut>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Queued {
    /// Your words, with the pictures their chips name.
    Request(String, Vec<Picture>),
    /// A request you typed `ultracode` in.
    WorkflowRequest(String, Vec<Picture>),
    Ask(String),
    /// One of the main agent CLI's own commands.
    Cli {
        name: String,
        args: String,
    },
    Side(String),
    Plan(String),
    /// `/captain <task>`.
    Captain(String),
    /// `/crew <task>`, whose words are read before the run starts.
    Crew(String),
    /// `/workflow <task>`, read the same way.
    Workflow(String),
    /// Your words for a subagent of the conversation's session, which the main agent passes on.
    Subagent {
        name: String,
        text: String,
    },
}

/// From a turn's task to the loop.
#[derive(Debug)]
pub(crate) enum TurnUpdate {
    /// An event of the turn's session, as it happens.
    Event { turn: TurnId, event: RunnerEvent },
    /// The step ended: its output, or why there is none.
    Done {
        turn: TurnId,
        outcome: Result<StepOutcome, StepError>,
    },
    /// A direct turn's CLI asks you something, and waits for the answer.
    Asked { turn: TurnId, asked: direct::Asked },
    /// What the turn's CLI asked was answered, or closed without an answer.
    Answered { turn: TurnId },
    /// The answer didn't fit its format, and the step asks the agent for it again.
    Retried { turn: TurnId },
}

impl ConversationState {
    /// A new conversation, with an id set aside so no other app session takes it.
    pub(crate) fn new(ctx: &Ctx) -> anyhow::Result<Self> {
        let conversation_dir = ctx.env.conversation_dir();
        let id = ConversationStore::claim(&conversation_dir).unwrap_or_else(|err| {
            // The journal's first write tells you, as it fails the same way.
            tracing::warn!("setting a conversation id aside: {err:#}");
            ConversationStore::next_id(&conversation_dir)
        });
        Ok(Self::empty(ctx, id))
    }

    fn empty(ctx: &Ctx, id: ConversationId) -> Self {
        let store = ConversationStore::open(&ctx.env.conversation_dir(), &id);
        Self {
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
            carry: carry::Carried::default(),
            session: None,
            carried_model: None,
            place: None,
            context: None,
            journal_warned: false,
            notice_warned: false,
            workflows: Vec::new(),
            sides: 0,
            prefs: Prefs::starting(ctx.settings()),
            goal: goal::Kept::default(),
            direct: direct::DirectState::default(),
            context_watch: None,
            print_wait: None,
            talk: talk::TalkState::default(),
            run_news: run_news::Watched::default(),
            mcp_servers: None,
            pictures: Vec::new(),
            edits: edits::Edits::default(),
            taken_alone: None,
            start: starting::KeptStart::default(),
        }
    }

    /// An earlier conversation, carried on: its turns go on numbering from its last, its
    /// answers prime a session that can't resume, and its CLI session resumes when the
    /// same agent takes the next turn.
    fn resumed(ctx: &Ctx, past: &PastConversation) -> Self {
        let id = past.summary.conversation.clone();
        let store = ConversationStore::open(&ctx.env.conversation_dir(), &id);
        let session = store.session();
        let place = store.place();
        let history = past
            .since_compacted()
            .iter()
            .filter_map(|turn| Some(Exchange::new(turn.request.clone(), turn.reply.as_ref()?)))
            .collect();
        let context =
            past::recorded_row(&store, past).map(|row| starting::as_its_cli_says(ctx, row));
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
            carry: carry::Carried::default(),
            session,
            carried_model: None,
            place,
            context,
            journal_warned: false,
            notice_warned: false,
            workflows: Vec::new(),
            sides: 0,
            prefs: Prefs::starting(ctx.settings()),
            goal: goal::Kept::default(),
            direct: direct::DirectState::default(),
            context_watch: None,
            print_wait: None,
            talk: talk::TalkState::default(),
            run_news: run_news::Watched::default(),
            mcp_servers: None,
            pictures: Vec::new(),
            edits: edits::Edits::default(),
            taken_alone: None,
            start: starting::KeptStart::default(),
        }
    }

    /// The agent whose session this conversation carries on, when its CLI isn't the main
    /// agent's. One that carries on a run's session keeps that agent whichever is the main one.
    fn written_by_another_cli(&self, ctx: &Ctx) -> Option<ModelRef> {
        let main = &ctx.settings().crew.main;
        self.session
            .as_ref()
            .filter(|session| self.place.is_none() && session.worker.agent != main.agent)
            .map(|session| session.worker.clone())
    }

    /// A new conversation to take `current`'s place, with `current`'s number when nothing was
    /// ever written there, so opening the app to carry a session on leaves no gap.
    fn instead_of(ctx: &Ctx, current: &ConversationState) -> anyhow::Result<Self> {
        if current.store.is_unused() {
            return Ok(Self::empty(ctx, current.id.clone()));
        }
        Self::new(ctx)
    }

    /// A new conversation that carries on a CLI's own session: its next turn resumes that
    /// session, or a copy of it when the launch forks, and its turns so far prime a fresh one on
    /// a fallback.
    fn carrying(
        ctx: &Ctx,
        current: &ConversationState,
        picked: &cli::Picked,
        worker: ModelRef,
    ) -> anyhow::Result<Self> {
        let mut conversation = Self::instead_of(ctx, current)?;
        let session = ConversationSession {
            worker,
            cli_session: picked.launch.session.clone(),
            fork: picked.launch.fork,
        };
        // Saved now, so `/resume conversation-<n>` finds the session after a restart too.
        conversation.store.save_session(&session)?;
        conversation.session = Some(session);
        conversation.title = picked.session.title.clone();
        conversation.history = picked.history();
        conversation.carried_model = picked.launch.model.clone();
        Ok(conversation)
    }

    /// Makes the next turn carry on a copy of `session`, which stays as it was.
    fn branch_off(&mut self, session: ConversationSession) -> anyhow::Result<()> {
        let session = ConversationSession {
            fork: true,
            ..session
        };
        // Saved now, so the branch still copies the session after a restart.
        self.store.save_session(&session)?;
        self.session = Some(session);
        Ok(())
    }

    /// A new conversation that carries on an agent's session from a run, through its CLI's own
    /// resume, with that agent, in `place.folder`.
    fn continuing(
        ctx: &Ctx,
        current: &ConversationState,
        found: &agent::Found,
        place: ConversationPlace,
    ) -> anyhow::Result<Self> {
        let mut conversation = Self::instead_of(ctx, current)?;
        let cli_session = found
            .cli_session
            .clone()
            .context("the session left no id to resume")?;
        let session = ConversationSession {
            worker: place.worker.clone(),
            cli_session,
            fork: false,
        };
        // Saved now, so `/resume conversation-<n>` carries it on in the same folder after a
        // restart.
        conversation.store.save_session(&session)?;
        conversation.store.save_place(&place)?;
        conversation.session = Some(session);
        conversation.title = found
            .task_title
            .clone()
            .unwrap_or_else(|| format!("{} {}", found.role.as_str(), found.run));
        conversation.carried_model = found.worker.model.clone();
        conversation.place = Some(place);
        Ok(conversation)
    }

    /// The conversation after this one keeps this app session's choices and open questions.
    /// What `/rewind` can put back stays only while both work in the same folder, and a worktree
    /// made again for this one goes once the next one works elsewhere.
    fn succeed(&mut self, ctx: &Ctx, mut next: ConversationState) {
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
        if next.id != self.id {
            self.store.release_if_empty();
            let out = ctx.out.clone();
            model_memory::conversation_ended(&ctx.env.paths, ctx.names.names(), move |text| {
                out.send(AppEvent::Notice {
                    level: NoteLevel::Info,
                    text,
                });
            });
        }
        *self = next;
        if let Some(place) = left {
            run_worktree::leave(ctx, &place);
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
                !running.step.own_session() && running.worker.agent == self.cli(ctx)
            })
    }

    /// The CLI its turns go to, such as `claude-code`: the agent's whose session it carries on,
    /// or the main agent's.
    pub(super) fn cli<'a>(&'a self, ctx: &'a Ctx) -> &'a str {
        self.next_worker(ctx).agent.as_str()
    }

    /// The worker its next turn runs: the agent's whose session it carries on, or the main
    /// agent.
    pub(crate) fn next_worker<'a>(&'a self, ctx: &'a Ctx) -> &'a ModelRef {
        self.place
            .as_ref()
            .map_or(&ctx.settings().crew.main, |place| &place.worker)
    }

    /// Typed while a turn runs, to go out once it ends. A message typed again as it goes out
    /// keeps the commands it was typed after.
    fn queue(&mut self, queued: Queued) {
        let after = self.shell_cutoff.unwrap_or(self.last_shell);
        self.queued.push_back((queued, after));
    }
}

/// The CLIs' own sessions, newest first, for `/resume`: this folder's, or every folder's.
pub(crate) async fn cli_sessions(ctx: &Ctx, every_folder: bool) -> Vec<CliSession> {
    cli::list(ctx, every_folder).await
}

/// Earlier conversations in this repo, newest first, for `/resume`. One whose run waits on you
/// is marked, since a run is found through the conversation that started it.
pub(crate) fn list(ctx: &Ctx, runs: &[RunSummary]) -> Vec<ConversationSummary> {
    let waiting: Vec<RunId> = runs
        .iter()
        .filter(|run| run.needs_you())
        .map(|run| run.run.clone())
        .collect();
    past::list(&ctx.env.conversation_dir(), &waiting)
}

/// `/clear`: a new conversation with the main agent, and the screen starts over under its
/// header, which says how to get back to the old one when it had turns. The old one stays in
/// `/resume`, open `StartWork` questions stay answerable, and runs keep going.
pub(crate) fn clear(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    if !settled(conversation) {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /clear starts a new conversation.",
        );
        return Ok(Vec::new());
    }
    let old = conversation.id.clone();
    let had_turns = conversation.turns > 0;
    // A goal covers the work that follows in its own conversation, and no other.
    goal::forget(ctx)?;
    conversation.succeed(ctx, ConversationState::new(ctx)?);
    ctx.send(AppEvent::ConversationCleared {
        way_back: had_turns.then_some(old),
    });
    Ok(vec![Effect::Status])
}

/// `/resume conversation-<n>`, or Enter on a conversation in the picker: draws it again as it
/// printed live, the runs it started included, and carries it on. The next turn resumes its CLI session, and a session another CLI wrote makes
/// that CLI the main agent, as picking the CLI's own session does. Where that CLI can't carry
/// it on, the main agent starts a fresh session primed with the conversation so far. One that
/// carries on an agent's session from a run is checked again first, since its run may be going
/// again.
pub(crate) async fn resume(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: &ConversationId,
) -> anyhow::Result<Vec<Effect>> {
    let id = past::normalize(id);
    if id == conversation.id && (conversation.turns > 0 || !conversation.store.runs().is_empty()) {
        ctx.notice(
            NoteLevel::Info,
            format!("{id} is the conversation on screen."),
        );
        return Ok(Vec::new());
    }
    if !settled(conversation) {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /resume carries the other one on.",
        );
        return Ok(Vec::new());
    }
    let past = past::load(&ctx.env.conversation_dir(), &id).filter(|past| !past.is_empty());
    let Some(past) = past else {
        anyhow::bail!("There's no conversation {id} in this repo. /resume lists them.");
    };
    let mut resumed = ConversationState::resumed(ctx, &past);
    // It carries on an agent's session, whose run may be going again since, and whose worktree may
    // be gone again.
    let mut made_again = false;
    if let Some(place) = resumed.place.as_mut() {
        if let Some(why) = agent::place_cannot(ctx, place).await {
            anyhow::bail!("{id} can't carry on: {why}.");
        }
        made_again = run_worktree::ensure_worktree(
            &ctx.env.repo_root,
            &place.folder,
            place.branch.as_deref(),
        )
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
    let written_elsewhere = resumed.written_by_another_cli(ctx);
    // Another CLI's session carries on in that CLI too, which takes the main agent's place.
    let taken_over = written_elsewhere
        .as_ref()
        .and_then(|writer| make_main(ctx, writer));
    let who_takes_it = written_elsewhere
        .as_ref()
        .map(|writer| who_takes_it(ctx, writer, taken_over.is_some()));
    let left = in_cli::left_off(&resumed.store);
    conversation.succeed(ctx, resumed);
    ctx.send(AppEvent::ConversationResumed {
        conversation: id,
        title: past.summary.title.clone(),
        worker: past.summary.worker.clone(),
        turns: Vec::new(),
        artifacts: past::artifacts(&ctx.env.conversation_dir(), &past.summary.conversation),
    });
    redraw::conversation(ctx, &conversation.store, &past.runs);
    in_cli::since_you_left(ctx, conversation, left).await;
    for text in where_it_works.into_iter().chain(who_takes_it) {
        ctx.notice(NoteLevel::Info, text);
    }
    note_run_on_screen(ctx);
    let mut effects = taken_over.unwrap_or_default();
    effects.push(Effect::Status);
    Ok(effects)
}

/// Before `run` comes on screen, the conversation that started it does, unless it is on screen
/// already or the run has none. `None` when it can't come now, as while a turn runs, and then
/// the run stays where it is too.
pub(crate) async fn come_home(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    run: &RunId,
) -> anyhow::Result<Option<Vec<Effect>>> {
    let home = past::home_of(&ctx.env.conversation_dir(), run);
    let Some(home) = home.filter(|home| *home != conversation.id) else {
        return Ok(Some(Vec::new()));
    };
    if !settled(conversation) {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "{run} opens in {home}, the conversation that started it. A turn is still \
                 running here: Esc stops it, then open the run again."
            ),
        );
        return Ok(None);
    }
    resume(ctx, conversation, &home).await.map(Some)
}

/// Who takes the next message of a conversation `writer`'s CLI wrote, once it is the main agent
/// or couldn't be made it.
fn who_takes_it(ctx: &Ctx, writer: &ModelRef, is_main_now: bool) -> String {
    let name = cli::name(ctx, &writer.agent);
    if is_main_now {
        return format!(
            "{name} wrote this conversation, so it is the main agent now, as /model would make \
             it, and your next message goes to its session through its own resume."
        );
    }
    let main = &ctx.settings().crew.main;
    format!(
        "{name} wrote this conversation and can't carry its session on here, so {main} takes \
         the next message, primed with its last turns."
    )
}

/// After a conversation is carried on while a run is on screen, where plain text goes.
fn note_run_on_screen(ctx: &Ctx) {
    if let AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Waiting { run, .. } =
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

/// `/resume conversation-<n> branch`, or `b` on a conversation in the picker: a new conversation
/// that starts from that one's turns, as `/branch` does from the one on screen. The original
/// keeps its turns and its CLI session: the new one's first turn carries on a copy of that
/// session where the main agent's CLI can fork it, and otherwise starts a fresh session
/// primed with the turns.
pub(crate) async fn branch_from(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: &ConversationId,
) -> anyhow::Result<Vec<Effect>> {
    if !settled(conversation) {
        ctx.notice(NoteLevel::Info, BRANCH_WAITS);
        return Ok(Vec::new());
    }
    let id = past::normalize(id);
    let past = past::load(&ctx.env.conversation_dir(), &id).filter(|past| !past.is_empty());
    let Some(past) = past else {
        anyhow::bail!("There's no conversation {id} in this repo. /resume lists them.");
    };
    let original = ConversationState::resumed(ctx, &past);
    // One that works in a run's folder keeps its session there, where this branch isn't.
    let to_copy = original
        .session
        .filter(|session| original.place.is_none() && main_agent_forks(ctx, &session.worker.agent));
    let mut next = ConversationState::instead_of(ctx, conversation)?;
    next.title = past.summary.title.clone();
    next.history = original.history;
    let starts = match to_copy {
        Some(session) => {
            let starts = branch_copies("its session", &cli::binary(ctx, &session.worker.agent));
            next.branch_off(session)?;
            starts
        }
        None => BRANCH_STARTS.to_owned(),
    };
    conversation.succeed(ctx, next);
    ctx.send(AppEvent::ConversationResumed {
        conversation: conversation.id.clone(),
        title: conversation.title.clone(),
        worker: past.summary.worker.clone(),
        turns: Vec::new(),
        artifacts: Vec::new(),
    });
    let original = ConversationStore::open(&ctx.env.conversation_dir(), &id);
    redraw::conversation(ctx, &original, &past.runs);
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Branched from {id} into {}. {starts} /resume {id} goes back to the original.",
            conversation.id
        ),
    );
    note_run_on_screen(ctx);
    Ok(vec![Effect::Status])
}

const BRANCH_WAITS: &str =
    "A turn is still running. Esc stops it, then /resume branches from the other one.";

const BRANCH_STARTS: &str = "Your next message starts a new session that has its last turns.";

/// What a branch says when its first turn carries on a copy of `what`, such as "it".
fn branch_copies(what: &str, binary: &str) -> String {
    format!("Your next message carries on a copy of {what}, through {binary}'s own fork.")
}

/// Whether the main agent can carry on a copy of a session `agent`'s CLI wrote: it is that
/// CLI, conversations resume their sessions, and the CLI has a fork of its own.
fn main_agent_forks(ctx: &Ctx, agent: &str) -> bool {
    let settings = ctx.settings();
    let main = &settings.crew.main;
    main.agent == agent
        && settings.sessions.conversation_resumes
        && ctx
            .services
            .workers
            .get(main)
            .is_ok_and(|handle| handle.capabilities().fork)
}

/// `/resume claude-code:<id> branch`, or `b` on a CLI's session in the picker: reprints its
/// last turns and carries on from them, so the CLI's own session is never written to. Where
/// the main agent's CLI wrote the session here and can fork it, the next turn carries on a copy
/// of it. Otherwise it starts a new session primed with the turns.
pub(crate) async fn branch_from_cli_session(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    reference: &CliSessionRef,
) -> anyhow::Result<Vec<Effect>> {
    if !settled(conversation) {
        ctx.notice(NoteLevel::Info, BRANCH_WAITS);
        return Ok(Vec::new());
    }
    let mut picked = cli::pick(ctx, reference).await?;
    let worker = ctx.settings().crew.main.clone();
    // A CLI finds a session to copy from the folder it ran in, which is where turns run here.
    let copies = picked.session.folder == ctx.env.repo_root
        && main_agent_forks(ctx, &picked.session.agent_id.0);
    let next = if copies {
        picked.launch = picked.launch.forked();
        ConversationState::carrying(ctx, conversation, &picked, worker.clone())?
    } else {
        let mut next = ConversationState::instead_of(ctx, conversation)?;
        next.title = picked.session.title.clone();
        next.history = picked.history();
        next
    };
    conversation.succeed(ctx, next);
    ctx.send(AppEvent::ConversationResumed {
        conversation: conversation.id.clone(),
        title: conversation.title.clone(),
        worker: Some(worker.clone()),
        turns: picked.replay(&picked.worker(&worker)),
        artifacts: Vec::new(),
    });
    let short = cli::short_id(&picked.session.id);
    let starts = if copies {
        branch_copies("it", picked.binary)
    } else {
        BRANCH_STARTS.to_owned()
    };
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Branched from {} session {short}, which stays as it was. {starts}",
            picked.name
        ),
    );
    note_run_on_screen(ctx);
    Ok(vec![Effect::Status])
}

/// `/resume claude-code:<id>`, or Enter on a CLI's session in the picker: reprints its last
/// turns condensed, as a resumed conversation does, and carries it on in a new conversation
/// whose next turn goes through the CLI's own resume. A session another CLI wrote makes that
/// CLI the main agent, so it carries on the same way.
pub(crate) async fn resume_cli_session(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    reference: &CliSessionRef,
) -> anyhow::Result<Vec<Effect>> {
    if !settled(conversation) {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /resume carries the other one on.",
        );
        return Ok(Vec::new());
    }
    let picked = cli::pick(ctx, reference).await?;
    // A conversation that only branches from the session isn't the session itself.
    let on_screen = conversation
        .session
        .as_ref()
        .is_some_and(|session| !session.fork && session.cli_session == picked.launch.session);
    let short = cli::short_id(&picked.session.id).to_owned();
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
    let default = ctx.settings().crew.main.clone();
    let worker = picked.worker(&default);
    // Another CLI's session carries on in that CLI too, which takes the main agent's place.
    let taken_over = if worker.agent == default.agent {
        Some(Vec::new())
    } else {
        make_main(ctx, &worker)
    };
    let next = ConversationState::carrying(ctx, conversation, &picked, worker.clone())?;
    conversation.succeed(ctx, next);
    ctx.send(AppEvent::ConversationResumed {
        conversation: conversation.id.clone(),
        title: conversation.title.clone(),
        worker: Some(worker.clone()),
        turns: picked.replay(&worker),
        artifacts: Vec::new(),
    });
    let binary = picked.binary;
    let how = match &taken_over {
        Some(_) if worker.agent == default.agent => {
            format!("Your next message goes to it through {binary}'s own resume.")
        }
        Some(_) => format!(
            "{} is the main agent now, as /model would make it, and your next message goes to \
             the session through {binary}'s own resume.",
            picked.name
        ),
        None => format!(
            "{} can't take a turn here, so {default} takes the next message, primed with this \
             session's last turns.",
            picked.name
        ),
    };
    ctx.notice(
        NoteLevel::Info,
        format!("Carrying on {} session {short}. {how}", picked.name),
    );
    note_run_on_screen(ctx);
    let mut effects = taken_over.unwrap_or_default();
    effects.push(Effect::Status);
    Ok(effects)
}

/// Makes `worker`'s CLI the main agent for this app session, so a session that CLI wrote carries
/// on through its own resume. `None` when it can't take a turn here or can't resume, and the
/// main agent stays.
fn make_main(ctx: &mut Ctx, worker: &ModelRef) -> Option<Vec<Effect>> {
    let resumes = ctx
        .services
        .workers
        .get(worker)
        .is_ok_and(|handle| handle.capabilities().resume);
    if !resumes {
        return None;
    }
    let saved = super::settings::save_answered(
        ctx,
        "crew.main",
        format!("\"{worker}\""),
        argonauts_core::settings::SettingScope::Session,
    );
    match saved {
        // The conversation that follows already holds the session, so nothing carries over
        // from the one it replaces. The new main agent still brings its own commands.
        Ok(effects) => Some(
            effects
                .into_iter()
                .map(|effect| match effect {
                    Effect::FreshConversation => Effect::CommandsChanged,
                    other => other,
                })
                .collect(),
        ),
        Err(err) => {
            tracing::warn!("making {worker} the main agent: {err:#}");
            None
        }
    }
}

/// `/resume run-<n>/s-<n>`, or `c` on a finished agent: carries the agent's session on in a new
/// conversation whose turns are direct, through its CLI's own resume, in the folder it ran in.
/// That's the run's worktree, made again from its branch when the run merged and the worktree is
/// gone. Only a run that ended lets go of its sessions: a session still running is taken over
/// instead, one whose run may still start it again waits for the run, and one that can't resume
/// shows its transcript.
pub(crate) async fn resume_agent(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    reference: &AgentSessionRef,
) -> anyhow::Result<Vec<Effect>> {
    if !settled(conversation) {
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
    let on_screen = conversation
        .place
        .as_ref()
        .is_some_and(|place| &place.agent == reference)
        && conversation
            .session
            .as_ref()
            .map(|session| &session.cli_session)
            == found.cli_session.as_ref();
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
            .context("making the run's worktree again")?
            .with_context(|| format!("{reference} can't carry on"))?
    };
    let place = ConversationPlace {
        agent: reference.clone(),
        worker: found.worker.clone(),
        folder,
        branch: found.branch.clone(),
        made_again,
    };
    let next = ConversationState::continuing(ctx, conversation, &found, place.clone())?;
    conversation.succeed(ctx, next);
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
            steps: Vec::new(),
        })
        .collect();
    ctx.send(AppEvent::ConversationResumed {
        conversation: conversation.id.clone(),
        title: conversation.title.clone(),
        worker: Some(worker.clone()),
        turns,
        artifacts: Vec::new(),
    });
    let agent = ctx.names.worker(&worker);
    let binary = agent::binary(ctx, &worker.agent);
    let mut text = format!(
        "Carrying on {who}, {agent}. Your messages go to it through {binary}'s own resume, and \
         it can change files. "
    );
    text.push_str(&where_it_works(ctx, &place, made_again));
    ctx.notice(NoteLevel::Info, text);
    note_run_on_screen(ctx);
    Ok(vec![Effect::Status])
}

/// Which files a carried-on agent works on, said plainly: the run's worktree, not your checkout.
fn where_it_works(ctx: &Ctx, place: &ConversationPlace, made_again: bool) -> String {
    let folder = ctx.env.display(&place.folder);
    if place.folder == ctx.env.repo_root {
        return "It works in your checkout.".to_owned();
    }
    let on_branch = place
        .branch
        .as_deref()
        .map_or_else(String::new, |branch| format!(" on {branch}"));
    let mut text =
        format!("It works on the run's worktree in {folder}{on_branch}, not your checkout.");
    if made_again {
        text.push_str(" That worktree was gone, so it was made again from the branch.");
    }
    text
}

/// `argonauts -c`: the newest conversation in this repo, or a new one when there's none.
pub(crate) async fn continue_newest(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    match list(ctx, &[]).first() {
        Some(newest) => {
            let id = newest.conversation.clone();
            resume(ctx, conversation, &id).await
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

pub(crate) fn title(conversation: &ConversationState) -> &str {
    &conversation.title
}

pub(crate) fn id(conversation: &ConversationState) -> &ConversationId {
    &conversation.id
}

/// When the conversation's first words were written, for the Status tab.
pub(crate) fn started(conversation: &ConversationState) -> Option<time::OffsetDateTime> {
    conversation.store.started()
}

/// The tools a turn that starts now is offered, by name.
pub(crate) fn main_tools(ctx: &Ctx, conversation: &ConversationState) -> Vec<String> {
    tools::offered(ctx, conversation)
}

/// The servers a session's tools come from, beside Argonauts Code's own: Claude Code names an MCP
/// tool `mcp__<server>__<tool>`.
pub(crate) fn own_mcp_servers(tools: &[String]) -> Vec<String> {
    let mut servers: Vec<String> = tools
        .iter()
        .filter_map(|tool| tool.strip_prefix("mcp__")?.split_once("__"))
        .map(|(server, _)| server.to_owned())
        .filter(|server| !server.starts_with("argonauts"))
        .collect();
    servers.sort();
    servers.dedup();
    servers
}

/// The pictures of the message just sent, which the turn that carries it takes.
pub(crate) fn send_pictures(conversation: &mut ConversationState, pictures: Vec<Picture>) {
    conversation.pictures = pictures;
}

/// The MCP servers the main session's CLI said it brought, once one has started.
pub(crate) fn mcp_servers(conversation: &ConversationState) -> Option<Vec<String>> {
    conversation.mcp_servers.clone()
}

/// No step of this conversation is still running.
pub(crate) fn settled(conversation: &ConversationState) -> bool {
    conversation.turn.is_none() && conversation.stopping.is_empty()
}

/// A run started or attached: the turn in flight answers, but starts no run and offers none.
pub(crate) fn run_attached(conversation: &mut ConversationState) {
    if let Some(turn) = conversation.turn.as_mut() {
        turn.run_began = true;
    }
}

/// The runs this conversation started or opened, which carrying it on brings back to the panel.
pub(crate) fn own_runs(conversation: &ConversationState) -> Vec<RunId> {
    conversation.store.runs()
}

/// Runs that came on screen are this conversation's, so `argonauts -c` brings them back.
pub(crate) fn keep_runs(conversation: &ConversationState, runs: Vec<ConversationRun>) {
    for kept in runs {
        if let Err(err) = conversation.store.keep_run(&kept) {
            tracing::warn!("saving {} with {}: {err:#}", kept.run, conversation.id);
        }
    }
}

/// A `!` command ended. The main agent hears of it with your next message, never by itself.
pub(crate) fn shell_ran(conversation: &mut ConversationState, ran: ShellRan) {
    conversation.last_shell = ran.id;
    super::shell::hold(&mut conversation.ran, ran);
}

/// The commands the message going out now hears of. One typed while a turn ran hears only of
/// those that had ended when you typed it.
pub(crate) fn take_shell(conversation: &mut ConversationState) -> Vec<ShellRan> {
    match conversation.shell_cutoff {
        None => std::mem::take(&mut conversation.ran),
        Some(cutoff) => {
            let (heard, later) = conversation.ran.drain(..).partition(|ran| ran.id <= cutoff);
            conversation.ran = later;
            heard
        }
    }
}

/// Commands a message took but couldn't deliver wait for the next one.
pub(crate) fn give_back_shell(conversation: &mut ConversationState, ran: Vec<ShellRan>) {
    conversation.ran.splice(0..0, ran);
}

/// A message typed during a turn goes out now, or has gone: `typed_after` is the last
/// command that had ended when you typed it.
pub(crate) fn sending_queued(conversation: &mut ConversationState, typed_after: Option<u32>) {
    conversation.shell_cutoff = typed_after;
}

/// The commands you ran before a run started were for the conversation, not the run.
pub(crate) fn run_started(conversation: &mut ConversationState) {
    conversation.ran.clear();
}

/// The conversation session's row for `/context`.
pub(crate) fn context_row(conversation: &ConversationState) -> Option<ContextRow> {
    conversation.context.clone()
}

/// /context opened: the running turn's session is asked for its context by category, where
/// its CLI can say. Between turns there's no session to ask.
pub(crate) fn ask_context(conversation: &ConversationState, detail: ContextDetail) {
    if let Some(turn) = &conversation.turn {
        turn.live.ask_context(detail);
    }
}

/// How long `/context print` waits for a count before it prints the last one it has.
const PRINT_WAIT: Duration = Duration::from_secs(2);

/// What `/context print` waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrintWait {
    /// The session was asked then, and hasn't answered.
    Asked(Instant),
    Counted,
}

/// `/context print` while the conversation's session is at work on a CLI that can count
/// itself: it is asked as its own /context counts, and the rows wait for the answer. False
/// where there is no one to ask, and the rows print from the last count.
pub(crate) fn count_then_print(ctx: &Ctx, conversation: &mut ConversationState) -> bool {
    let Some(turn) = conversation
        .turn
        .as_ref()
        .filter(|turn| !turn.step.own_session())
    else {
        return false;
    };
    let can_say = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
        .ok()
        .and_then(|adapters| {
            adapters
                .into_iter()
                .find(|adapter| adapter.id() == turn.worker.agent)
        })
        .is_some_and(|adapter| adapter.context_usage_line(ContextDetail::Full).is_some());
    if !can_say {
        return false;
    }
    turn.live.ask_context(ContextDetail::Full);
    conversation.print_wait = Some(PrintWait::Asked(Instant::now()));
    true
}

/// A fresh start of the main session is being kept, so the one the screen has may be out of
/// date.
pub(crate) fn start_kept_fresh(conversation: &ConversationState) -> bool {
    conversation.start.kept_fresh()
}

/// Whether the rows `/context print` waits to print should print now: the count is in, the
/// turn is over, or the session took too long to answer. True once.
pub(crate) fn print_is_due(conversation: &mut ConversationState) -> bool {
    let due = match conversation.print_wait {
        Some(PrintWait::Counted) => true,
        Some(PrintWait::Asked(at)) => conversation.turn.is_none() || at.elapsed() >= PRINT_WAIT,
        None => false,
    };
    if due {
        conversation.print_wait = None;
    }
    due
}

/// Something on screen started or stopped showing the conversation's context. The session
/// then asks after each fill, at most every 10 seconds.
pub(crate) fn watch_context(conversation: &mut ConversationState, detail: Option<ContextDetail>) {
    conversation.context_watch = detail;
    if let Some(turn) = &conversation.turn {
        turn.live.watch_context(detail);
    }
}

/// Records the request and runs `Estimate` in the conversation session.
///
/// Captain honours how you ask before it takes the request for itself. A main session that has
/// the start tool is told what your words ask and starts the run itself. One that can't start
/// a run has code read your words instead: words that plainly ask for a workflow hand the
/// request to the crew, and a reviewer you name keeps Captain doing the work, checked in a
/// worktree, with that reviewer reading the change before it lands.
pub(crate) async fn new_request(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    conversation.taken_alone = None;
    let code_reads_the_words = conversation.turn.is_none()
        && conversation.place.is_none()
        && !conversation.prefs.plan_first
        && direct::workflow_is_direct(ctx.settings())
        && ctx.settings().captain_or_crew.read_instructions
        && !takes_tools(ctx, conversation);
    if code_reads_the_words && instructions::plainly_asks_for_a_workflow(&text) {
        return start_as_workflow(ctx, conversation, text, "your words ask for a workflow").await;
    }
    if code_reads_the_words && instructions::names_a_reviewer(&text, &Roster::new(ctx)) {
        let text = text.trim().to_owned();
        let effects = name_the_conversation(conversation, &text);
        begin(ctx, conversation, text, |record| TurnStep::Estimate {
            uncommitted_note: record.uncommitted_note(),
            read_instructions: true,
            command: None,
        })?;
        return Ok(effects);
    }
    request(ctx, conversation, text)
}

/// Plain text while a turn runs: into the running session at once where its CLI takes it
/// there, as Claude Code does, and otherwise sent as the next turn.
pub(crate) fn queue_turn(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    let pictures = &conversation.pictures;
    let joined = conversation.turn.as_mut().is_some_and(|turn| {
        let trimmed = text.trim();
        !turn.step.own_session()
            && !trimmed.is_empty()
            && turn.live.takes_messages()
            && turn.live.send(trimmed.to_owned(), pictures.clone())
    });
    if joined {
        conversation.pictures.clear();
        return Ok(Vec::new());
    }
    request(ctx, conversation, text)
}

/// Enter on a subagent of your conversation's own session with words in the prompt. The
/// subagent has no line of its own when Argonauts Code drives its CLI, so the main agent passes
/// your words on: at once while its turn runs where the CLI takes that, else as the next
/// turn. The conversation shows `@name` and your words, never how to pass them on.
pub(crate) fn message_subagent(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    name: String,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    let words = text.trim().to_owned();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let prompt = pass_on(&name, &words);
    if let Some(turn) = conversation.turn.as_mut() {
        let joined = !turn.step.own_session()
            && turn.live.takes_messages()
            && turn
                .live
                .send_for_subagent(prompt, name.clone(), words.clone());
        if !joined {
            conversation.queue(Queued::Subagent { name, text: words });
        }
        return Ok(Vec::new());
    }
    let shown = format!("@{name} {words}");
    let mut effects = name_the_conversation(conversation, &shown);
    if is_the_cli(ctx, conversation) {
        effects.extend(direct::begin_agent(ctx, conversation, shown, prompt)?);
        return Ok(effects);
    }
    begin(ctx, conversation, shown, |_| TurnStep::Cli {
        prompt,
        direct: None,
    })?;
    Ok(effects)
}

/// What the main agent reads, to pass your message on to one of its subagents: your words
/// first, then how to pass them on. Claude Code continues a subagent with its SendMessage
/// tool, by the agent id the subagent's launch returned, and the same call resumes one that
/// finished (checked live on 2.1.292, where a call without `message` was refused, so the note
/// spells the tool's fields out).
fn pass_on(name: &str, words: &str) -> String {
    format!(
        "For your subagent \"{name}\": {words}\n\n(A message from me for your subagent \
         \"{name}\". Pass it on now with the SendMessage tool: `to` is that subagent's agent id, \
         `message` is my words above after the colon, word for word, and `summary` is a few \
         words about them. If the subagent has finished, this resumes it. Then carry on with \
         what you were doing.)"
    )
}

/// `x` on a subagent of your conversation's own session, or Ctrl+X Ctrl+K: the session stops
/// that one and goes on. Between turns there's no session to ask, and none of its subagents
/// is running either.
pub(crate) fn stop_subagent(ctx: &mut Ctx, conversation: &mut ConversationState, id: String) {
    let asked = conversation
        .turn
        .as_mut()
        .is_some_and(|turn| turn.live.stop_subagent(id));
    if !asked {
        ctx.notice(
            NoteLevel::Info,
            "That subagent isn't running, so there's nothing to stop.".to_owned(),
        );
    }
}

/// Ctrl+Enter: what you sent the running turn goes to the model now.
pub(crate) fn send_now(conversation: &mut ConversationState) {
    if let Some(turn) = conversation.turn.as_mut() {
        turn.live.send_now();
    }
}

/// ↑ on your waiting messages: the ones not sent yet come back at once, and the session is
/// asked for the ones the model hasn't read, which come back as it lets them go.
pub(crate) fn take_back(ctx: &mut Ctx, conversation: &mut ConversationState) {
    let texts: Vec<String> = conversation
        .queued
        .iter()
        .filter_map(|(queued, _)| match queued {
            Queued::Request(text, _) | Queued::WorkflowRequest(text, _) => Some(text.clone()),
            _ => None,
        })
        .collect();
    conversation
        .queued
        .retain(|(queued, _)| !matches!(queued, Queued::Request(..) | Queued::WorkflowRequest(..)));
    if let Some(turn) = conversation.turn.as_mut() {
        turn.live.take_back();
    }
    if !texts.is_empty() {
        ctx.send(AppEvent::TakenBack { texts });
    }
}

/// The workflow view of one the conversation's CLI runs itself.
pub(crate) fn workflow_view(
    ctx: &Ctx,
    conversation: &ConversationState,
    workflow: Option<&WorkflowId>,
) {
    mirror::show(ctx, conversation, workflow);
}

/// The view of one agent of a workflow the conversation's CLI runs itself.
pub(crate) fn inner_agent_view(ctx: &Ctx, conversation: &ConversationState, agent: &InnerAgent) {
    mirror::show_agent(ctx, conversation, agent);
}

/// A button above the input, pressed. A press for a session that has ended does nothing.
pub(crate) fn press_notice(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    plugin: &str,
    handle: u64,
) {
    let pressed = conversation
        .turn
        .as_mut()
        .is_some_and(|turn| turn.live.press(plugin, handle));
    if !pressed {
        ctx.hint("Claude Code takes that while it's working on your message.");
    }
}

/// A request you typed `ultracode` in goes through a workflow: the crew
/// workflow in use, Claude Code's own from the main session in Captain, or in Captain on a CLI with
/// none of its own, one Argonauts Code runs on that same model. It waits for a running turn, as
/// any request does.
pub(crate) async fn workflow_request(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if conversation.turn.is_some() {
        let pictures = std::mem::take(&mut conversation.pictures);
        conversation.queue(Queued::WorkflowRequest(text, pictures));
        return Ok(Vec::new());
    }
    let direct = direct::workflow_is_direct(ctx.settings()) || conversation.place.is_some();
    // A turn that goes to the main session notes its own use as it starts.
    let route = ultra::route(ctx.settings(), true);
    if matches!(route, ultra::Route::Crew | ultra::Route::SameModel) {
        crate::telemetry::used(&ctx.env.paths, crate::telemetry::Use::Ultracode);
    }
    match route {
        ultra::Route::Crew => start_workflow(ctx, conversation, text).await,
        ultra::Route::SameModel => Ok(same_model_workflow(ctx, conversation, text)),
        ultra::Route::MainSession(cli_workflow) if direct => {
            if let Some(why) = ultra::unavailable(ctx.settings()) {
                ctx.notice(NoteLevel::Info, why);
            }
            let mut effects = name_the_conversation(conversation, &text);
            let words = DirectWords {
                cli_workflow,
                ..DirectWords::default()
            };
            if conversation.prefs.plan_first {
                effects.extend(direct::plan(ctx, conversation, text, words)?);
            } else {
                effects.extend(direct::begin_with(ctx, conversation, text, None, words)?);
            }
            Ok(effects)
        }
        ultra::Route::MainSession(_) | ultra::Route::AsUsual => request(ctx, conversation, text),
    }
}

/// Captain on a CLI with no workflow of its own: Argonauts Code runs the workflow in its place,
/// every agent on that same CLI and model.
fn same_model_workflow(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    text: String,
) -> Vec<Effect> {
    let mut effects = name_the_conversation(conversation, &text);
    let mut run = run_request(ctx, text, RunMode::Crew, None, None);
    run.request_settings
        .extend(ultra::same_model(ctx.settings()));
    effects.push(handover::start(run));
    effects
}

/// Whether the CLI behind this conversation takes Argonauts Code's tools when Argonauts Code drives
/// it: the main agent's, or the agent's whose session the conversation carries on. Never while
/// `main_session.tools` is off.
fn takes_tools(ctx: &Ctx, conversation: &ConversationState) -> bool {
    if !ctx.settings().main_session.tools {
        return false;
    }
    ctx.services
        .workers
        .takes_session_tools(conversation.next_worker(ctx))
}

/// The main session is the CLI you chose: your message goes to it as you typed
/// it, it works in your checkout as it does by itself, and work leaves it only when it calls a
/// tool. True with Captain in use, for an agent's session carried on, and whatever the workflow
/// for a CLI that takes Argonauts Code's tools. A CLI that can't take them, with a crew workflow in
/// use, keeps the verdict that ends each reply, and code reads it.
fn is_the_cli(ctx: &Ctx, conversation: &ConversationState) -> bool {
    direct::workflow_is_direct(ctx.settings())
        || conversation.place.is_some()
        || takes_tools(ctx, conversation)
}

/// A turn at a time: text that arrives while one runs waits for it to end.
fn request(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if conversation.turn.is_some() {
        let pictures = std::mem::take(&mut conversation.pictures);
        conversation.queue(Queued::Request(text, pictures));
        return Ok(Vec::new());
    }
    let effects = name_the_conversation(conversation, &text);
    // The main session is the CLI itself: your message goes to it as its CLI takes it, with
    // no estimate first, and it hands work over by calling a tool. With plan first on, the
    // session plans before it changes anything.
    if is_the_cli(ctx, conversation) {
        let mut effects = effects;
        if conversation.prefs.plan_first {
            effects.extend(direct::plan(
                ctx,
                conversation,
                text,
                DirectWords::default(),
            )?);
        } else {
            effects.extend(direct::begin(ctx, conversation, text, None)?);
        }
        return Ok(effects);
    }
    let read_instructions = ctx.settings().captain_or_crew.read_instructions;
    begin(ctx, conversation, text, |record| TurnStep::Estimate {
        uncommitted_note: record.uncommitted_note(),
        read_instructions,
        command: None,
    })?;
    Ok(effects)
}

/// A request whose run plans first and waits for you; `/plan <task>`.
fn request_planned(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
) -> anyhow::Result<Vec<Effect>> {
    if conversation.turn.is_some() {
        conversation.queue(Queued::Plan(text));
        return Ok(Vec::new());
    }
    // Direct Captain, and an agent's session carried on, plan in the session that then goes ahead,
    // and change nothing until you say. With a workflow in use it is the workflow that plans
    // first and waits for you.
    if direct::workflow_is_direct(ctx.settings()) || conversation.place.is_some() {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let mut effects = name_the_conversation(conversation, &text);
        effects.extend(direct::plan(
            ctx,
            conversation,
            text,
            DirectWords::default(),
        )?);
        return Ok(effects);
    }
    let effects = request(ctx, conversation, text)?;
    if let Some(turn) = conversation.turn.as_mut() {
        turn.plan_first = true;
    }
    Ok(effects)
}

/// `/ask`: `ConversationAnswer`, never a run.
pub(crate) async fn ask(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    question: String,
) -> anyhow::Result<Vec<Effect>> {
    let question = question.trim().to_owned();
    if question.is_empty() {
        ctx.notice(NoteLevel::Info, "Put the question after /ask.");
        return Ok(Vec::new());
    }
    if conversation.turn.is_some() {
        conversation.queue(Queued::Ask(question));
        return Ok(Vec::new());
    }
    let run_status = run_status(ctx).await;
    let effects = name_the_conversation(conversation, &question);
    begin(ctx, conversation, question, |_| TurnStep::Ask {
        run_status,
    })?;
    Ok(effects)
}

/// One of the main agent CLI's own commands, sent to its session as `prompt`, the command
/// itself. The conversation shows it as you typed it, and the reply as an answer.
pub(crate) fn cli_command(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    command: &CliCommand,
    args: &str,
    prompt: String,
) -> anyhow::Result<Vec<Effect>> {
    if conversation.turn.is_some() {
        conversation.queue(Queued::Cli {
            name: command.name.clone(),
            args: args.trim().to_owned(),
        });
        return Ok(Vec::new());
    }
    let shown = command.invocation(args);
    let mut effects = name_the_conversation(conversation, &shown);
    // The Captain workflow, or an agent's session carried on: the command works where the
    // conversation does, as in the CLI itself.
    if is_the_cli(ctx, conversation) {
        effects.extend(direct::begin_agent(ctx, conversation, shown, prompt)?);
        return Ok(effects);
    }
    begin(ctx, conversation, shown, |_| TurnStep::Cli {
        prompt,
        direct: None,
    })?;
    Ok(effects)
}

/// [`cli_command`] that only reads, whatever the conversation's mode: `prompt` holds words anyone
/// may have written, so its session gets no tool that writes or runs a command, in your
/// checkout or anywhere, before you approve what comes of it.
pub(crate) fn reading_cli_command(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    command: &CliCommand,
    args: &str,
    prompt: String,
) -> anyhow::Result<Vec<Effect>> {
    if conversation.turn.is_some() {
        conversation.queue(Queued::Cli {
            name: command.name.clone(),
            args: args.trim().to_owned(),
        });
        return Ok(Vec::new());
    }
    let shown = command.invocation(args);
    let effects = name_the_conversation(conversation, &shown);
    begin(ctx, conversation, shown, |_| TurnStep::Cli {
        prompt,
        direct: None,
    })?;
    Ok(effects)
}

pub(crate) fn owns_choice(conversation: &ConversationState, id: &ChoiceId) -> bool {
    offer::owns_choice(conversation, id) || talk::owns(conversation, id)
}

/// The `StartWork` choice, and the main agent's own questions.
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    action: AnswerAction,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if talk::owns(conversation, &id) {
        return talk::answer(ctx, conversation, id, action);
    }
    offer::answer_choice(ctx, conversation, id, action, text).await
}

pub(crate) fn set_aside(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
) -> anyhow::Result<Vec<Effect>> {
    if talk::owns(conversation, &id) {
        return Ok(talk::set_aside(ctx, conversation, id));
    }
    offer::set_aside(ctx, conversation, id)
}

/// Autopilot came on with questions open, and nothing waits for you there: each question the
/// conversation would not have asked on autopilot gets what autopilot gives a new one, and
/// closes with the line that says so. What your own command opened to pick from, such as
/// `/rewind`'s list, stays yours.
pub(crate) fn autopilot_came_on(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    direct::set_approval(conversation, ApprovalMode::Autopilot);
    let mut effects = talk::settle_on_autopilot(ctx, conversation);
    effects.extend(direct::settle_on_autopilot(ctx, conversation)?);
    effects.extend(offer::settle_on_autopilot(ctx, conversation));
    Ok(effects)
}

/// `/captain` and `/crew`: a run with no estimate.
pub(crate) async fn start_forced(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
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
    let direct = mode == RunMode::Captain && direct::captain_is_direct(ctx.settings());
    // Your words are read first when they may say how to run, as a request's are: the command
    // keeps its mode, and the rest of what they say is checked and followed. Words that can't
    // say how leave the command as quick as it always was.
    let reads = ctx.settings().captain_or_crew.read_instructions
        && !(direct && conversation.prefs.plan_first)
        && instructions::might_say_how(&task, &Roster::new(ctx));
    if (direct || reads) && conversation.turn.is_some() {
        conversation.queue(match mode {
            RunMode::Captain => Queued::Captain(task),
            RunMode::Crew => Queued::Crew(task),
        });
        return Ok(Vec::new());
    }
    let mut effects = name_the_conversation(conversation, &task);
    if reads {
        let command = Command {
            mode,
            said: format!("you typed /{}", mode_name(mode)),
            style: None,
        };
        begin(ctx, conversation, task, |record| TurnStep::Estimate {
            uncommitted_note: record.uncommitted_note(),
            read_instructions: true,
            command: Some(command),
        })?;
        return Ok(effects);
    }
    // Captain you choose is a direct turn, unless something asks for the checked worktree. With
    // plan first on, it plans before it changes anything.
    if direct {
        if conversation.prefs.plan_first {
            effects.extend(direct::plan(
                ctx,
                conversation,
                task,
                DirectWords::default(),
            )?);
        } else {
            effects.extend(direct::begin(ctx, conversation, task, None)?);
        }
        return Ok(effects);
    }
    let mut request = run_request(ctx, task, mode, None, None);
    if conversation.prefs.plan_first {
        request = planned(request);
    }
    effects.push(handover::start(request));
    Ok(effects)
}

/// Esc during a turn: cancels the step, which asks the agent process to stop. The turn stays
/// stopping until its step ends. Anything queued behind it goes back into the input box.
///
/// While the turn's CLI runs subagents or workflows of its own, only the main agent's turn
/// stops, as on Claude Code's screen: they go on, and the turn stays for them
/// and for what you say next.
/// A bare `/crew`, as the line under a change the main agent took alone offers it: that request
/// goes to the crew, and the main agent's work on it stops. With none taken alone, it says what
/// `/crew` takes.
pub(crate) async fn crew_taken_alone(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    let request = conversation.taken_alone.take().unwrap_or_default();
    let working_on_it = conversation
        .turn
        .as_ref()
        .is_some_and(|turn| !request.is_empty() && turn.text == request);
    if working_on_it && let Some(id) = stop_turn(ctx, conversation) {
        settle_mode(ctx, &id);
    }
    start_forced(ctx, conversation, RunMode::Crew, request).await
}

pub(crate) fn cancel_turn(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    let spared = conversation
        .turn
        .as_mut()
        .is_some_and(|turn| turn.live.stop_turn_alone());
    if !spared {
        let Some(id) = stop_turn(ctx, conversation) else {
            return Ok(Vec::new());
        };
        settle_mode(ctx, &id);
    }
    let texts: Vec<String> = conversation
        .queued
        .drain(..)
        .map(|(queued, _)| match queued {
            Queued::Request(text, _) | Queued::WorkflowRequest(text, _) => text,
            Queued::Ask(question) => format!("/ask {question}"),
            Queued::Cli { name, args } if args.is_empty() => format!("/{name}"),
            Queued::Cli { name, args } => format!("/{name} {args}"),
            Queued::Side(question) => format!("/btw {question}"),
            Queued::Plan(task) => format!("/plan {task}"),
            Queued::Captain(task) => format!("/captain {task}"),
            Queued::Crew(task) => format!("/crew {task}"),
            Queued::Workflow(task) => task.clone(),
            Queued::Subagent { name, text } => format!("@{name} {text}"),
        })
        .collect();
    if !texts.is_empty() {
        ctx.send(AppEvent::TakenBack { texts });
    }
    Ok(Vec::new())
}

pub(crate) async fn on_update(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    update: TurnUpdate,
) -> anyhow::Result<Vec<Effect>> {
    match update {
        TurnUpdate::Event { turn, event } => {
            on_event(ctx, conversation, &turn, event);
            Ok(Vec::new())
        }
        TurnUpdate::Done { turn, outcome } => on_done(ctx, conversation, &turn, outcome),
        TurnUpdate::Asked { turn, asked } => {
            // The CLI waits on the answer, which the inactivity watchdog mustn't take for a
            // session gone quiet.
            if let Some(current) = conversation.turn.as_ref().filter(|now| now.id == turn) {
                current.live.asking(true);
            }
            direct::on_asked(ctx, conversation, &turn, asked)
        }
        TurnUpdate::Answered { turn } => {
            if let Some(current) = conversation.turn.as_ref().filter(|now| now.id == turn) {
                current.live.asking(false);
            }
            Ok(Vec::new())
        }
        TurnUpdate::Retried { turn } => {
            on_retried(ctx, conversation, &turn);
            Ok(Vec::new())
        }
    }
}

/// A new main agent: on the same CLI the session carries on, and on another the next turn
/// starts a fresh one from what `switch.carry` says. A conversation that carries on an agent's
/// session from a run stays with that agent and its session.
pub(crate) fn fresh_session(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    if let Some(place) = &conversation.place {
        let agent = ctx.names.worker(&place.worker);
        let main = ctx.names.worker(&ctx.settings().crew.main);
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
    carry::switched(ctx, conversation)
}

/// The screen is closing: a turn in flight is cancelled. The loop then waits a little for
/// every stopping step to end. A worktree made again for the conversation goes too, unless its
/// agent may still be writing it; the next start removes it then.
pub(crate) fn close(ctx: &mut Ctx, conversation: &mut ConversationState) {
    let in_flight = stop_turn(ctx, conversation).is_some() || !conversation.stopping.is_empty();
    if let Some(place) = conversation
        .place
        .as_ref()
        .filter(|place| place.made_again && !in_flight)
    {
        run_worktree::leave(ctx, place);
    }
    conversation.store.release_if_empty();
}

fn stop_turn(ctx: &mut Ctx, conversation: &mut ConversationState) -> Option<TurnId> {
    let mut turn = conversation.turn.take()?;
    turn.cancel.cancel();
    direct::cancelled(ctx, conversation, &mut turn);
    conversation.stopping.push(turn.id.clone());
    emit(
        ctx,
        conversation,
        ConversationEvent::TurnCancelled {
            turn: turn.id.clone(),
        },
    );
    Some(turn.id)
}

/// The first request names the conversation, and the terminal's title shows it.
fn name_the_conversation(conversation: &mut ConversationState, text: &str) -> Vec<Effect> {
    if !conversation.title.is_empty() {
        return Vec::new();
    }
    conversation.title = title_from_request(text);
    vec![Effect::Status]
}

/// A run you picked, forced like `/captain` and `/crew`.
fn run_request(
    ctx: &Ctx,
    request: String,
    mode: RunMode,
    estimate: Option<ChangeEstimate>,
    conversation_turn: Option<TurnId>,
) -> StartRun {
    let settings = ctx.settings();
    StartRun {
        request,
        mode,
        forced: true,
        estimate,
        captain_or_crew: None,
        conversation_turn,
        continues: None,
        overrides: ctx.settings.run_overrides(),
        include_uncommitted: settings.intake.include_uncommitted,
        main_agent: Some(settings.crew.main.clone()),
        approval_mode: None,
        outside_agent: None,
        instructions: Vec::new(),
        request_settings: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    }
}

/// The same run, planned first: the crew plans, and the plan waits for you whatever the
/// approvals say. A Captain run has no plan, so it becomes a crew run.
fn planned(mut request: StartRun) -> StartRun {
    request.mode = RunMode::Crew;
    request
        .overrides
        .push(format!("{PLAN_MODE_KEY}=\"supervised\""));
    request
}

/// The plan approval's mode, which `/plan` sets for one run.
const PLAN_MODE_KEY: &str = "approvals.plan.mode";

/// `/workflow`'s run is the crew's, whatever the words say. A goal is no kind of run: it comes
/// with the settings every run takes, from `/goal` or the request's own words.
pub(super) fn styled(mut request: StartRun, _style: RunStyle) -> StartRun {
    request.mode = RunMode::Crew;
    request
}

/// `/workflow <task>`: a crew run that's a workflow. Words that may say how to run it are read
/// first, as `/crew` reads them: the command keeps its mode and style, and the rest is checked
/// and followed.
pub(crate) async fn start_workflow(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    task: String,
) -> anyhow::Result<Vec<Effect>> {
    start_as_workflow(ctx, conversation, task, "you typed /workflow").await
}

/// A crew run that's a workflow, which a command or your own words chose and `said` names.
async fn start_as_workflow(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    task: String,
    said: &str,
) -> anyhow::Result<Vec<Effect>> {
    let task = task.trim().to_owned();
    let reads = ctx.settings().captain_or_crew.read_instructions
        && instructions::might_say_how(&task, &Roster::new(ctx));
    if reads && conversation.turn.is_some() {
        conversation.queue(Queued::Workflow(task));
        return Ok(Vec::new());
    }
    let mut effects = name_the_conversation(conversation, &task);
    if reads {
        let command = Command {
            mode: RunMode::Crew,
            said: said.to_owned(),
            style: Some(RunStyle::Workflow),
        };
        begin(ctx, conversation, task, |record| TurnStep::Estimate {
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
    if conversation.prefs.plan_first {
        request = planned(request);
    }
    effects.push(handover::start(request));
    Ok(effects)
}

/// `/<name> <request>`: a crew run that follows the workflow of that name, for this request
/// alone. The workflow in use stays as it was.
pub(crate) async fn start_with_workflow(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    request: String,
    workflow: &str,
) -> anyhow::Result<Vec<Effect>> {
    let request = request.trim().to_owned();
    let mut effects = name_the_conversation(conversation, &request);
    let mut run = run_request(ctx, request, RunMode::Crew, None, None);
    let named = toml::Value::String(workflow.to_owned()).to_string();
    run.request_settings.push(format!("workflow={named}"));
    effects.push(handover::start(run));
    Ok(effects)
}

/// The run on screen's zero-token summary, for `/ask` during a run.
async fn run_status(ctx: &Ctx) -> Option<String> {
    let run = match &ctx.mode {
        AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Waiting { run, .. } => run,
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
fn send_queued(conversation: &mut ConversationState) -> Vec<Effect> {
    conversation
        .queued
        .drain(..)
        .map(|(queued, typed_after)| {
            let command = match queued {
                Queued::Request(text, pictures) => UiCommand::Submit {
                    text,
                    workflow_requested: false,
                    pictures,
                },
                Queued::WorkflowRequest(text, pictures) => UiCommand::Submit {
                    text,
                    workflow_requested: true,
                    pictures,
                },
                Queued::Ask(question) => UiCommand::Slash {
                    command: SlashCommand::Ask { question },
                },
                Queued::Cli { name, args } => UiCommand::Slash {
                    command: SlashCommand::Cli { name, args },
                },
                Queued::Side(question) => UiCommand::Slash {
                    command: SlashCommand::Btw {
                        question: Some(question),
                    },
                },
                Queued::Plan(task) => UiCommand::Slash {
                    command: SlashCommand::Plan { task: Some(task) },
                },
                Queued::Captain(task) => UiCommand::Slash {
                    command: SlashCommand::Captain { task },
                },
                Queued::Crew(task) => UiCommand::Slash {
                    command: SlashCommand::Crew { task: Some(task) },
                },
                Queued::Subagent { name, text } => UiCommand::MessageSubagent { name, text },
                Queued::Workflow(task) => UiCommand::Slash {
                    command: SlashCommand::WorkflowRun { task: Some(task) },
                },
            };
            Effect::Queued {
                command: Box::new(command),
                typed_after,
            }
        })
        .collect()
}

/// A slash command you typed goes in the record where you typed it, so the conversation drawn
/// again prints it there.
pub(crate) fn command_typed(ctx: &Ctx, conversation: &mut ConversationState, text: String) {
    let text = text.trim();
    if !text.is_empty() {
        let typed = ConversationEvent::CommandTyped {
            text: text.to_owned(),
            at: Some(time::OffsetDateTime::now_utc()),
        };
        record(ctx, conversation, &typed);
    }
}

/// A line a command you typed printed back goes in the record after the command, so the
/// conversation drawn again prints it under it.
pub(crate) fn command_replied(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    reply: CommandReply,
) {
    if !reply.text.trim().is_empty() {
        record(
            ctx,
            conversation,
            &ConversationEvent::CommandReplied { reply },
        );
    }
}

/// Sends the event and records it, except the streamed text, which the finished turn holds,
/// and side questions, which stay out of the conversation.
fn emit(ctx: &Ctx, conversation: &mut ConversationState, event: ConversationEvent) {
    if event_turn(&event).is_some_and(carry::is_carry_turn) {
        return;
    }
    let passing =
        matches!(&event, ConversationEvent::Agent { event, .. } if live::is_passing(event));
    if !passing {
        record(ctx, conversation, &event);
    }
    ctx.send(AppEvent::Conversation { event });
}

/// Writes the event in the conversation's record, where a side question's never goes.
fn record(ctx: &Ctx, conversation: &mut ConversationState, event: &ConversationEvent) {
    let kept = event_turn(event)
        .is_none_or(|turn| !side::is_side_turn(turn) && !carry::is_carry_turn(turn));
    if kept
        && let Err(err) = conversation.store.append(event)
        && !conversation.journal_warned
    {
        conversation.journal_warned = true;
        ctx.notice(
            NoteLevel::Warn,
            format!(
                "The conversation isn't being recorded in {}: {err:#}",
                ctx.env.display(conversation.store.journal_path())
            ),
        );
    }
}

/// The turn the event belongs to. A command you typed belongs to none.
fn event_turn(event: &ConversationEvent) -> Option<&TurnId> {
    match event {
        ConversationEvent::TurnStarted { turn, .. }
        | ConversationEvent::Agent { turn, .. }
        | ConversationEvent::TurnFinished { turn, .. }
        | ConversationEvent::TurnFailed { turn, .. }
        | ConversationEvent::TurnCancelled { turn }
        | ConversationEvent::TurnStopped { turn, .. }
        | ConversationEvent::FellBack { turn, .. }
        | ConversationEvent::Joined { turn, .. }
        | ConversationEvent::AnswerRetried { turn }
        | ConversationEvent::ContextLeft { turn, .. } => Some(turn),
        ConversationEvent::CommandTyped { .. } | ConversationEvent::CommandReplied { .. } => None,
    }
}
