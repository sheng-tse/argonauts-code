//! Direct solo: solo you choose is a turn of the main session in your checkout,
//! as its CLI works when you run it. Your words go through as you typed them, the CLI's own
//! tools and agents do the work, and its last message is the end. No run, card, report, gates,
//! merge, retro, or recipe.
//!
//! Around the turn Argonauts keeps the sandbox, the budget per message, and handoff, and adds
//! what the CLI would have you answer in its own window: `ask` builds the cards, `listen` takes
//! the questions in, and `rewind` keeps a snapshot before and after each turn so `/rewind` can
//! put files back.

mod ask;
mod listen;
mod rewind;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use argonauts_conductor::sandbox::profile::PROTECTED_PATHS;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::asking::{Ask, AskReply};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::{RunMode, TriageDecision};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::{ChoiceId, TurnId, WorkerRef};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::settings::protocol::{ForcedMode, SoloStyle};
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::steps::intake::{Instruction, InstructionKind};
use argonauts_core::{Asks, NoteLevel};
use serde_json::Value;

pub(crate) use self::listen::Asked;
use self::listen::Listener;
use self::rewind::Snapshot;
use super::{ChatState, Turn, lifecycle, run_request};
use crate::choices::{self, option};
use crate::controller::instructions::{self, Applied, Roster};
use crate::controller::turn::{DirectWords, TurnStep, part};
use crate::controller::{Ctx, Effect};
use argonauts_conductor::handoff::shortlist::{Sources, shortlist as handoff_shortlist};
use argonauts_core::CardId;
use argonauts_core::events::card::{CardBudget, CardKind, CardSpec};
use argonauts_core::limits::{Candidate, CandidateQuery, TrackRecord, TrackRecords};
use argonauts_core::names::RoleName;
use argonauts_core::settings::handoff::HandoffSettings;

const ANTIGRAVITY: &str = "antigravity-cli";

/// What agy's resumed turn is told when you let it run what it was refused.
const RUN_REFUSED: &str = "You may run the commands you were refused.";

/// Direct solo's part of the conversation.
#[derive(Debug, Default)]
pub(crate) struct DirectState {
    /// The approval mode on screen, which says whether a direct turn asks.
    approval: ApprovalMode,
    /// Open questions from the turn's CLI, by their card.
    asks: HashMap<ChoiceId, PendingAsk>,
    /// "Yes, and allow edits for the rest of this conversation".
    edits_allowed: bool,
    /// Each direct turn that changed files, oldest first.
    turns: Vec<TurnRecord>,
    /// Stopped turns whose session hasn't ended yet, so their files may still change.
    stopping: HashMap<TurnId, Running>,
    /// agy's refused commands, offered after its turn.
    refused: HashMap<ChoiceId, RefusedOffer>,
    /// The open `/rewind` card. Its answers name turns by their place in `turns`.
    rewinding: Option<ChoiceId>,
    /// Said with your next message: the files `/rewind` put back.
    rewound: Option<String>,
    /// Plans made with plan first on, waiting for "Go ahead with this plan?".
    plans: HashMap<ChoiceId, Planned>,
    /// A turn whose agent ran out, waiting for you to pick who carries it on.
    handoff: Option<(ChoiceId, PendingHandoff)>,
    /// Ignored files turns wrote, which snapshots keep from then on so `/rewind` can put them
    /// back.
    watched: BTreeSet<String>,
}

impl std::fmt::Debug for PendingHandoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingHandoff")
            .field("turn", &self.turn.id)
            .field("why", &self.why)
            .field("candidates", &self.candidates)
            .finish_non_exhaustive()
    }
}

/// A direct turn that ran out, held while the handoff card is open.
struct PendingHandoff {
    turn: Turn,
    tried: Vec<WorkerRef>,
    why: String,
    /// In the card's order.
    candidates: Vec<WorkerRef>,
}

