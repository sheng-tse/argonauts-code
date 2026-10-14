//! Direct Captain: Captain you choose is a turn of the main session in your checkout,
//! as its CLI works when you run it. Your words go through as you typed them, the CLI's own
//! tools and agents do the work, and its last message is the end. No run, task, report, checks,
//! merge, run review, or learned workflow.
//!
//! Around the turn Argonauts Code keeps the sandbox, the limit per message, and fallback, and adds
//! what the CLI would have you answer in its own window: `ask` builds the questions, `listen` takes
//! the questions in, and `rewind` keeps a snapshot before and after each turn so `/rewind` can
//! put files back.

mod ask;
mod listen;
mod rewind;
mod rm;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use argonauts_conductor::sandbox::profile::PROTECTED_PATHS;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::asking::{Ask, AskReply};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::events::run::{CaptainOrCrewDecision, RunMode};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::{ChoiceId, ModelRef, TurnId};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::settings::fallback::FallbackEntry;
use argonauts_core::settings::protocol::{CaptainKind, ForcedMode};
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::{Asks, NoteLevel};
use serde_json::Value;

pub(crate) use self::listen::Asked;
use self::listen::Listener;
use self::rewind::Snapshot;
use super::{
    ConversationState, Turn, handover, lifecycle, run_request, send_queued, standing, tools,
};
use crate::choices::{self, option};
use crate::controller::turn::{self, DirectWords, TurnStep, part};
use crate::controller::{Ctx, Effect};
use argonauts_conductor::fallback::shortlist::{
    Sources, auto_check, shortlist as fallback_shortlist,
};
use argonauts_core::TaskId;
use argonauts_core::events::task::{TaskKind, TaskLimit, TaskSpec};
use argonauts_core::limits::{Candidate, CandidateQuery, TrackRecord, TrackRecords};
use argonauts_core::names::RoleName;

const ANTIGRAVITY: &str = "antigravity-cli";

/// What agy's resumed turn is told when you let it run what it was refused.
const RUN_REFUSED: &str = "You may run the commands you were refused.";

/// Direct Captain's part of the conversation.
#[derive(Debug, Default)]
pub(crate) struct DirectState {
    /// The approval mode on screen, which says whether a direct turn asks.
    approval: ApprovalMode,
    /// Open questions from the turn's CLI, by their id.
    asks: HashMap<ChoiceId, PendingAsk>,
    /// "Yes, and allow edits for the rest of this conversation".
    edits_allowed: bool,
    /// Each direct turn that changed files, oldest first.
    turns: Vec<TurnRecord>,
    /// Stopped turns whose session hasn't ended yet, so their files may still change.
    stopping: HashMap<TurnId, Running>,
    /// agy's refused commands, offered after its turn.
    refused: HashMap<ChoiceId, RefusedOffer>,
    /// The open `/rewind` question. Its answers name turns by their place in `turns`.
    rewinding: Option<ChoiceId>,
    /// Said with your next message: the files `/rewind` put back.
    rewound: Option<String>,
    /// Plans made with plan first on, waiting for "Go ahead with this plan?".
    plans: HashMap<ChoiceId, Planned>,
    /// A turn whose agent ran out, waiting for you to pick who carries it on, or who answers
    /// when it changes nothing.
    fallback: Option<(ChoiceId, PendingFallback)>,
    /// Ignored files turns wrote, which snapshots keep from then on so `/rewind` can put them
    /// back.
    watched: BTreeSet<String>,
    /// The setup line the conversation's session last heard, so it hears a new one only when
    /// something changed.
    pub(super) told_setup: Option<String>,
}

impl std::fmt::Debug for PendingFallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingFallback")
            .field("turn", &self.turn.id)
            .field("why", &self.why)
            .field("candidates", &self.candidates)
            .finish_non_exhaustive()
    }
}

/// A turn that ran out, held while the fallback question is open.
struct PendingFallback {
    turn: Turn,
    tried: Vec<ModelRef>,
    why: String,
    /// In the question's order.
    candidates: Vec<ModelRef>,
}

/// A plan-first turn's request, for the turn that goes ahead with it.
#[derive(Debug)]
struct Planned {
    request: String,
    words: DirectWords,
    /// The CLI that planned, as the screen names it.
    cli: String,
}

impl DirectState {
    /// The next conversation keeps the mode and what `/rewind` can put back, and asks about
    /// edits again.
    pub(super) fn carry(&mut self) -> Self {
        Self {
            approval: self.approval,
            turns: std::mem::take(&mut self.turns),
            watched: std::mem::take(&mut self.watched),
            ..Self::default()
        }
    }

    /// A conversation that works in another folder can't put this one's files back, and the
    /// ignored files kept for it are that folder's.
    pub(super) fn forget_turns(&mut self) {
        self.turns.clear();
        self.watched.clear();
    }
}

/// A direct turn in flight.
#[derive(Debug)]
pub(super) struct Running {
    /// Your message.
    text: String,
    /// Answers the turn's questions, and stops when the turn ends.
    listener: Option<Listener>,
    asks: Asks,
    before: Option<Snapshot>,
    /// The commands the turn's tool calls ran, by call id.
    commands: HashMap<String, String>,
    /// Commands agy refused because they needed asking.
    refused: Vec<String>,
    /// What the agent said so far, for a fallback.
    said: String,
    /// Ignored files earlier turns wrote, which every snapshot keeps.
    watched: BTreeSet<String>,
    /// Files the turn's own tool calls wrote, by path from your folder.
    written: BTreeSet<String>,
    /// Protected files that weren't there when the turn started. The sandbox holds a protected
    /// folder that isn't there yet, but a file only on macOS, so the turn's end checks these.
    unmade: Vec<String>,
    /// The folder it works in, whose files `/rewind` puts back.
    root: PathBuf,
    /// The session has Argonauts Code's tools, so a setup you answered may write Argonauts Code's
    /// files.
    has_tools: bool,
    /// Files a setup you answered wrote during the turn, by path from your folder. The turn's
    /// end leaves them as the setup wrote them.
    setup_wrote: BTreeSet<String>,
}

#[derive(Debug)]
struct PendingAsk {
    turn: TurnId,
    tool: String,
    input: Value,
    question: ask::Question,
    reply: tokio::sync::oneshot::Sender<AskReply>,
}

#[derive(Debug)]
struct RefusedOffer {
    worker: ModelRef,
    /// The turn's request, which the turn that runs the commands carries on.
    request: String,
}

#[derive(Debug)]
struct TurnRecord {
    text: String,
    before: Snapshot,
    after: Snapshot,
    changed: Vec<String>,
}

/// Captain you choose runs direct: `captain.kind` says so, and nothing that needs a worktree is on.
pub(crate) fn captain_is_direct(settings: &Settings) -> bool {
    settings.captain.direct()
}

/// What a direct turn after the estimate says, so the agent starts from the plan it wrote.
pub(super) const GO_AHEAD: &str = "Go ahead.";

/// The choice settled on direct Captain: Captain your words chose where `captain.kind` is direct,
/// or Captain the rules picked where `captain_or_crew.captain_kind` says so, with nothing on that
/// needs a worktree.
pub(super) fn picks_direct(captain_or_crew: &CaptainOrCrewDecision) -> bool {
    captain_or_crew.mode == RunMode::Captain && captain_or_crew.captain == Some(CaptainKind::Direct)
}

/// The Captain workflow sends every message to the main session as a direct turn.
pub(crate) fn workflow_is_direct(settings: &Settings) -> bool {
    settings.captain_or_crew.force == Some(ForcedMode::Captain) && captain_is_direct(settings)
}

pub(crate) fn set_approval(conversation: &mut ConversationState, mode: ApprovalMode) {
    conversation.direct.approval = mode;
}

/// The approval mode on screen.
pub(super) fn approval(conversation: &ConversationState) -> ApprovalMode {
    conversation.direct.approval
}

/// Starts a direct turn with your message. `asks` overrides what the mode says, as agy's
/// resumed turn does. A CLI that can't ask before each edit gets a checked run instead, when
/// `captain.asks` wants every change asked about.
pub(super) fn begin(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
    asks: Option<Asks>,
) -> anyhow::Result<Vec<Effect>> {
    begin_with(ctx, conversation, text, asks, DirectWords::default())
}

/// [`begin`], with what your checked words change about the turn.
pub(super) fn begin_with(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
    asks: Option<Asks>,
    words: DirectWords,
) -> anyhow::Result<Vec<Effect>> {
    let asks = asks.unwrap_or_else(|| mode_asks(ctx, conversation, &words));
    if let Some(instead) = instead(ctx, conversation, asks, &text) {
        return Ok(instead);
    }
    start(ctx, conversation, text, asks, |asking| TurnStep::Direct {
        asking,
        go_ahead: None,
        words,
    })?;
    Ok(Vec::new())
}

/// The session that just read `request` goes ahead with it in your checkout. The turn is your
/// request, for `/rewind`, the record, and a run it starts, and the session reads only `words`
/// such as "Go ahead.", as it has your message already.
pub(super) fn go_ahead(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    request: String,
    said: &str,
    asks: Option<Asks>,
    words: DirectWords,
) -> anyhow::Result<Vec<Effect>> {
    let asks = asks.unwrap_or_else(|| mode_asks(ctx, conversation, &words));
    if let Some(instead) = instead(ctx, conversation, asks, &request) {
        return Ok(instead);
    }
    start(ctx, conversation, request, asks, |asking| {
        TurnStep::Direct {
            asking,
            go_ahead: Some(said.to_owned()),
            words,
        }
    })?;
    Ok(Vec::new())
}

/// Who may carry a direct turn on when its agent runs out: `[fallback.main]`'s list, or with
/// no list anywhere the other installed models, as a run's shortlist judges them for the main
/// role, so a disabled, testing, or over-cap agent is left out and a paid API needs your OK.
/// With `same_cli` only the stopped CLI's own models may.
pub(super) fn shortlist(ctx: &Ctx, stopped: &ModelRef, tried: &[ModelRef]) -> Vec<Candidate> {
    let settings = ctx.settings();
    let rules = settings
        .fallback
        .applied_to(RoleName::Main, &stopped.agent_id());
    let listed = if rules.candidates.is_empty() {
        let defaults = turn::default_fallbacks(ctx);
        defaults.into_iter().map(FallbackEntry::Model).collect()
    } else {
        rules.candidates.clone()
    };
    let query = CandidateQuery {
        task: your_checkout(),
        role: RoleName::Main,
        stopped: stopped.clone(),
        exclude_providers: Vec::new(),
        fallback: listed,
        all_valid: false,
    };
    let sources = Sources {
        settings,
        registry: ctx.services.registry.as_ref(),
        plan_usage: ctx.services.plan_usage.as_ref(),
        health: ctx.services.health.as_ref(),
        records: &NoRecords,
        slots: None,
        workers: Some(ctx.services.workers.as_ref()),
    };
    fallback_shortlist(&query, &sources)
        .candidates
        .into_iter()
        .filter(|candidate| {
            !tried.contains(&candidate.worker)
                && (!rules.same_cli || candidate.worker.agent == stopped.agent)
                && ctx.services.workers.get(&candidate.worker).is_ok()
        })
        .collect()
}

/// A direct turn has no task of its own, so the shortlist judges it as one Captain task that owns
/// your checkout.
fn your_checkout() -> TaskSpec {
    TaskSpec {
        id: TaskId::new("direct-001").expect("a valid task id"),
        title: String::new(),
        task: String::new(),
        kind: TaskKind::Captain,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: None,
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
    }
}

/// Track records order a run's candidates; a direct turn takes its list's order.
struct NoRecords;

impl TrackRecords for NoRecords {
    fn get(&self, _: &ModelRef, _: &str) -> Option<TrackRecord> {
        None
    }
}

/// The fallback question: who carries the turn on, each with what it costs, or stop here. A turn
/// that changes nothing has nothing to carry on, so it asks who answers and that agent reads
/// the turn from its start.
pub(super) fn ask_fallback(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: Turn,
    tried: Vec<ModelRef>,
    why: String,
    candidates: &[Candidate],
) {
    let from = ctx.names.worker(&turn.worker);
    let mut options: Vec<_> = candidates
        .iter()
        .take(9)
        .zip('1'..='9')
        .map(|(candidate, key)| {
            let mut offered = option(
                key,
                &ctx.names.worker(&candidate.worker),
                AnswerAction::Choose {
                    value: candidate.worker.to_string(),
                },
                false,
            );
            offered.detail = Some(if candidate.needs_confirm {
                "an API key, billed to your account".to_owned()
            } else {
                candidate.plan_usage_text.clone()
            })
            .filter(|detail| !detail.trim().is_empty());
            offered
        })
        .collect();
    options.push(option(
        's',
        "Stop here",
        AnswerAction::Choose {
            value: "stop".to_owned(),
        },
        false,
    ));
    let (asked, detail) = if turn.step.writes() {
        let (told, _) = fallback(ctx, &turn);
        ("Who carries it on?", vec![format!("They're told {told}.")])
    } else {
        ("Who answers instead?", Vec::new())
    };
    let id = ctx.next_choice_id();
    let mut choice = choices::app_choice(
        id.clone(),
        ChoiceKind::Fallback,
        format!("{from} {why}. {asked}"),
        detail,
        options,
    );
    choice.bell = true;
    let pending = PendingFallback {
        turn,
        tried,
        why,
        candidates: candidates
            .iter()
            .map(|candidate| candidate.worker.clone())
            .collect(),
    };
    if let Some((old, earlier)) = conversation.direct.fallback.replace((id, pending)) {
        ctx.send(AppEvent::ChoiceClosed {
            id: old,
            outcome: "Stopped".to_owned(),
        });
        // A turn that only answers already said it failed, and changed nothing to report.
        if earlier.turn.step.writes() {
            let error = format!("{} {}", earlier.turn.worker, earlier.why);
            lifecycle::end_direct(ctx, conversation, earlier.turn, error);
        }
    }
    ctx.send(AppEvent::Choice { choice });
}