/// A plan-first turn's request, for the turn that goes ahead with it.
#[derive(Debug)]
struct Planned {
    request: String,
    words: DirectWords,
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
    /// What the agent said so far, for whoever takes over.
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
    worker: WorkerRef,
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

/// Solo you choose runs direct: `solo.style` says so, and nothing that needs a copy is on.
pub(crate) fn solo_is_direct(settings: &Settings) -> bool {
    settings.solo.direct()
}

/// What a direct turn after the estimate says, so the agent starts from the plan it wrote.
pub(super) const GO_AHEAD: &str = "Go ahead.";

/// Triage settled on direct solo: solo your words chose where `solo.style` is direct, or solo
/// the rules picked where `triage.solo_style` says so, with nothing on that needs a copy.
pub(super) fn triage_picks_direct(triage: &TriageDecision) -> bool {
    triage.mode == RunMode::Solo && triage.solo == Some(SoloStyle::Direct)
}

/// The solo preset sends every message to the main session as a direct turn.
pub(crate) fn preset_is_direct(settings: &Settings) -> bool {
    settings.triage.force == Some(ForcedMode::Solo) && solo_is_direct(settings)
}

pub(crate) fn set_approval(chat: &mut ChatState, mode: ApprovalMode) {
    chat.direct.approval = mode;
}

/// Starts a direct turn with your message. `asks` overrides what the mode says, as agy's
/// resumed turn does. A CLI that can't ask before each edit gets a checked run instead, when
/// `solo.asks` wants every change asked about.
pub(super) fn begin(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
    asks: Option<Asks>,
) -> anyhow::Result<Vec<Effect>> {
    begin_with(ctx, chat, text, asks, DirectWords::default())
}

/// [`begin`], with what your checked words change about the turn.
pub(super) fn begin_with(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
    asks: Option<Asks>,
    words: DirectWords,
) -> anyhow::Result<Vec<Effect>> {
    let asks = asks.unwrap_or_else(|| mode_asks(ctx, chat, &words));
    if let Some(instead) = instead(ctx, chat, asks, &text) {
        return Ok(instead);
    }
    start(ctx, chat, text, asks, |asking| TurnStep::Direct {
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
    chat: &mut ChatState,
    request: String,
    said: &str,
    asks: Option<Asks>,
    words: DirectWords,
) -> anyhow::Result<Vec<Effect>> {
    let asks = asks.unwrap_or_else(|| mode_asks(ctx, chat, &words));
    if let Some(instead) = instead(ctx, chat, asks, &request) {
        return Ok(instead);
    }
    start(ctx, chat, request, asks, |asking| TurnStep::Direct {
        asking,
        go_ahead: Some(said.to_owned()),
        words,
    })?;
    Ok(Vec::new())
}

/// Who may carry a direct turn on when its agent runs out: `roles.default.handoff`'s list, as
/// a run's shortlist judges it for the default role, so a disabled, testing, or over-cap vendor
/// is left out and a paid API needs your OK. A direct turn never falls back to the crew.
pub(super) fn shortlist(ctx: &Ctx, stopped: &WorkerRef, tried: &[WorkerRef]) -> Vec<Candidate> {
    let settings = ctx.settings();
    let role = settings
        .roles
        .get(&RoleName::Default)
        .map(|role| &role.handoff);
    let rules = HandoffSettings::applied(&settings.handoff, role, &stopped.vendor());
    if rules.candidates.is_empty() {
        return Vec::new();
    }
    let query = CandidateQuery {
        card: your_checkout(),
        role: RoleName::Default,
        stopped: stopped.clone(),
        exclude_families: Vec::new(),
        fallback: rules.candidates.clone(),
        all_valid: false,
    };
    let sources = Sources {
        settings,
        registry: ctx.services.registry.as_ref(),
        quota: ctx.services.quota.as_ref(),
        health: ctx.services.health.as_ref(),
        records: &NoRecords,
        slots: None,
    };
    handoff_shortlist(&query, &sources)
        .candidates
        .into_iter()
        .filter(|candidate| {
            !tried.contains(&candidate.worker)
                && ctx.services.workers.get(&candidate.worker).is_ok()
        })
        .collect()
}

/// A direct turn has no card of its own, so the shortlist judges it as one solo card that owns
/// your checkout.
fn your_checkout() -> CardSpec {
    CardSpec {
        id: CardId::new("direct-001").expect("a valid card id"),
        title: String::new(),
        task: String::new(),
        kind: CardKind::Solo,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        owner: None,
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    }
}

/// Track records order a run's candidates; a direct turn takes its list's order.
struct NoRecords;

impl TrackRecords for NoRecords {
    fn get(&self, _: &WorkerRef, _: &str) -> Option<TrackRecord> {
        None
    }
}

/// The handoff card: who carries the turn on, each with what it costs, or stop here.
pub(super) fn ask_handoff(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: Turn,
    tried: Vec<WorkerRef>,
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
                candidate.quota_text.clone()
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
    let (told, _) = handoff(ctx, &turn);
    let id = ctx.next_choice_id();
    let mut choice = choices::app_choice(
        id.clone(),
        ChoiceKind::HandOff,
        format!("{from} {why}. Who carries it on?"),
        vec![format!("They're told {told}.")],
        options,
    );
    choice.bell = true;
    let pending = PendingHandoff {
        turn,
        tried,
        why,
        candidates: candidates
            .iter()
            .map(|candidate| candidate.worker.clone())
            .collect(),
    };
    if let Some((old, earlier)) = chat.direct.handoff.replace((id, pending)) {
        ctx.send(AppEvent::ChoiceClosed {
            id: old,
            outcome: "Stopped".to_owned(),
        });
        let error = format!("{} {}", earlier.turn.worker, earlier.why);
        lifecycle::end_direct(ctx, chat, earlier.turn, error);
    }
    ctx.send(AppEvent::Choice { choice });
}

fn answer_handoff(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    pending: PendingHandoff,
    value: &str,
) -> anyhow::Result<Vec<Effect>> {
    let picked = pending
        .candidates
        .iter()
        .find(|worker| worker.to_string() == value)
        .cloned();
    let Some(to) = picked.filter(|_| chat.turn.is_none()) else {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: "Stopped".to_owned(),
        });
        let error = format!("{} {}", pending.turn.worker, pending.why);
        return Ok(lifecycle::end_direct(ctx, chat, pending.turn, error));
    };
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: format!("{} carries it on", ctx.names.worker(&to)),
    });
    lifecycle::hand_off(ctx, chat, pending.turn, pending.tried, pending.why, to)
}