fn answer_fallback(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    pending: PendingFallback,
    value: &str,
) -> anyhow::Result<Vec<Effect>> {
    let picked = pending
        .candidates
        .iter()
        .find(|worker| worker.to_string() == value)
        .cloned();
    let answers = !pending.turn.step.writes();
    let Some(to) = picked.filter(|_| conversation.turn.is_none()) else {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: "Stopped".to_owned(),
        });
        if answers {
            return Ok(send_queued(conversation));
        }
        let error = format!("{} {}", pending.turn.worker, pending.why);
        return Ok(lifecycle::end_direct(
            ctx,
            conversation,
            pending.turn,
            error,
        ));
    };
    let does = if answers { "answers" } else { "carries it on" };
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: format!("{} {does}", ctx.names.worker(&to)),
    });
    if answers {
        return lifecycle::answer_on(ctx, conversation, pending.turn, pending.tried, &to);
    }
    lifecycle::fallback(
        ctx,
        conversation,
        pending.turn,
        pending.tried,
        pending.why,
        to,
    )
}

/// Plan first in direct Captain: the turn reads and plans, and changes nothing until you say go
/// ahead with its plan.
pub(super) fn plan(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
    words: DirectWords,
) -> anyhow::Result<Vec<Effect>> {
    lifecycle::begin(ctx, conversation, text, |_| TurnStep::Plan { words })?;
    Ok(Vec::new())
}

/// What the session reads when you go ahead with its plan.
pub(super) const GO_AHEAD_WITH_PLAN: &str = "Go ahead with this plan.";

/// After a plan-first turn's plan: "Go ahead with this plan?". See [`offer_go_ahead`] for what
/// comes back.
pub(super) fn offer_plan(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: &Turn,
) -> Option<DirectWords> {
    match &turn.step {
        TurnStep::Plan { words } => offer_go_ahead(ctx, conversation, turn, words.clone()),
        _ => None,
    }
}

/// "Go ahead with this plan?" for `turn`'s request, whose reply was the plan. On autopilot
/// nothing waits for you: one line says the plan goes ahead, and `words` come back for the
/// caller to start the turn that carries it out, once this one has ended.
pub(super) fn offer_go_ahead(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: &Turn,
    words: DirectWords,
) -> Option<DirectWords> {
    let cli = ctx.names.cli(&turn.worker.agent);
    // An agent's session carried on works in the run's worktree, not your checkout.
    let folder = if conversation.folder(ctx) == ctx.env.repo_root.as_path() {
        "your checkout"
    } else {
        "the run's worktree"
    };
    if conversation.direct.approval == ApprovalMode::Autopilot {
        ctx.notice(
            NoteLevel::Info,
            format!("Decided on autopilot · {cli} goes ahead with its plan in {folder}."),
        );
        return Some(words);
    }
    let id = ctx.next_choice_id();
    let mut choice = choices::app_choice(
        id.clone(),
        ChoiceKind::GoAhead,
        "Go ahead with this plan?".to_owned(),
        vec![format!("{cli} carries it out in {folder}.")],
        choices::options(ChoiceKind::GoAhead),
    );
    choice.bell = true;
    conversation.direct.plans.insert(
        id,
        Planned {
            request: turn.text.clone(),
            words,
            cli,
        },
    );
    ctx.send(AppEvent::Choice { choice });
    None
}

fn answer_plan(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    planned: Planned,
    value: &str,
) -> anyhow::Result<Vec<Effect>> {
    if value != "yes" {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: "No · nothing changed".to_owned(),
        });
        return Ok(Vec::new());
    }
    // One turn at a time, and this one needs the session the plan was made in.
    if conversation.turn.is_some() {
        ctx.notice(
            NoteLevel::Info,
            "A turn is running. Answer the plan's question once it ends.",
        );
        conversation.direct.plans.insert(id, planned);
        return Ok(Vec::new());
    }
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: "Yes, go ahead".to_owned(),
    });
    go_ahead(
        ctx,
        conversation,
        planned.request,
        GO_AHEAD_WITH_PLAN,
        None,
        planned.words,
    )
}

/// Codex, agy, and OpenCode can't be asked before each edit in a headless run.
fn asks_each(worker: &str) -> bool {
    !matches!(worker, "codex" | ANTIGRAVITY | "opencode")
}

/// What happens instead of a direct turn whose CLI can't ask before each edit when every change
/// should be asked about: in your checkout, a checked run. An agent's session carried on waits
/// until you allow it to go on, since a checked worktree would start a new session.
fn instead(
    ctx: &mut Ctx,
    conversation: &ConversationState,
    asks: Asks,
    text: &str,
) -> Option<Vec<Effect>> {
    match &conversation.place {
        Some(place) if asks == Asks::EveryChange && !asks_each(&place.worker.agent) => {
            let cli = ctx.names.cli(&place.worker.agent);
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "{cli} can't ask before each edit in a headless run, and a checked worktree \
                     would leave this session behind. Auto mode, or captain.asks = \"like_cli\", \
                     lets it carry on."
                ),
            );
            Some(Vec::new())
        }
        Some(_) => None,
        None => checked_instead(ctx, asks, text).map(|run| vec![run]),
    }
}

/// One of the CLI's own commands, where the conversation works: your checkout, or the folder
/// of the agent's session it carries on.
pub(super) fn begin_agent(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    shown: String,
    prompt: String,
) -> anyhow::Result<Vec<Effect>> {
    let asks = mode_asks(ctx, conversation, &DirectWords::default());
    start(ctx, conversation, shown, asks, |asking| TurnStep::Cli {
        prompt,
        direct: Some(asking),
    })?;
    Ok(Vec::new())
}

/// Nothing in auto mode or autopilot, unless your words asked to be asked first, and what
/// `captain.asks` says otherwise. Words only ever add asking: none make a turn ask less than its
/// mode says.
fn mode_asks(ctx: &Ctx, conversation: &ConversationState, words: &DirectWords) -> Asks {
    match conversation.direct.approval {
        ApprovalMode::Auto | ApprovalMode::Autopilot if !words.supervised => Asks::Never,
        ApprovalMode::Auto
        | ApprovalMode::Autopilot
        | ApprovalMode::Supervised
        | ApprovalMode::AutoPreview => ctx.settings().captain.asks,
    }
}

/// Codex, agy, and OpenCode can't be asked before each edit in stage 1, so `every_change` runs
/// them in a checked worktree, and a line says why.
fn checked_instead(ctx: &Ctx, asks: Asks, text: &str) -> Option<Effect> {
    let main = &ctx.settings().crew.main;
    if asks != Asks::EveryChange || asks_each(&main.agent) {
        return None;
    }
    let cli = ctx.names.cli(&main.agent);
    ctx.notice(
        NoteLevel::Info,
        format!(
            "{cli} can't ask before each edit in a headless run, so this runs in a checked worktree."
        ),
    );
    let request = run_request(ctx, text.to_owned(), RunMode::Captain, None, None);
    Some(handover::start(request))
}

fn start(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: String,
    asks: Asks,
    step: impl FnOnce(Option<argonauts_core::session_runner::Asking>) -> TurnStep,
) -> anyhow::Result<()> {
    // Its folder is there before the snapshot `/rewind` takes of it.
    if !lifecycle::place_ready(ctx, conversation)? {
        return Ok(());
    }
    let settings = ctx.settings();
    // What the session may call to make something happen outside itself, and the standing
    // lines that tell it so.
    let offered = tools::offered(ctx, conversation);
    let lines = standing::lines(&offered);
    let rewind = settings.captain.rewind.then(|| max_bytes(settings));
    let next = TurnId::numbered(conversation.turns + 1);
    let roles = crate::controller::ask_role::roles(settings);
    let listener = if asks != Asks::Never || !offered.is_empty() || !roles.is_empty() {
        Some(listen::listen(ctx.internal.clone(), next)?)
    } else {
        None
    };
    let asking = listener.as_ref().map(|listener| {
        let mut asking = listener.asking(asks, offered, lines);
        asking.roles = roles;
        asking
    });
    let root = conversation.folder(ctx).to_path_buf();
    let watched = conversation.direct.watched.clone();
    // A folder with no git keeps nothing to go back to until `/git` turns its history on.
    let rewind = rewind.filter(|_| ctx.env.git().has_history() || root != ctx.env.repo_root);
    let before = rewind.and_then(|max| match rewind::take(&root, max, &watched) {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            ctx.notice(
                NoteLevel::Warn,
                format!("/rewind can't put back what this turn changes: {err:#}"),
            );
            None
        }
    });
    let unmade = unmade_files(ctx, &root);
    lifecycle::begin(ctx, conversation, text.clone(), |_| step(asking))?;
    if let Some(turn) = conversation.turn.as_mut() {
        turn.direct = Some(Running {
            text,
            listener,
            asks,
            before,
            commands: HashMap::new(),
            refused: Vec::new(),
            said: String::new(),
            watched,
            written: BTreeSet::new(),
            unmade,
            root,
            has_tools: turn.step.has_tools(),
            setup_wrote: BTreeSet::new(),
        });
    }
    Ok(())
}

/// The protected files that aren't in the turn's folder now, when our sandbox wraps the turn.
/// With it off the CLI works as when you start it, and nothing is protected.
fn unmade_files(ctx: &Ctx, root: &Path) -> Vec<String> {
    let sandboxed = !matches!(
        ctx.settings().sandbox.mode,
        SandboxChoice::Tool | SandboxChoice::None
    );
    PROTECTED_PATHS
        .iter()
        .filter(|path| sandboxed && !path.ends_with('/'))
        .filter(|path| std::fs::symlink_metadata(root.join(path)).is_err())
        .map(|path| (*path).to_owned())
        .collect()
}

/// A protected file the turn made anyway, where the sandbox couldn't hold it, is moved into the
/// turn's own folder, where nothing loads it, and a line says where.
fn set_aside_planted(
    ctx: &Ctx,
    conversation: &ConversationState,
    running: &Running,
    turn: &TurnId,
) {
    let root = &running.root;
    let made: Vec<&String> = running
        .unmade
        .iter()
        .filter(|path| std::fs::symlink_metadata(root.join(path)).is_ok())
        .collect();
    if made.is_empty() {
        return;
    }
    let aside = ctx
        .env
        .conversation_dir()
        .join(conversation.id.as_str())
        .join(turn.as_str())
        .join("set-aside");
    for path in made {
        let moved = std::fs::create_dir_all(&aside)
            .and_then(|()| std::fs::rename(root.join(path), aside.join(path)));
        match moved {
            Ok(()) => ctx.notice(
                NoteLevel::Warn,
                format!(
                    "This turn made {path}, which configures agents, so it's set aside in {} where \
                     nothing loads it. Move it back yourself if you want it.",
                    aside.display()
                ),
            ),
            Err(err) => ctx.notice(
                NoteLevel::Warn,
                format!("This turn made {path}, which configures agents, and it couldn't be set aside: {err}. Look it over before any agent runs here."),
            ),
        }
    }
}

/// Notes a direct turn's prompt carries after your message: what `/rewind` put back, and for a
/// session with Argonauts Code's tools, the setup in use when it hasn't heard it. Argonauts Code
/// adds no instructions of its own beyond those.
pub(super) fn notes(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    step: &TurnStep,
    fresh: bool,
) -> Vec<InputPart> {
    let mut notes: Vec<InputPart> = conversation
        .direct
        .rewound
        .take()
        .map(|text| part(InputPartKind::Notes, "rewind", text))
        .into_iter()
        .collect();
    if step.has_tools() {
        notes.extend(standing::setup_note(ctx, conversation, fresh));
    }
    notes
}

/// Follows the turn's events for what comes after it: agy's refused commands, and what the
/// agent said, for a fallback.
pub(super) fn on_agent(running: &mut Running, event: &AgentEvent) {
    match event {
        AgentEvent::ToolStarted {
            id, name, input, ..
        } => {
            let command = ["CommandLine", "command", "cmd"]
                .iter()
                .find_map(|key| input.get(*key).and_then(Value::as_str));
            if let Some(command) = command {
                running.commands.insert(id.clone(), command.to_owned());
            }
            let written = written_path(&running.root, name, input);
            running.written.extend(written);
        }
        AgentEvent::ToolFinished {
            id,
            ok: false,
            output,
            parent: None,
        } => {
            let refused = output.as_deref().is_some_and(|output| {
                output.contains("permission check failed")
                    || output.contains("required approval")
                    || output.contains("auto-denied")
            });
            if let Some(command) = running.commands.get(id).filter(|_| refused)
                && !running.refused.contains(command)
            {
                running.refused.push(command.clone());
            }
        }
        AgentEvent::TextDelta { text, parent: None } => running.said.push_str(text),
        AgentEvent::Message { text, parent: None } if running.said.is_empty() => {
            running.said.push_str(text);
        }
        _ => {}
    }
}

/// The file a tool call writes, by path from your folder: an edit's or a write's, whatever the
/// CLI calls the tool. One outside your folder isn't `/rewind`'s.
fn written_path(root: &std::path::Path, name: &str, input: &Value) -> Option<String> {
    let name = name.to_lowercase();
    let writes = ["edit", "write", "patch", "replace", "create", "notebook"]
        .iter()
        .any(|verb| name.contains(verb));
    if !writes {
        return None;
    }
    let path = [
        "file_path",
        "filePath",
        "notebook_path",
        "path",
        "TargetFile",
        "target_file",
    ]
    .iter()
    .find_map(|key| input.get(*key).and_then(Value::as_str))?;
    let path = std::path::Path::new(path);
    // The CLI may name your folder by its real path, such as /private/var for /var.
    let real = root.canonicalize().ok();
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .ok()
            .or_else(|| path.strip_prefix(real.as_deref()?).ok())?
    } else {
        path
    };
    let relative = relative.to_str()?.trim_start_matches("./");
    (!relative.is_empty() && !relative.starts_with("..")).then(|| relative.to_owned())
}