/// Plan first in direct solo: the turn reads and plans, and changes nothing until you say go
/// ahead with its plan.
pub(super) fn plan(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
    words: DirectWords,
) -> anyhow::Result<Vec<Effect>> {
    lifecycle::begin(ctx, chat, text, |_| TurnStep::Plan { words })?;
    Ok(Vec::new())
}

/// What the session reads when you go ahead with its plan.
const GO_AHEAD_WITH_PLAN: &str = "Go ahead with this plan.";

/// After a plan-first turn's plan: "Go ahead with this plan?".
pub(super) fn offer_plan(ctx: &mut Ctx, chat: &mut ChatState, turn: &Turn) {
    if let TurnStep::Plan { words } = &turn.step {
        offer_go_ahead(ctx, chat, turn, words.clone());
    }
}

/// "Go ahead with this plan?" for `turn`'s request, whose reply was the plan.
pub(super) fn offer_go_ahead(ctx: &mut Ctx, chat: &mut ChatState, turn: &Turn, words: DirectWords) {
    let cli = ctx.names.harness(&turn.worker.worker);
    // An agent's session carried on works in the run's copy, not your checkout.
    let folder = if chat.folder(ctx) == ctx.env.repo_root.as_path() {
        "your checkout"
    } else {
        "the run's copy"
    };
    let id = ctx.next_choice_id();
    let mut choice = choices::app_choice(
        id.clone(),
        ChoiceKind::GoAhead,
        "Go ahead with this plan?".to_owned(),
        vec![format!("{cli} carries it out in {folder}.")],
        choices::options(ChoiceKind::GoAhead),
    );
    choice.bell = true;
    chat.direct.plans.insert(
        id,
        Planned {
            request: turn.text.clone(),
            words,
        },
    );
    ctx.send(AppEvent::Choice { choice });
}