/// After the turn's answer: the snapshot for `/rewind`, then agy's refused commands, offered.
pub(super) fn finished(ctx: &mut Ctx, conversation: &mut ConversationState, turn: &mut Turn) {
    let Some(mut running) = turn.direct.take() else {
        return;
    };
    close_asks(ctx, conversation, &turn.id);
    set_aside_planted(ctx, conversation, &running, &turn.id);
    record(ctx, conversation, &mut running);
    let agy = turn.worker.agent == ANTIGRAVITY;
    if agy && running.asks != Asks::Never && !running.refused.is_empty() {
        offer_refused(ctx, conversation, turn, &running.refused);
    }
}

/// A turn that ended without its answer, such as at the limit or a crash: one line says
/// what it changed, so the end is never silent.
pub(super) fn failed(ctx: &mut Ctx, conversation: &mut ConversationState, turn: &mut Turn) {
    let Some(mut running) = turn.direct.take() else {
        return;
    };
    close_asks(ctx, conversation, &turn.id);
    set_aside_planted(ctx, conversation, &running, &turn.id);
    let changed = record(ctx, conversation, &mut running);
    match changed {
        Some(summary) => ctx.notice(NoteLevel::Info, format!("Stopped · {summary}")),
        None => ctx.notice(NoteLevel::Info, "Stopped · nothing changed"),
    }
}

/// Esc: the turn is stopping, and its files are counted once its session ends.
pub(super) fn cancelled(ctx: &mut Ctx, conversation: &mut ConversationState, turn: &mut Turn) {
    close_asks(ctx, conversation, &turn.id);
    if let Some(mut running) = turn.direct.take() {
        running.listener = None;
        conversation
            .direct
            .stopping
            .insert(turn.id.clone(), running);
    }
}

/// A stopped turn's session ended: what it changed, and `/rewind`.
pub(super) fn stopped(ctx: &mut Ctx, conversation: &mut ConversationState, id: &TurnId) {
    let Some(mut running) = conversation.direct.stopping.remove(id) else {
        return;
    };
    set_aside_planted(ctx, conversation, &running, id);
    if let Some(summary) = record(ctx, conversation, &mut running) {
        ctx.notice(NoteLevel::Info, summary);
    }
}

/// What the next agent is told when this one runs out, as a note in its prompt and as the
/// `↷` line says it: the files changed so far, and what the last agent said.
pub(super) fn fallback(ctx: &Ctx, turn: &Turn) -> (String, Option<InputPart>) {
    let Some(running) = turn.direct.as_ref() else {
        return ("your request".to_owned(), None);
    };
    let root = &running.root;
    let max = max_bytes(ctx.settings());
    let changed = running
        .before
        .as_ref()
        .and_then(|before| {
            let keep = running.watched.union(&running.written).cloned().collect();
            let now = rewind::take(root, max, &keep).ok()?;
            let mut changed = rewind::changed(root, before, &now).ok()?;
            changed.extend(rewind::unkept(root, before, &running.written, max));
            changed.sort();
            changed.dedup();
            Some(changed)
        })
        .unwrap_or_default();
    let told = match changed.as_slice() {
        [] => "your request and the transcript so far".to_owned(),
        [one] => format!("your request, the transcript so far, and that {one} changed"),
        many => format!(
            "your request, the transcript so far, and that {} files changed",
            many.len()
        ),
    };
    let mut text = String::from(
        "Another agent started on this request in this folder and stopped before it finished. \
         Carry on from where it left off.",
    );
    if !changed.is_empty() {
        text.push_str(&format!(
            "\n\nFiles it changed so far: {}.",
            changed.join(", ")
        ));
    }
    if !running.said.trim().is_empty() {
        text.push_str(&format!("\n\nWhat it said:\n{}", running.said.trim()));
    }
    (told, Some(part(InputPartKind::Fallback, "fallback", text)))
}

/// The snapshot after the turn, kept when the turn changed something, and a line saying what
/// changed and what `/rewind` can put back. `None` when it changed nothing or there was no
/// snapshot.
fn record(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    running: &mut Running,
) -> Option<String> {
    running.listener = None;
    let before = running.before.take()?;
    let root = &running.root;
    let max = max_bytes(ctx.settings());
    // Ignored files the turn wrote are kept from now on, so a later turn's can be put back.
    for path in &running.written {
        if rewind::ignored(root, path) {
            conversation.direct.watched.insert(path.clone());
        }
    }
    let after = rewind::take(root, max, &conversation.direct.watched)
        .inspect_err(|err| {
            ctx.notice(
                NoteLevel::Warn,
                format!("/rewind can't put back what this turn changed: {err:#}"),
            );
        })
        .ok()?;
    let unkept = rewind::unkept(root, &before, &running.written, max);
    let mut changed = rewind::changed(root, &before, &after).ok()?;
    changed.extend(unkept.iter().cloned());
    changed.sort();
    changed.dedup();
    if changed.is_empty() {
        return None;
    }
    keep_setup_files(ctx, running, &before, &after, &changed, max);
    // Such as "app.py (+1 -1) changed · /rewind puts it back, except .env".
    let mut summary = rewind::summary(root, &before, &after, &changed);
    summary.push_str(" · /rewind puts it back");
    match unkept.as_slice() {
        [] => {}
        [one] => summary.push_str(&format!(", except {one}")),
        [rest @ .., last] => summary.push_str(&format!(", except {} and {last}", rest.join(", "))),
    }
    conversation.direct.turns.push(TurnRecord {
        text: running.text.clone(),
        before,
        after,
        changed,
    });
    Some(summary)
}

/// Notes the files a setup you answered wrote in the turn `turn`, so its end keeps them.
pub(in crate::controller::conversation) fn setup_wrote(
    conversation: &mut super::ConversationState,
    turn: &TurnId,
    paths: &[PathBuf],
) {
    let Some(running) = conversation
        .turn
        .as_mut()
        .filter(|running| &running.id == turn)
        .and_then(|turn| turn.direct.as_mut())
    else {
        return;
    };
    let root = running.root.clone();
    let from_root = paths
        .iter()
        .filter_map(|path| path.strip_prefix(&root).ok())
        .filter_map(|path| path.to_str().map(str::to_owned));
    running.setup_wrote.extend(from_root);
}

/// Whether one of the turn's shell commands names `path` in `.argonauts`: the path itself, or
/// the folder and the file's name, as `cd .argonauts && sed -i … config.toml` does.
fn named_by_command(running: &Running, path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    running
        .commands
        .values()
        .any(|command| ask::names_setup_folder(command) && command.contains(name))
}

/// The main session never edits Argonauts Code's own files: a setup changes them, after you pick
/// where. Our sandbox keeps `.argonauts/` closed to a direct turn. With it off,
/// what the turn wrote there goes back at once: its own edits, and the files a shell command
/// named, unless a setup you answered wrote them. Anything else that changed there is named,
/// unless the session has Argonauts Code's tools, whose setups you answered write there.
fn keep_setup_files(
    ctx: &Ctx,
    running: &Running,
    before: &rewind::Snapshot,
    after: &rewind::Snapshot,
    changed: &[String],
    max: u64,
) {
    if !ctx.settings().sandbox.mode.ours_off() {
        return;
    }
    let (edited, other): (Vec<String>, Vec<String>) = changed
        .iter()
        .filter(|path| ask::in_setup_folder(path))
        .filter(|path| !running.setup_wrote.contains(*path))
        .cloned()
        .partition(|path| running.written.contains(path) || named_by_command(running, path));
    if !edited.is_empty() {
        match rewind::restore(&running.root, before, after, &edited, max) {
            Ok(restored) if !restored.put_back.is_empty() => ctx.notice(
                NoteLevel::Warn,
                format!(
                    "This turn edited {}, which only a setup changes, so it's put back. Ask for \
                     the change in words and a question asks where to keep it.",
                    restored.put_back.join(", ")
                ),
            ),
            Ok(_) => {}
            Err(err) => ctx.notice(
                NoteLevel::Warn,
                format!(
                    "This turn edited Argonauts Code's own files and they couldn't be put back: {err:#}"
                ),
            ),
        }
    }
    if !other.is_empty() && !running.has_tools {
        ctx.notice(
            NoteLevel::Warn,
            format!(
                "{} changed during this turn. Only a setup should change Argonauts Code's files; \
                 /rewind puts them back.",
                other.join(", ")
            ),
        );
    }
}

fn max_bytes(settings: &Settings) -> u64 {
    settings.captain.rewind_max_mb.saturating_mul(1024 * 1024)
}

/// A question from the turn's session: answered at once when you already allowed it,
/// otherwise shown to you.
pub(crate) fn on_asked(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: &TurnId,
    asked: Asked,
) -> anyhow::Result<Vec<Effect>> {
    let current = conversation
        .turn
        .as_ref()
        .filter(|current| &current.id == turn);
    let Some(current) = current else {
        let _ = asked
            .reply
            .send(AskReply::deny("That turn is over, so nothing runs for it."));
        return Ok(Vec::new());
    };
    match asked.ask {
        Ask::Permission {
            tool_name, input, ..
        } => {
            let patterns = &ctx.settings().approvals.allow_commands;
            if ask::allowed(
                &tool_name,
                &input,
                conversation.direct.edits_allowed,
                patterns,
            ) {
                let _ = asked.reply.send(AskReply::allow(&input));
                return Ok(Vec::new());
            }
            // Autopilot came on after this turn's CLI was told to ask.
            if conversation.direct.approval == ApprovalMode::Autopilot && !asked_by_words(current) {
                let _ = asked.reply.send(on_autopilot(&tool_name, &input));
                return Ok(Vec::new());
            }
            let cli = ctx.names.cli(&current.worker.agent);
            let question = ask::Question::of(&tool_name, &input);
            let id = ctx.next_choice_id();
            let mut choice = choices::app_choice(
                id.clone(),
                ChoiceKind::Permission,
                question.title(&cli, &tool_name),
                question.body.clone(),
                question.options(&cli),
            );
            choice.bell = true;
            conversation.direct.asks.insert(
                id,
                PendingAsk {
                    turn: turn.clone(),
                    tool: tool_name,
                    input,
                    question,
                    reply: asked.reply,
                },
            );
            ctx.send(AppEvent::Choice { choice });
            Ok(Vec::new())
        }
        Ask::Tool { name, arguments } => {
            let call = tools::Call {
                turn: turn.clone(),
                arguments,
                reply: asked.reply,
            };
            Ok(tools::call(ctx, conversation, &name, call))
        }
        Ask::Role { role, what, .. } => {
            let role_asked = crate::controller::ask_role::Asked {
                role,
                what,
                request: current.text.clone(),
                folder: conversation.folder(ctx).to_path_buf(),
            };
            crate::controller::ask_role::answer(ctx, role_asked, asked.reply);
            Ok(Vec::new())
        }
    }
}

/// Your own words had this turn ask you first, which stands whatever the mode on screen.
fn asked_by_words(turn: &Turn) -> bool {
    matches!(&turn.step, TurnStep::Direct { words, .. } if words.supervised)
}

/// Autopilot's answer to what a turn's CLI asks: yes, or a no it carries on from.
fn on_autopilot(tool: &str, input: &Value) -> AskReply {
    if ask::autopilot_allows(tool, input) {
        AskReply::allow(input)
    } else {
        ask::not_on_autopilot()
    }
}

/// Autopilot came on with questions open, and nothing waits for you there. What a turn's CLI
/// asked gets autopilot's answer, unless your own words had that turn ask you first. A turn
/// waiting for who carries it on goes to the first agent that may take it, a plan made first
/// goes ahead, and agy runs what it was refused, as a turn started on autopilot would have.
pub(super) fn settle_on_autopilot(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    settle_asks(ctx, conversation);
    let mut effects = settle_fallback(ctx, conversation)?;
    effects.extend(settle_plans(ctx, conversation)?);
    effects.extend(settle_refused(ctx, conversation)?);
    Ok(effects)
}

fn settle_asks(ctx: &Ctx, conversation: &mut ConversationState) {
    let asks_you = conversation
        .turn
        .as_ref()
        .filter(|turn| asked_by_words(turn))
        .map(|turn| turn.id.clone());
    let mut open: Vec<ChoiceId> = conversation
        .direct
        .asks
        .iter()
        .filter(|(_, pending)| Some(&pending.turn) != asks_you.as_ref())
        .map(|(id, _)| id.clone())
        .collect();
    open.sort();
    for id in open {
        let Some(pending) = conversation.direct.asks.remove(&id) else {
            continue;
        };
        let about = &pending.question.about;
        let outcome = if ask::autopilot_allows(&pending.tool, &pending.input) {
            format!("Decided on autopilot · yes to {about}")
        } else {
            format!("Decided on autopilot · no to {about}, which is asked about every time")
        };
        let _ = pending
            .reply
            .send(on_autopilot(&pending.tool, &pending.input));
        ctx.send(AppEvent::ChoiceClosed { id, outcome });
    }
}