fn answer_plan(
    ctx: &mut Ctx,
    chat: &mut ChatState,
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
    if chat.turn.is_some() {
        ctx.notice(
            NoteLevel::Info,
            "A turn is running. Answer the plan's question once it ends.",
        );
        chat.direct.plans.insert(id, planned);
        return Ok(Vec::new());
    }
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: "Yes, go ahead".to_owned(),
    });
    go_ahead(
        ctx,
        chat,
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
/// until you allow it to go on, since a checked copy would start a new session.
fn instead(ctx: &mut Ctx, chat: &ChatState, asks: Asks, text: &str) -> Option<Vec<Effect>> {
    match &chat.place {
        Some(place) if asks == Asks::EveryChange && !asks_each(&place.worker.worker) => {
            let cli = ctx.names.harness(&place.worker.worker);
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "{cli} can't ask before each edit in a headless run, and a checked copy \
                     would leave this session behind. Auto mode, or solo.asks = \"like_cli\", \
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
pub(super) fn begin_vendor(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    shown: String,
    prompt: String,
) -> anyhow::Result<Vec<Effect>> {
    let asks = mode_asks(ctx, chat, &DirectWords::default());
    start(ctx, chat, shown, asks, |asking| TurnStep::Vendor {
        prompt,
        direct: Some(asking),
    })?;
    Ok(Vec::new())
}

/// Nothing in auto mode or full auto, unless your words asked to be asked first, and what
/// `solo.asks` says otherwise. Words only ever add asking: none make a turn ask less than its
/// mode says.
fn mode_asks(ctx: &Ctx, chat: &ChatState, words: &DirectWords) -> Asks {
    match chat.direct.approval {
        ApprovalMode::Auto | ApprovalMode::FullAuto if !words.supervised => Asks::Never,
        ApprovalMode::Auto
        | ApprovalMode::FullAuto
        | ApprovalMode::Supervised
        | ApprovalMode::Shadow => ctx.settings().solo.asks,
    }
}

/// Codex, agy, and OpenCode can't be asked before each edit in stage 1, so `every_change` runs
/// them in a checked copy, and a line says why.
fn checked_instead(ctx: &Ctx, asks: Asks, text: &str) -> Option<Effect> {
    let main = &ctx.settings().default_agent;
    if asks != Asks::EveryChange || asks_each(&main.worker) {
        return None;
    }
    let cli = ctx.names.harness(&main.worker);
    ctx.notice(
        NoteLevel::Info,
        format!(
            "{cli} can't ask before each edit in a headless run, so this runs in a checked copy."
        ),
    );
    Some(Effect::StartRun(Box::new(run_request(
        ctx,
        text.to_owned(),
        RunMode::Solo,
        None,
        None,
    ))))
}

fn start(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: String,
    asks: Asks,
    step: impl FnOnce(Option<argonauts_core::worker::Asking>) -> TurnStep,
) -> anyhow::Result<()> {
    // Its folder is there before the snapshot `/rewind` takes of it.
    if !lifecycle::place_ready(ctx, chat)? {
        return Ok(());
    }
    let settings = ctx.settings();
    let start_run = settings.solo.start_run_tool;
    let rewind = settings.solo.rewind.then(|| max_bytes(settings));
    let next = TurnId::numbered(chat.turns + 1);
    let listener = if asks != Asks::Never || start_run {
        Some(listen::listen(ctx.internal.clone(), next)?)
    } else {
        None
    };
    let asking = listener
        .as_ref()
        .map(|listener| listener.asking(asks, start_run));
    let root = chat.folder(ctx).to_path_buf();
    let watched = chat.direct.watched.clone();
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
    lifecycle::begin(ctx, chat, text.clone(), |_| step(asking))?;
    if let Some(turn) = chat.turn.as_mut() {
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
fn set_aside_planted(ctx: &Ctx, chat: &ChatState, running: &Running, turn: &TurnId) {
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
        .chat_dir()
        .join(chat.id.as_str())
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

/// Notes a direct turn's prompt carries after your message: only what `/rewind` put back,
/// since Argonauts adds no instructions of its own.
pub(super) fn notes(chat: &mut ChatState) -> Vec<InputPart> {
    chat.direct
        .rewound
        .take()
        .map(|text| part(InputPartKind::Notes, "rewind", text))
        .into_iter()
        .collect()
}

/// Follows the turn's events for what comes after it: agy's refused commands, and what the
/// agent said, for a handoff.
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
pub(super) fn finished(ctx: &mut Ctx, chat: &mut ChatState, turn: &mut Turn) {
    let Some(mut running) = turn.direct.take() else {
        return;
    };
    close_asks(ctx, chat, &turn.id);
    set_aside_planted(ctx, chat, &running, &turn.id);
    record(ctx, chat, &mut running);
    let agy = turn.worker.worker == ANTIGRAVITY;
    if agy && running.asks != Asks::Never && !running.refused.is_empty() {
        offer_refused(ctx, chat, turn, &running.refused);
    }
}

/// A turn that ended without its answer, such as at the budget or a crash: one line says
/// what it changed, so the end is never silent.
pub(super) fn failed(ctx: &mut Ctx, chat: &mut ChatState, turn: &mut Turn) {
    let Some(mut running) = turn.direct.take() else {
        return;
    };
    close_asks(ctx, chat, &turn.id);
    set_aside_planted(ctx, chat, &running, &turn.id);
    let changed = record(ctx, chat, &mut running);
    match changed {
        Some(summary) => ctx.notice(NoteLevel::Info, format!("Stopped · {summary}")),
        None => ctx.notice(NoteLevel::Info, "Stopped · nothing changed"),
    }
}

/// Esc: the turn is stopping, and its files are counted once its session ends.
pub(super) fn cancelled(ctx: &mut Ctx, chat: &mut ChatState, turn: &mut Turn) {
    close_asks(ctx, chat, &turn.id);
    if let Some(mut running) = turn.direct.take() {
        running.listener = None;
        chat.direct.stopping.insert(turn.id.clone(), running);
    }
}

/// A stopped turn's session ended: what it changed, and `/rewind`.
pub(super) fn stopped(ctx: &mut Ctx, chat: &mut ChatState, id: &TurnId) {
    let Some(mut running) = chat.direct.stopping.remove(id) else {
        return;
    };
    set_aside_planted(ctx, chat, &running, id);
    if let Some(summary) = record(ctx, chat, &mut running) {
        ctx.notice(NoteLevel::Info, summary);
    }
}

/// What the next agent is told when this one runs out, as a note in its prompt and as the
/// `↷` line says it: the files changed so far, and what the last agent said.
pub(super) fn handoff(ctx: &Ctx, turn: &Turn) -> (String, Option<InputPart>) {
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
    (told, Some(part(InputPartKind::Handoff, "handoff", text)))
}

/// The snapshot after the turn, kept when the turn changed something, and a line saying what
/// changed and what `/rewind` can put back. `None` when it changed nothing or there was no
/// snapshot.
fn record(ctx: &Ctx, chat: &mut ChatState, running: &mut Running) -> Option<String> {
    running.listener = None;
    let before = running.before.take()?;
    let root = &running.root;
    let max = max_bytes(ctx.settings());
    // Ignored files the turn wrote are kept from now on, so a later turn's can be put back.
    for path in &running.written {
        if rewind::ignored(root, path) {
            chat.direct.watched.insert(path.clone());
        }
    }
    let after = rewind::take(root, max, &chat.direct.watched)
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
    // Such as "app.py (+1 -1) changed · /rewind puts it back, except .env".
    let mut summary = rewind::summary(root, &before, &after, &changed);
    summary.push_str(" · /rewind puts it back");
    match unkept.as_slice() {
        [] => {}
        [one] => summary.push_str(&format!(", except {one}")),
        [rest @ .., last] => summary.push_str(&format!(", except {} and {last}", rest.join(", "))),
    }
    chat.direct.turns.push(TurnRecord {
        text: running.text.clone(),
        before,
        after,
        changed,
    });
    Some(summary)
}

fn max_bytes(settings: &Settings) -> u64 {
    settings.solo.rewind_max_mb.saturating_mul(1024 * 1024)
}

/// A question from the turn's session: answered at once when you already allowed it,
/// otherwise a card.
pub(crate) fn on_asked(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: &TurnId,
    asked: Asked,
) -> anyhow::Result<Vec<Effect>> {
    let current = chat.turn.as_ref().filter(|current| &current.id == turn);
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
            if ask::allowed(&tool_name, &input, chat.direct.edits_allowed, patterns) {
                let _ = asked.reply.send(AskReply::allow(&input));
                return Ok(Vec::new());
            }
            let cli = ctx.names.harness(&current.worker.worker);
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
            chat.direct.asks.insert(
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
        Ask::StartRun { how_to_run, .. } => {
            Ok(start_run(ctx, chat, turn, &how_to_run, asked.reply))
        }
    }
}

/// `argonauts_start_run`: a run, when your message asked for one in its own words. The words
/// go through the same checks as an estimate's do, against your message, and the run's request
/// is your message as you typed it, so neither the agent's restatement nor a word it picked out
/// can start anything. The run's Triage line says what was followed and what wasn't.
fn start_run(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: &TurnId,
    how_to_run: &[Value],
    reply: tokio::sync::oneshot::Sender<AskReply>,
) -> Vec<Effect> {
    let Some(current) = chat.turn.as_ref().filter(|current| &current.id == turn) else {
        return Vec::new();
    };
    if current.run_began {
        let _ = reply.send(AskReply::text(
            "A run already started for this request. Stop here: the run does the work.",
        ));
        return Vec::new();
    }
    let text = current.text.clone();
    let how_to_run: Vec<Instruction> = how_to_run
        .iter()
        .filter_map(|instruction| serde_json::from_value(instruction.clone()).ok())
        .collect();
    let applied = instructions::check(&how_to_run, &text, ctx.settings(), &Roster::new(ctx), None);
    let Some(mode) = run_asked(&applied) else {
        let _ = reply.send(AskReply::text(
            "The user didn't ask for a run, so do it yourself.",
        ));
        return Vec::new();
    };
    if let Some(current) = chat.turn.as_mut() {
        current.run_began = true;
    }
    let mut start = run_request(ctx, text, mode, None, Some(turn.clone()));
    start.forced = false;
    start.triage = Some(TriageDecision {
        mode,
        escalate_to_crew: false,
        rule: TriageDecision::AS_YOU_ASKED.to_owned(),
        reason: String::new(),
        // A run is a copy, so solo that starts one is checked.
        solo: (mode == RunMode::Solo).then_some(SoloStyle::Checked),
    });
    applied.apply_to(&mut start);
    super::keep::offer(ctx, chat, &applied);
    let _ = reply.send(AskReply::text(format!(
        "Argonauts started a {} run for this request. Stop here: the run does the work, and \
         the user sees it.",
        crate::controller::recap::mode_name(mode)
    )));
    vec![Effect::StartRun(Box::new(start))]
}

/// The run your checked words ask for: the crew, a checked copy, or other agents in roles,
/// which a turn of this session can't give. Solo in your checkout is this turn already, and a
/// model, an effort, or a tightening alone changes nothing a run would add, so none of them
/// starts one.
fn run_asked(applied: &Applied) -> Option<RunMode> {
    use InstructionKind as Kind;
    let asked = applied.asked;
    if asked.mode == Some(RunMode::Crew) {
        return Some(RunMode::Crew);
    }
    let roles = applied.instructions.iter().any(|instruction| {
        instruction.followed()
            && matches!(
                instruction.kind,
                Kind::Lead
                    | Kind::Coordinate
                    | Kind::Build
                    | Kind::Review
                    | Kind::Plan
                    | Kind::Check
                    | Kind::Crew
                    | Kind::Exclude
                    | Kind::Style
            )
    });
    (asked.solo == Some(SoloStyle::Checked) || roles).then_some(RunMode::Solo)
}

/// A question of `turn`'s is open on a card, so its CLI waits on you in silence.
pub(super) fn waits_on_you(chat: &ChatState, turn: &TurnId) -> bool {
    chat.direct.asks.values().any(|ask| &ask.turn == turn)
}

pub(crate) fn owns_choice(chat: &ChatState, id: &ChoiceId) -> bool {
    chat.direct.asks.contains_key(id)
        || chat.direct.refused.contains_key(id)
        || chat.direct.plans.contains_key(id)
        || chat.direct.rewinding.as_ref() == Some(id)
        || chat
            .direct
            .handoff
            .as_ref()
            .is_some_and(|(open, _)| open == id)
}

pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    action: &AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    let value = match action {
        AnswerAction::Choose { value } => value.as_str(),
        AnswerAction::Cancel => "no",
        other => anyhow::bail!("{other:?} doesn't answer this question"),
    };
    if let Some(pending) = chat.direct.asks.remove(&id) {
        return answer_ask(ctx, chat, id, pending, value);
    }
    if let Some(offer) = chat.direct.refused.remove(&id) {
        return answer_refused(ctx, chat, id, offer, value);
    }
    if let Some(planned) = chat.direct.plans.remove(&id) {
        return answer_plan(ctx, chat, id, planned, value);
    }
    if chat
        .direct
        .handoff
        .as_ref()
        .is_some_and(|(open, _)| open == &id)
        && let Some((_, pending)) = chat.direct.handoff.take()
    {
        return answer_handoff(ctx, chat, id, pending, value);
    }
    if chat.direct.rewinding.as_ref() == Some(&id) {
        return rewind_to(ctx, chat, id, value);
    }
    ctx.notice(NoteLevel::Info, "That question is already closed.");
    Ok(Vec::new())
}

/// Esc: No to a question, the `/rewind` card closed, and agy's offer left waiting.
pub(crate) fn set_aside(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
) -> anyhow::Result<Vec<Effect>> {
    if chat.direct.refused.contains_key(&id) {
        return Ok(Vec::new());
    }
    answer_choice(ctx, chat, id, &AnswerAction::Cancel)
}

fn answer_ask(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    pending: PendingAsk,
    value: &str,
) -> anyhow::Result<Vec<Effect>> {
    let mut effects = Vec::new();
    // Such as "Yes to the edit to notes.txt", since the card's own question goes when it closes.
    let about = pending.question.about.clone();
    let outcome = match value {
        "yes" => {
            let _ = pending.reply.send(AskReply::allow(&pending.input));
            format!("Yes to {about}")
        }
        "always" => {
            effects = grant(ctx, chat, &pending.question.grant)?;
            // What's now allowed answers the questions already waiting on it.
            let _ = pending.reply.send(AskReply::allow(&pending.input));
            answer_waiting(ctx, chat);
            format!("Yes to {about}, and don't ask again")
        }
        "no" => {
            let _ = pending.reply.send(ask::no());
            // As in Claude Code: No stops the turn, and your next message goes to the agent.
            if chat
                .turn
                .as_ref()
                .is_some_and(|turn| turn.id == pending.turn)
            {
                effects.extend(super::cancel_turn(ctx, chat)?);
            }
            format!("No to {about}")
        }
        other => {
            chat.direct.asks.insert(id, pending);
            anyhow::bail!("{other:?} isn't one of this question's answers");
        }
    };
    ctx.send(AppEvent::ChoiceClosed { id, outcome });
    Ok(effects)
}

/// Saves what "don't ask again" covers: edits for this conversation, or a pattern at "This
/// project, just me", which you set by answering, as with any safety confirm.
fn grant(ctx: &mut Ctx, chat: &mut ChatState, grant: &ask::Grant) -> anyhow::Result<Vec<Effect>> {
    let pattern = match grant {
        ask::Grant::Edits => {
            chat.direct.edits_allowed = true;
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
fn answer_waiting(ctx: &mut Ctx, chat: &mut ChatState) {
    let patterns = ctx.settings().approvals.allow_commands.clone();
    let edits = chat.direct.edits_allowed;
    let allowed: Vec<ChoiceId> = chat
        .direct
        .asks
        .iter()
        .filter(|(_, pending)| ask::allowed(&pending.tool, &pending.input, edits, &patterns))
        .map(|(id, _)| id.clone())
        .collect();
    for id in allowed {
        if let Some(pending) = chat.direct.asks.remove(&id) {
            let _ = pending.reply.send(AskReply::allow(&pending.input));
            ctx.send(AppEvent::ChoiceClosed {
                id,
                outcome: "Yes".to_owned(),
            });
        }
    }
}

/// The turn's open questions close with it, each a no.
fn close_asks(ctx: &Ctx, chat: &mut ChatState, turn: &TurnId) {
    let ids: Vec<ChoiceId> = chat
        .direct
        .asks
        .iter()
        .filter(|(_, pending)| &pending.turn == turn)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        chat.direct.asks.remove(&id);
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: "The turn ended.".to_owned(),
        });
    }
}

fn offer_refused(ctx: &mut Ctx, chat: &mut ChatState, turn: &Turn, commands: &[String]) {
    let cli = ctx.names.harness(&turn.worker.worker);
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
    chat.direct.refused.insert(
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
    chat: &mut ChatState,
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
    if chat.turn.is_some() {
        ctx.notice(
            NoteLevel::Info,
            "A turn is running. Answer agy's question once it ends.",
        );
        chat.direct.refused.insert(id, offer);
        return Ok(Vec::new());
    }
    let cli = ctx.names.harness(&offer.worker.worker);
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: format!("{cli} continues, running commands without asking"),
    });
    go_ahead(
        ctx,
        chat,
        offer.request,
        RUN_REFUSED,
        Some(Asks::Never),
        DirectWords::default(),
    )
}

/// `/rewind` in a direct conversation: your messages whose turns changed files, newest first.
pub(crate) fn rewind(ctx: &mut Ctx, chat: &mut ChatState) -> bool {
    if chat.direct.turns.is_empty() {
        return false;
    }
    let id = ctx.next_choice_id();
    let newest = chat.direct.turns.len() - 1;
    let options = chat
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
    chat.direct.rewinding = Some(id);
    ctx.send(AppEvent::Choice { choice });
    true
}

fn rewind_to(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    value: &str,
) -> anyhow::Result<Vec<Effect>> {
    chat.direct.rewinding = None;
    let Ok(from) = value.parse::<usize>() else {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: choices::NOT_CHANGED.to_owned(),
        });
        return Ok(Vec::new());
    };
    let (Some(target), Some(last)) = (chat.direct.turns.get(from), chat.direct.turns.last()) else {
        anyhow::bail!("that message's files can't be put back any more");
    };
    let mut paths: Vec<String> = chat.direct.turns[from..]
        .iter()
        .flat_map(|record| record.changed.iter().cloned())
        .collect();
    paths.sort();
    paths.dedup();
    let root = chat.folder(ctx).to_path_buf();
    let restored = rewind::restore(
        &root,
        &target.before,
        &last.after,
        &paths,
        max_bytes(ctx.settings()),
    )?;
    let asked = target.text.clone();
    chat.direct.turns.truncate(from);
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
        chat.direct.rewound = Some(format!(
            "The user put back these files to how they were before \"{}\": {}.",
            asked,
            restored.put_back.join(", ")
        ));
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::instructions::{Agent, AgentState};
    use argonauts_core::steps::intake::InstructionScope;

    fn roster() -> Roster {
        let agent = |id: &str| Agent {
            id: id.to_owned(),
            name: id.to_owned(),
            family: id.to_owned(),
            state: AgentState::Ready,
            ..Agent::default()
        };
        Roster {
            agents: vec![
                agent("claude-code"),
                agent("codex"),
                agent("antigravity-cli"),
            ],
        }
    }

    fn said(kind: InstructionKind, says: &str, who: &str, value: &str) -> Instruction {
        Instruction {
            says: says.to_owned(),
            kind,
            who: who.to_owned(),
            model: String::new(),
            effort: String::new(),
            value: value.to_owned(),
            scope: InstructionScope::ThisRequest,
        }
    }

    /// Only words that ask for the crew, a checked copy, or other agents in roles start a run:
    /// solo in your checkout, a model or an effort, and words you didn't write are this turn.
    #[test]
    fn a_run_starts_only_for_what_this_turn_cannot_do() {
        use InstructionKind::{Crew, Effort, Mode, Review};
        let settings = Settings::default();
        let asked = |message: &str, list: &[Instruction]| {
            run_asked(&instructions::check(
                list,
                message,
                &settings,
                &roster(),
                None,
            ))
        };
        let review = said(Review, "have codex review this", "codex", "");
        assert_eq!(
            asked("Have codex review this.", &[review]),
            Some(RunMode::Solo)
        );
        let list = [
            said(Crew, "use the crew", "", ""),
            said(Review, "codex reviews", "codex", ""),
        ];
        assert_eq!(
            asked("Use the crew, codex reviews.", &list),
            Some(RunMode::Crew)
        );
        let solo = said(Mode, "just do it solo", "", "solo");
        assert_eq!(asked("Just do it solo.", &[solo]), None);
        let picked = said(Mode, "the", "", "crew");
        assert_eq!(
            asked("Fix the rounding in invoice/money.py", &[picked]),
            None
        );
        let effort = said(Effort, "max effort", "", "max");
        assert_eq!(asked("Fix it on max effort.", &[effort]), None);
    }
}