/// The turn waiting on "Who carries it on?" or "Who answers instead?" goes to the first agent
/// that may take it by itself, and stops where only agents billed to an API key are left, as
/// one that ran out on autopilot does.
fn settle_fallback(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    let Some((id, pending)) = conversation.direct.fallback.take() else {
        return Ok(Vec::new());
    };
    // One turn at a time, and another may have taken its place while the question was open.
    let next = if conversation.turn.is_some() {
        Err("Decided on autopilot · another turn is running, so it stops here.")
    } else {
        shortlist(ctx, &pending.turn.worker, &pending.tried)
            .into_iter()
            .find(|candidate| auto_check(candidate).is_ok())
            .map(|candidate| candidate.worker)
            .ok_or(lifecycle::PAID_ONLY_ON_AUTOPILOT)
    };
    let answers = !pending.turn.step.writes();
    match next {
        Ok(to) => {
            let does = if answers { "answers" } else { "carries it on" };
            ctx.send(AppEvent::ChoiceClosed {
                id,
                outcome: format!("Decided on autopilot · {} {does}", ctx.names.worker(&to)),
            });
            if answers {
                return lifecycle::answer_on(ctx, conversation, pending.turn, pending.tried, &to);
            }
            lifecycle::fallback(
                ctx,
                conversation,
                pending.turn,
                pending.tried,
                pending.why,
                to,
            )
        }
        Err(why_not) => {
            ctx.send(AppEvent::ChoiceClosed {
                id,
                outcome: why_not.to_owned(),
            });
            // A turn that only answers already said it failed, and changed nothing to report.
            if answers {
                return Ok(send_queued(conversation));
            }
            let error = format!("{} {}", pending.turn.worker, pending.why);
            Ok(lifecycle::end_direct(
                ctx,
                conversation,
                pending.turn,
                error,
            ))
        }
    }
}

/// A plan waiting on "Go ahead with this plan?" goes ahead, as one made on autopilot does. The
/// session that planned carries it out, one turn at a time, so a plan that can't have its
/// turn now closes.
fn settle_plans(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    let mut open: Vec<ChoiceId> = conversation.direct.plans.keys().cloned().collect();
    open.sort();
    let mut effects = Vec::new();
    for id in open {
        let Some(planned) = conversation.direct.plans.remove(&id) else {
            continue;
        };
        if conversation.turn.is_some() {
            ctx.send(AppEvent::ChoiceClosed {
                id,
                outcome: "Decided on autopilot · not carried out, since a turn is running. \
                          Ask again to have it done."
                    .to_owned(),
            });
            continue;
        }
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: format!(
                "Decided on autopilot · {} goes ahead with its plan",
                planned.cli
            ),
        });
        effects.extend(go_ahead(
            ctx,
            conversation,
            planned.request,
            GO_AHEAD_WITH_PLAN,
            None,
            planned.words,
        )?);
    }
    Ok(effects)
}

/// agy's offer to run what it was refused gets a yes, as a turn started on autopilot runs
/// commands unasked in our sandbox. The grant is for one turn of agy's own session, so an offer
/// that can't have it now closes.
fn settle_refused(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    let mut open: Vec<ChoiceId> = conversation.direct.refused.keys().cloned().collect();
    open.sort();
    let mut effects = Vec::new();
    for id in open {
        let Some(offer) = conversation.direct.refused.remove(&id) else {
            continue;
        };
        if conversation.turn.is_some() {
            ctx.send(AppEvent::ChoiceClosed {
                id,
                outcome: "Decided on autopilot · not run, since a turn is running. Ask again \
                          to have them run."
                    .to_owned(),
            });
            continue;
        }
        let cli = ctx.names.cli(&offer.worker.agent);
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: format!(
                "Decided on autopilot · {cli} continues, running commands without asking"
            ),
        });
        effects.extend(go_ahead(
            ctx,
            conversation,
            offer.request,
            RUN_REFUSED,
            Some(Asks::Never),
            DirectWords::default(),
        )?);
    }
    Ok(effects)
}

/// A question of `turn`'s is open, so its CLI waits on you in silence.
pub(super) fn waits_on_you(conversation: &ConversationState, turn: &TurnId) -> bool {
    conversation
        .direct
        .asks
        .values()
        .any(|ask| &ask.turn == turn)
}

pub(crate) fn owns_choice(conversation: &ConversationState, id: &ChoiceId) -> bool {
    conversation.direct.asks.contains_key(id)
        || conversation.direct.refused.contains_key(id)
        || conversation.direct.plans.contains_key(id)
        || conversation.direct.rewinding.as_ref() == Some(id)
        || conversation
            .direct
            .fallback
            .as_ref()
            .is_some_and(|(open, _)| open == id)
}

pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    action: &AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    let value = match action {
        AnswerAction::Choose { value } => value.as_str(),
        AnswerAction::Cancel => "no",
        other => anyhow::bail!("{other:?} doesn't answer this question"),
    };
    if let Some(pending) = conversation.direct.asks.remove(&id) {
        return answer_ask(ctx, conversation, id, pending, value);
    }
    if let Some(offer) = conversation.direct.refused.remove(&id) {
        return answer_refused(ctx, conversation, id, offer, value);
    }
    if let Some(planned) = conversation.direct.plans.remove(&id) {
        return answer_plan(ctx, conversation, id, planned, value);
    }
    if conversation
        .direct
        .fallback
        .as_ref()
        .is_some_and(|(open, _)| open == &id)
        && let Some((_, pending)) = conversation.direct.fallback.take()
    {
        return answer_fallback(ctx, conversation, id, pending, value);
    }
    if conversation.direct.rewinding.as_ref() == Some(&id) {
        return rewind_to(ctx, conversation, id, value);
    }
    ctx.notice(NoteLevel::Info, "That question is already closed.");
    Ok(Vec::new())
}

/// Esc: No to a question, the `/rewind` question closed, and agy's offer left waiting.
pub(crate) fn set_aside(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
) -> anyhow::Result<Vec<Effect>> {
    if conversation.direct.refused.contains_key(&id) {
        return Ok(Vec::new());
    }
    answer_choice(ctx, conversation, id, &AnswerAction::Cancel)
}

fn answer_ask(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    pending: PendingAsk,
    value: &str,
) -> anyhow::Result<Vec<Effect>> {
    let mut effects = Vec::new();
    // Such as "Yes to the edit to notes.txt", since the question's own text goes when it
    // closes.
    let about = pending.question.about.clone();
    let outcome = match value {
        "yes" => {
            let _ = pending.reply.send(AskReply::allow(&pending.input));
            format!("Yes to {about}")
        }
        "always" => {
            effects = grant(ctx, conversation, &pending.question.grant)?;
            // What's now allowed answers the questions already waiting on it.
            let _ = pending.reply.send(AskReply::allow(&pending.input));
            answer_waiting(ctx, conversation);
            format!("Yes to {about}, and don't ask again")
        }
        "no" => {
            let _ = pending.reply.send(ask::no());
            // As in Claude Code: No stops the turn, and your next message goes to the agent.
            if conversation
                .turn
                .as_ref()
                .is_some_and(|turn| turn.id == pending.turn)
            {
                effects.extend(super::cancel_turn(ctx, conversation)?);
            }
            format!("No to {about}")
        }
        other => {
            conversation.direct.asks.insert(id, pending);
            anyhow::bail!("{other:?} isn't one of this question's answers");
        }
    };
    ctx.send(AppEvent::ChoiceClosed { id, outcome });
    Ok(effects)
}

/// Saves what "don't ask again" covers: edits for this conversation, or a pattern at "This
/// project, just me", which you set by answering, as with any safety confirm.
fn grant(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    grant: &ask::Grant,
) -> anyhow::Result<Vec<Effect>> {
    let pattern = match grant {
        ask::Grant::Edits => {
            conversation.direct.edits_allowed = true;
            return Ok(Vec::new());
        }
        ask::Grant::Commands(prefix) => ask::command_pattern(prefix),
        ask::Grant::Tool(tool) => tool.clone(),
        ask::Grant::Never => return Ok(Vec::new()),
    };
    let mut patterns = ctx.settings().approvals.allow_commands.clone();
    if patterns.contains(&pattern) {
        return Ok(Vec::new());
    }
    patterns.push(pattern);
    let value = toml::Value::Array(patterns.into_iter().map(toml::Value::String).collect());
    crate::controller::settings::save_answered(
        ctx,
        "approvals.allow_commands",
        value.to_string(),
        SettingScope::ProjectLocal,
    )
}

/// Questions still open that your last answer now allows.
fn answer_waiting(ctx: &mut Ctx, conversation: &mut ConversationState) {
    let patterns = ctx.settings().approvals.allow_commands.clone();
    let edits = conversation.direct.edits_allowed;
    let allowed: Vec<ChoiceId> = conversation
        .direct
        .asks
        .iter()
        .filter(|(_, pending)| ask::allowed(&pending.tool, &pending.input, edits, &patterns))
        .map(|(id, _)| id.clone())
        .collect();
    for id in allowed {
        if let Some(pending) = conversation.direct.asks.remove(&id) {
            let _ = pending.reply.send(AskReply::allow(&pending.input));
            ctx.send(AppEvent::ChoiceClosed {
                id,
                outcome: "Yes".to_owned(),
            });
        }
    }
}

/// The turn's open questions close with it, each a no.
fn close_asks(ctx: &Ctx, conversation: &mut ConversationState, turn: &TurnId) {
    let ids: Vec<ChoiceId> = conversation
        .direct
        .asks
        .iter()
        .filter(|(_, pending)| &pending.turn == turn)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        conversation.direct.asks.remove(&id);
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: "The turn ended.".to_owned(),
        });
    }
}

fn offer_refused(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: &Turn,
    commands: &[String],
) {
    let cli = ctx.names.cli(&turn.worker.agent);
    let id = ctx.next_choice_id();
    let mut options = choices::options(ChoiceKind::RefusedCommands);
    if let Some(first) = options.first_mut() {
        first.detail = Some(
            "one more turn in which agy runs commands without asking, in our sandbox".to_owned(),
        );
    }
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::RefusedCommands,
        format!("Let {cli} run what it was refused?"),
        commands.to_vec(),
        options,
    );
    conversation.direct.refused.insert(
        id,
        RefusedOffer {
            worker: turn.worker.clone(),
            request: turn.text.clone(),
        },
    );
    ctx.send(AppEvent::Choice { choice });
}

/// "Yes, and continue" resumes agy for one turn with `--dangerously-skip-permissions`, inside
/// our sandbox: permission rules can't be set for one agy run, so this is the narrowest grant.
fn answer_refused(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    offer: RefusedOffer,
    value: &str,
) -> anyhow::Result<Vec<Effect>> {
    if value != "continue" {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: "No".to_owned(),
        });
        return Ok(Vec::new());
    }
    // The grant is for one turn of agy's own session, so it waits for the turn that's running.
    if conversation.turn.is_some() {
        ctx.notice(
            NoteLevel::Info,
            "A turn is running. Answer agy's question once it ends.",
        );
        conversation.direct.refused.insert(id, offer);
        return Ok(Vec::new());
    }
    let cli = ctx.names.cli(&offer.worker.agent);
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: format!("{cli} continues, running commands without asking"),
    });
    go_ahead(
        ctx,
        conversation,
        offer.request,
        RUN_REFUSED,
        Some(Asks::Never),
        DirectWords::default(),
    )
}

/// `/rewind` in a direct conversation: your messages whose turns changed files, newest first.
pub(crate) fn rewind(ctx: &mut Ctx, conversation: &mut ConversationState) -> bool {
    if conversation.direct.turns.is_empty() {
        if !ctx.env.git().has_history() && conversation.place.is_none() {
            ctx.notice(
                NoteLevel::Info,
                "This folder has no git, so nothing was kept to go back to. /git turns on a \
                 hidden history, and /rewind works from then on.",
            );
            return true;
        }
        return false;
    }
    let id = ctx.next_choice_id();
    let newest = conversation.direct.turns.len() - 1;
    let options = conversation
        .direct
        .turns
        .iter()
        .enumerate()
        .rev()
        .zip('a'..='z')
        .map(|((index, record), key)| {
            let mut option = option(
                key,
                &crate::controller::recap::title_from_request(&record.text),
                choices::choose(&index.to_string()),
                index == newest,
            );
            option.detail = Some(match record.changed.len() {
                1 => record.changed[0].clone(),
                n => format!("{n} files"),
            });
            option
        })
        .collect();
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::RewindTurn,
        "Put back the files changed since which message?",
        vec!["Files you changed since stay as you left them.".to_owned()],
        options,
    );
    conversation.direct.rewinding = Some(id);
    ctx.send(AppEvent::Choice { choice });
    true
}

fn rewind_to(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    value: &str,
) -> anyhow::Result<Vec<Effect>> {
    conversation.direct.rewinding = None;
    let Ok(from) = value.parse::<usize>() else {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: choices::NOT_CHANGED.to_owned(),
        });
        return Ok(Vec::new());
    };
    let (Some(target), Some(last)) = (
        conversation.direct.turns.get(from),
        conversation.direct.turns.last(),
    ) else {
        anyhow::bail!("that message's files can't be put back any more");
    };
    let mut paths: Vec<String> = conversation.direct.turns[from..]
        .iter()
        .flat_map(|record| record.changed.iter().cloned())
        .collect();
    paths.sort();
    paths.dedup();
    let root = conversation.folder(ctx).to_path_buf();
    let restored = rewind::restore(
        &root,
        &target.before,
        &last.after,
        &paths,
        max_bytes(ctx.settings()),
    )?;
    let asked = target.text.clone();
    conversation.direct.turns.truncate(from);
    let mut outcome = match restored.put_back.len() {
        0 => "Nothing to put back".to_owned(),
        1 => format!("Put back {}", restored.put_back[0]),
        n => format!("Put back {n} files"),
    };
    if !restored.kept.is_empty() {
        outcome.push_str(&format!(
            " · kept as you changed them: {}",
            restored.kept.join(", ")
        ));
    }
    if !restored.cant.is_empty() {
        outcome.push_str(&format!(
            " · can't put back, as nothing kept how they were: {}",
            restored.cant.join(", ")
        ));
    }
    ctx.send(AppEvent::ChoiceClosed { id, outcome });
    if !restored.put_back.is_empty() {
        conversation.direct.rewound = Some(format!(
            "The user put back these files to how they were before \"{}\": {}.",
            asked,
            restored.put_back.join(", ")
        ));
    }
    Ok(Vec::new())
}
