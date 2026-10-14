//! The live region's own rows from the app state: the working line, the narrator's live line,
//! queued messages, the input rule's label, the mode line, and the title. The bottom line's
//! parts are [`bottom_line`]'s.

use std::time::{Duration, Instant};

use argonauts_core::KeyScheme;
use argonauts_core::app::{AppMode, AwakeHold, ChoiceKind};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::card::CardState;
use argonauts_core::events::run::{Party, RunPhase};
use argonauts_core::events::session::{Delivery, QueuedMessage};
use argonauts_core::input::MentionTarget;
use argonauts_core::settings::approvals::ApprovalMode as CoreApproval;
use argonauts_core::settings::bottom_line::AWAKE;
use argonauts_core::settings::ui::{
    HeadsUp as HeadsUpSetting, NarratorMode, UiSettings, VerbTheme,
};
use time::{OffsetDateTime, UtcOffset};

use crate::bottom::line::PartBody;
use crate::bottom::verbs::VerbStyle;
use crate::bottom::{Activity, ApprovalMode, BottomState, HeadsUp, ModeLine, VimMode, WorkingLine};
use crate::cell::Tone;
use crate::format;
use crate::keymap::{Action, Keymap};
use crate::state::{AppState, ChatState, Doing, RunState};

use super::{Cx, bottom_line};

/// Brings every row under the transcript in line with the state. Runs after every event and
/// action, so it must be cheap and idempotent.
pub fn sync(cx: &mut Cx<'_>) {
    let state = cx.state;
    let keys = cx.out.vim.map_or(cx.settings.keys, |on| {
        if on {
            KeyScheme::Vim
        } else {
            KeyScheme::Standard
        }
    });
    let verbs = verb_style(cx.settings.verbs);
    let narrator_live = cx.settings.narrator == NarratorMode::Live;
    let now = cx.now;
    cx.model.verbs = verbs;
    if let Some(title) = title(state) {
        cx.model.title = title;
    }

    let picker = &mut cx.model.verb_picker;
    let bottom = &mut cx.model.bottom;
    sync_working(state, bottom, || picker.working(verbs), now);
    bottom.narrator = narrator_live.then(|| live_narration(state)).flatten();
    let (queued, queued_where) = queued(state);
    bottom.queued = queued;
    bottom.queued_where = queued_where;
    if let Some(label) = composer_label(state) {
        bottom.composer.label = Some(label);
    }
    bottom.composer.ultracode = state.status.as_ref().is_some_and(|status| status.ultracode);
    bottom.workflow_key = workflow_key(cx.settings);

    apply_keys(&mut bottom.mode, keys);
    if let Some(approval) = controller_approval(state)
        && bottom.synced.approval != Some(approval)
    {
        bottom.mode.approval = approval;
        bottom.synced.approval = Some(approval);
    }
    bottom.mode.background_agents = state.agents_running();
    let on_screen = usize::from(bottom.card.is_some());
    bottom.mode.needs_you =
        state.needs_you().len().saturating_sub(on_screen) + state.background_waiting();

    bottom.send_now = state.chat.send_now && state.chat.turn.is_some();
    bottom.send_now_hint = send_now_hint(state, cx.settings);
    sync_heads_up(state, cx.settings, bottom);

    let now = OffsetDateTime::now_utc();
    let clock = bottom_line::Clock {
        now,
        utc_offset: state.terminal.utc_offset.unwrap_or(UtcOffset::UTC),
    };
    match awake_part(state, &clock) {
        Some(awake) => {
            bottom.line_slots.insert(AWAKE.to_owned(), awake);
        }
        None => {
            bottom.line_slots.remove(AWAKE);
        }
    }
    let line = bottom_line::build(state, cx.settings, &bottom.line_slots, now);
    // Your mods placed above the input draw under the band's own cards.
    if bottom.file_mods.as_ref() != state.mods.as_ref() {
        bottom.file_mods = state.mods.clone();
    }
    bottom.line = line;
    bottom.enter_sends = enter_sends(state);
    bottom.run_takes_text = run_takes_text(state);
    bottom.shell_running = state.shell.is_some();
    bottom.exit_note = exit_note(state);
}

/// `ctrl+enter to send now` under your messages while the session takes them now, or the
/// chord where the terminal can't tell Ctrl+Enter from a new line.
fn send_now_hint(state: &AppState, settings: &UiSettings) -> Option<String> {
    let waiting = state.chat.send_now && state.chat.turn.is_some() && !state.chat.queued.is_empty();
    if !waiting {
        return None;
    }
    let keymap = Keymap::with_overrides(&settings.keymap).0;
    let keys = keymap.keys(Action::SendNow);
    let protocol = crate::terminal::setup::keyboard_protocol();
    let key = keys
        .iter()
        .find(|key| protocol || key.contains(' ') || !key.ends_with("enter"))
        .or_else(|| keys.first())?;
    Some(format!("{key} to send now"))
}

/// What Claude Code draws above the input, kept while the drawing is the same so your keys
/// stay on their button.
fn sync_heads_up(state: &AppState, settings: &UiSettings, bottom: &mut BottomState) {
    let drawn = state
        .chat
        .heads_up
        .as_ref()
        .filter(|_| settings.heads_up == HeadsUpSetting::WhenEnabled);
    match drawn {
        Some(drawn) if bottom.heads_up.drawn != *drawn => bottom.heads_up.redraw(drawn.clone()),
        Some(_) => {}
        None => bottom.heads_up = HeadsUp::default(),
    }
}

/// Vim keys show `-- INSERT --` or `-- NORMAL --` always, and the screen switches between
/// the two itself.
pub fn apply_keys(mode: &mut ModeLine, keys: KeyScheme) {
    match keys {
        KeyScheme::Vim if mode.vim.is_none() => mode.vim = Some(VimMode::Insert),
        KeyScheme::Vim => {}
        KeyScheme::Standard => mode.vim = None,
    }
}

pub fn verb_style(theme: VerbTheme) -> VerbStyle {
    match theme {
        VerbTheme::Voyage => VerbStyle::Voyage,
        VerbTheme::Plain => VerbStyle::Plain,
    }
}

/// What the working line is about.
enum Work<'a> {
    Chat(&'a ChatState),
    Run(&'a RunState),
    Pausing(&'a RunState),
}

impl Work<'_> {
    fn key(&self) -> String {
        match self {
            Work::Chat(chat) => {
                format!(
                    "turn:{}",
                    chat.turn.as_ref().map_or("", |turn| turn.as_str())
                )
            }
            Work::Run(run) => format!("run:{}", run.id),
            Work::Pausing(run) => format!("pausing:{}", run.id),
        }
    }

    /// Whether the conductor said the stream stalled. A crew's run stalls only once every agent
    /// still running has, since one at work keeps the run going.
    fn stalled(&self) -> bool {
        match self {
            Work::Chat(chat) => chat.stalled,
            Work::Run(run) => {
                let mut running = run.running_sessions().peekable();
                running.peek().is_some() && running.all(|session| session.stalled)
            }
            Work::Pausing(_) => false,
        }
    }
}

fn current_work(state: &AppState) -> Option<Work<'_>> {
    let run_work = |id| {
        let run = state.runs.get(id)?;
        if run.finished.is_some() || waits_on_you(run) {
            return None;
        }
        Some(if run.pausing {
            Work::Pausing(run)
        } else {
            Work::Run(run)
        })
    };
    // While the agent waits on your answer to what it asked, nothing goes on but the wait, as
    // Claude Code hides its spinner while a permission prompt is up.
    let asks_you = state
        .choices
        .iter()
        .any(|choice| choice.kind == ChoiceKind::Permission);
    match &state.mode {
        Some(AppMode::Answering { .. }) if asks_you => None,
        // Only what the main agent's CLI runs in the background works now. Its rows show
        // that, and there is no turn for Esc to stop.
        Some(AppMode::Answering { .. }) if state.chat.waits_on_background() => None,
        Some(AppMode::Answering { .. }) => Some(Work::Chat(&state.chat)),
        Some(AppMode::Working { run }) => run_work(run),
        Some(AppMode::Pausing { run }) => state.runs.get(run).map(Work::Pausing),
        Some(AppMode::Idle | AppMode::Parked { .. }) => None,
        None if state.chat.turn.is_some() => Some(Work::Chat(&state.chat)),
        None => {
            let run = state.attached_run().filter(|run| run.is_active())?;
            run_work(&run.id)
        }
    }
}

/// A card waits on your answer, and neither an agent nor the conductor's gates or merges are at
/// work on the run, so nothing is going on but the wait. The line steps aside, as Claude Code's
/// spinner does while a permission prompt is up, and comes back with the run's own clock once
/// you answer.
fn waits_on_you(run: &RunState) -> bool {
    let card_at_work = run.cards.values().any(|card| {
        matches!(
            card.state,
            Some(
                CardState::Running
                    | CardState::Gating
                    | CardState::InReview
                    | CardState::Integrating
            )
        )
    });
    run.checkpoints.iter().any(|open| open.opened.needs_you)
        && run.running_sessions().next().is_none()
        && !card_at_work
}

fn sync_working(
    state: &AppState,
    bottom: &mut crate::bottom::BottomState,
    pick: impl FnOnce() -> &'static str,
    now: Instant,
) {
    let Some(work) = current_work(state) else {
        bottom.working = None;
        bottom.synced.work = None;
        return;
    };
    let key = work.key();
    let same = bottom.synced.work.as_deref() == Some(key.as_str());
    let previous = bottom.working.take().filter(|_| same);

    let mut line = match &previous {
        Some(line) => line.clone(),
        None => WorkingLine::new(verb_for(&work, pick), started_at(&work, now)),
    };
    match &work {
        Work::Chat(chat) => {
            line.tokens_down = chat.tokens_out;
            line.activity = activity(previous.as_ref().map(|l| &l.activity), chat.doing, now);
            line.details.clear();
            line.hide_while_text_streams = true;
            line.esc_stops = true;
        }
        Work::Run(run) => {
            line.tokens_down = run.tokens_out();
            // A lone agent's words read like a chat turn's; a crew's would jump between agents.
            let mut running = run.running_sessions();
            line.activity = match (running.next(), running.next()) {
                (Some(only), None) => {
                    activity(previous.as_ref().map(|l| &l.activity), only.doing, now)
                }
                _ => Activity::None,
            };
            line.details = run_details(run);
            // Esc leaves a run's agents going. It still stops a chat turn answered meanwhile.
            line.esc_stops = state.chat.turn.is_some();
        }
        Work::Pausing(_) => {
            line.tokens_down = 0;
            line.activity = Activity::None;
            line.details = vec!["esc again to stop now".to_owned()];
            line.timed = false;
            line.esc_stops = true;
        }
    }
    // The fade runs from the stall the conductor reported, never from a quiet spell, since a
    // model can think for minutes without a word.
    line.stalled_since = if work.stalled() {
        line.stalled_since.or(Some(now))
    } else {
        None
    };
    bottom.working = Some(line);
    bottom.synced.work = Some(key);
}

/// A new word for each stretch of work; the same turn or run keeps its word.
fn verb_for(work: &Work<'_>, pick: impl FnOnce() -> &'static str) -> String {
    match work {
        Work::Pausing(_) => "Pausing".to_owned(),
        Work::Chat(_) | Work::Run(_) => pick().to_owned(),
    }
}

/// A run joined midway shows how long it has been going, as of its last event.
fn started_at(work: &Work<'_>, now: Instant) -> Instant {
    let Work::Run(run) = work else {
        return now;
    };
    let (Some(started), Some(updated)) = (run.started, run.updated) else {
        return now;
    };
    let ran = Duration::try_from(updated - started).unwrap_or_default();
    now.checked_sub(ran).unwrap_or(now)
}

/// The screen times each spell itself, since the stream says only what's happening now.
fn activity(previous: Option<&Activity>, doing: Doing, now: Instant) -> Activity {
    match (doing, previous) {
        (Doing::Thinking, Some(Activity::Thinking { since })) => {
            Activity::Thinking { since: *since }
        }
        (Doing::Thinking, _) => Activity::Thinking { since: now },
        (Doing::RunningTool, Some(Activity::RunningTool { since })) => {
            Activity::RunningTool { since: *since }
        }
        (Doing::RunningTool, _) => Activity::RunningTool { since: now },
        (Doing::Nothing | Doing::Writing, Some(Activity::Thinking { since })) => {
            Activity::Thought {
                lasted: now.saturating_duration_since(*since),
            }
        }
        (Doing::Nothing | Doing::Writing, Some(thought @ Activity::Thought { .. })) => {
            thought.clone()
        }
        (Doing::Nothing | Doing::Writing, _) => Activity::None,
    }
}

/// Phase, cards, agents, and spend, as in `Build · 2/5 cards · 3 agents · $0.21`. The phase is
/// one the workflow view lists; only a run with no workflow names its own stage.
fn run_details(run: &RunState) -> Vec<String> {
    let mut details = Vec::new();
    let phase = match run.shown_workflow() {
        Some(_) => run.workflow_phase().map(str::to_owned),
        None => run.phase.map(|phase| phase_name(phase).to_owned()),
    };
    details.extend(phase.filter(|phase| !phase.trim().is_empty()));
    let (done, total) = run.cards_done();
    if total > 0 {
        details.push(format!("{done}/{total} tasks"));
    }
    match run.running_sessions().count() {
        0 => {}
        1 => details.push("1 agent".to_owned()),
        agents => details.push(format!("{agents} agents")),
    }
    let spent = run.spend();
    if spent.cost_usd > 0.0 {
        details.push(format::spend(&spent));
    }
    details
}

pub fn phase_name(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Goal => "Goal",
        RunPhase::Planning => "Planning",
        RunPhase::Dividing => "Dividing",
        RunPhase::Contracts => "Contracts",
        RunPhase::Build => "Build",
        RunPhase::FinalCheck => "Final check",
        RunPhase::Merge => "Merge",
        RunPhase::Retro => "Run review",
    }
}

/// The narrator's latest line while its run still goes, or any line not about a run.
fn live_narration(state: &AppState) -> Option<String> {
    let line = state.narrator.as_ref()?;
    let current = match &line.run {
        Some(run) => state.runs.get(run).is_some_and(RunState::is_active),
        None => true,
    };
    current.then(|| line.text.clone())
}

/// Your undelivered guidance and mentions in the attached run, then chat text waiting for its
/// turn, each with where it goes when that's one agent.
fn queued(state: &AppState) -> (Vec<String>, Vec<Option<String>>) {
    let mut texts = Vec::new();
    let mut places = Vec::new();
    if let Some(run) = state.attached_run() {
        for message in run
            .queued
            .iter()
            .filter(|message| message.from == Party::User)
        {
            texts.push(own_words(&message.text).to_owned());
            places.push(destination(run, message, &state.names));
        }
    }
    for text in &state.chat.queued {
        texts.push(text.clone());
        places.push(None);
    }
    (texts, places)
}

/// What you typed, without the line the engine frames it with for the agents, such as
/// `Guidance m-001 from you:`, which would stand in for your words on the screen.
fn own_words(text: &str) -> &str {
    match text.split_once('\n') {
        Some((frame, words)) if frame.ends_with(':') && frame.contains(" from you") => words,
        _ => text,
    }
}

/// `to Opus 5.5 on Claude Code, after its current tool call` or `to Antigravity CLI, at its
/// next turn`.
fn destination(run: &RunState, message: &QueuedMessage, names: &DisplayNames) -> Option<String> {
    let to = match &message.to {
        MentionTarget::All => return None,
        MentionTarget::Worker(worker) => names.worker(worker),
        MentionTarget::Card(card) => card.to_string(),
        MentionTarget::Role(role) => role.as_str().to_owned(),
        MentionTarget::Number(key) => run
            .sessions_in_order()
            .filter(|session| session.follow == Some(*key))
            .last()
            .map_or_else(
                || format!("@{key}"),
                |session| names.worker(&session.worker),
            ),
    };
    let when = match message.delivery {
        Delivery::Steer => "after its current tool call",
        Delivery::NextTurn => "at its next turn",
        Delivery::SessionStart => "when it starts",
    };
    Some(format!("to {to}, {when}"))
}

/// The input rule's label: the main session's model by name alone, such as `Opus 5.5`, with
/// the workflow's name first when it isn't solo. Which CLI the model runs on is
/// the marked chip on the bottom line. A run's own workflow shows on its panel row, never here.
fn composer_label(state: &AppState) -> Option<String> {
    let workflow = workflow_label(state).filter(|workflow| workflow != SOLO);
    match (workflow, main_model(state)) {
        (Some(workflow), Some(model)) => Some(format!(
            "{} · {model}",
            argonauts_core::text::shown_name(&workflow)
        )),
        (workflow, model) => workflow
            .map(|workflow| argonauts_core::text::shown_name(&workflow))
            .or(model),
    }
}

/// The workflow in which the main session's CLI does everything itself.
const SOLO: &str = "solo";

/// The model the main session runs, or its CLI's name while nobody knows the model.
fn main_model(state: &AppState) -> Option<String> {
    let agent = state
        .status
        .as_ref()
        .map(|status| &status.main_agent)
        .or_else(|| state.welcome.as_ref()?.main_agent.as_ref())?;
    let agent = state.names.running(agent);
    let cli = agent.worker.as_str();
    Some(
        state
            .names
            .model(cli, agent.model.as_deref())
            .unwrap_or_else(|| state.names.harness(cli)),
    )
}

/// The key that ignores the word `ultracode` in a draft, as the notice names it: `opt+w` on a
/// Mac keyboard, where Alt is the Option key.
fn workflow_key(settings: &UiSettings) -> String {
    let keymap = Keymap::with_overrides(&settings.keymap).0;
    let key = keymap
        .keys(Action::WorkflowKeywordToggle)
        .into_iter()
        .next()
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        key.replace("alt+", "opt+")
    } else {
        key
    }
}

fn workflow_label(state: &AppState) -> Option<String> {
    state
        .status
        .as_ref()
        .map(|status| status.workflow.clone())
        .filter(|preset| !preset.is_empty())
        .or_else(|| {
            state
                .welcome
                .as_ref()
                .map(|welcome| welcome.workflow.clone())
        })
        .filter(|preset| !preset.is_empty())
}

/// The run plain text and the spend part are about: the one the app mode names, else the
/// attached one.
pub(super) fn focus_run(state: &AppState) -> Option<&RunState> {
    let named = match &state.mode {
        Some(AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Parked { run, .. }) => {
            state.runs.get(run)
        }
        _ => None,
    };
    named.or_else(|| state.attached_run())
}

fn controller_approval(state: &AppState) -> Option<ApprovalMode> {
    let mode = focus_run(state)
        .and_then(|run| run.approval)
        .or_else(|| state.status.as_ref().map(|status| status.approval_mode))?;
    Some(match mode {
        CoreApproval::Supervised => ApprovalMode::Supervised,
        CoreApproval::AutoPreview => ApprovalMode::AutoPreview,
        CoreApproval::Auto => ApprovalMode::Auto,
        CoreApproval::Autopilot => ApprovalMode::Autopilot,
    })
}

fn title(state: &AppState) -> Option<String> {
    let title = state
        .status
        .as_ref()
        .map(|status| status.title.trim())
        .filter(|title| !title.is_empty())
        .or_else(|| {
            state
                .attached_run()
                .map(|run| run.title.trim())
                .filter(|title| !title.is_empty())
        })?;
    Some(title.to_owned())
}

/// `☕ awake until you quit`, or until a time, while `/awake` holds the computer's sleep off.
fn awake_part(state: &AppState, clock: &bottom_line::Clock) -> Option<PartBody> {
    let until = match state.status.as_ref()?.awake.as_ref()? {
        AwakeHold::UntilQuit => "you quit".to_owned(),
        AwakeHold::Until { at } if *at <= clock.now => return None,
        AwakeHold::Until { at } => bottom_line::clock_time(*at, clock),
    };
    // The ASCII glyph set is for fonts that draw symbols at the wrong width, so the cup goes.
    let ascii = state.ui.as_ref().is_some_and(|ui| {
        crate::theme::Glyphs::for_settings(ui.glyphs, ui.ambiguous_width).is_ascii()
    });
    let cup = if ascii { "" } else { "☕ " };
    Some(PartBody::Words {
        text: format!("{cup}awake until {until}"),
        tone: Tone::Code,
    })
}

/// The adapter id behind a chip's short name.
pub(super) fn vendor_for_label(label: &str) -> Option<&'static str> {
    Some(match label.trim() {
        "Claude" => "claude-code",
        "Codex" => "codex",
        "agy" => "antigravity-cli",
        "OpenCode" => "opencode",
        "OpenRouter" => "openrouter",
        _ => return None,
    })
}

/// The short names chips use for vendors.
pub fn label_for_vendor(vendor: &str) -> String {
    match vendor {
        "claude-code" => "Claude",
        "codex" => "Codex",
        "antigravity-cli" => "agy",
        "opencode" => "OpenCode",
        "openrouter" => "OpenRouter",
        "ollama" | "lmstudio" | "llamacpp" | "llama-cpp" | "vllm" | "local" => "Local",
        other => other,
    }
    .to_owned()
}

/// Where Enter sends a draft now, while a run works or a card waits.
fn enter_sends(state: &AppState) -> Option<String> {
    match &state.mode {
        Some(AppMode::Working { .. }) => Some("Enter sends guidance to the run".to_owned()),
        Some(AppMode::Pausing { .. }) => Some("Enter cancels the pause".to_owned()),
        Some(AppMode::Parked { .. }) => Some("Enter resumes the run".to_owned()),
        Some(AppMode::Answering { .. }) => Some("Enter sends it after this answer".to_owned()),
        Some(AppMode::Idle) | None => state
            .status
            .as_ref()
            .map(|status| format!("Enter asks {}", state.names.worker(&status.main_agent))),
    }
}

/// Whether Enter sends a draft to a run rather than to the default agent.
fn run_takes_text(state: &AppState) -> bool {
    match &state.mode {
        Some(AppMode::Working { .. } | AppMode::Pausing { .. } | AppMode::Parked { .. }) => true,
        Some(AppMode::Answering { .. } | AppMode::Idle) => false,
        None => state.attached_run().is_some_and(|run| run.is_active()),
    }
}

/// Added to the exit hint: runs keep going under their supervisor after the app leaves.
fn exit_note(state: &AppState) -> Option<String> {
    match state.runs.values().filter(|run| run.is_active()).count() {
        0 => None,
        1 => Some("the run keeps going".to_owned()),
        runs => Some(format!("{runs} runs keep going")),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::{AppEvent, ChatEvent};
    use argonauts_core::events::run::{ParkReason, RunMode, RunPhase};
    use argonauts_core::events::workflow::StepEnd;
    use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};
    use argonauts_core::names::{TurnId, VendorId, WorkflowId};
    use argonauts_core::workflow::{StandardPhase, StepKind, StepRef};
    use argonauts_core::{AgentEvent, RunId};

    use super::*;
    use crate::state::{StepState, WorkflowState};

    /// A queued message reads as you typed it, without the frame the agents get with it.
    #[test]
    fn a_queued_message_reads_as_you_typed_it() {
        assert_eq!(
            own_words("Guidance m-001 from you:\nkeep the date format ISO only"),
            "keep the date format ISO only"
        );
        assert_eq!(
            own_words("Message m-002 from you, about m-001:\nand the tests"),
            "and the tests"
        );
        assert_eq!(own_words("keep: ISO\nonly"), "keep: ISO\nonly");
        assert_eq!(own_words("plain words"), "plain words");
    }

    #[test]
    fn a_run_parked_at_a_card_still_takes_text() {
        let run = RunId::new("run-20260930-100000-a1b2").unwrap();
        let mut state = AppState::default();
        assert!(!run_takes_text(&state));
        for (mode, takes) in [
            (AppMode::Idle, false),
            (
                AppMode::Answering {
                    turn: TurnId::numbered(1),
                },
                false,
            ),
            (AppMode::Working { run: run.clone() }, true),
            (AppMode::Pausing { run: run.clone() }, true),
            (
                AppMode::Parked {
                    run: run.clone(),
                    reason: ParkReason::Checkpoint,
                },
                true,
            ),
        ] {
            state.mode = Some(mode.clone());
            assert_eq!(run_takes_text(&state), takes, "{mode:?}");
        }
    }

    /// The input's rule names your preset, what your next message would use, so it keeps
    /// its name while a run follows a workflow the lead picked, or runs solo.
    #[test]
    fn the_input_rule_keeps_your_preset_whatever_the_run_follows() {
        let id = RunId::new("run-20260930-100000-a1b2").unwrap();
        let mut state = AppState::default();
        state.status = Some(argonauts_core::app::StatusLine {
            title: String::new(),
            repo: "~/code".into(),
            workflow: "council".into(),
            ultracode: true,
            run: None,
            main_agent: "claude-code".parse().unwrap(),
            quotas: Vec::new(),
            line: Default::default(),
            spent: None,
            limit: None,
            narrator: NarratorMode::Off,
            approval_mode: CoreApproval::Supervised,
            awake: None,
        });
        let mut run = crate::state::RunState::new(id.clone());
        run.mode = Some(RunMode::Crew);
        run.workflow = Some("council".into());
        run.workflows
            .push(workflow(1, "maker-checker", &["Build"], None));
        state.runs.insert(id.clone(), run);
        state.mode = Some(AppMode::Working { run: id.clone() });
        assert_eq!(
            composer_label(&state).as_deref(),
            Some("Council · Claude Code")
        );

        state.runs.get_mut(&id).unwrap().mode = Some(RunMode::Solo);
        assert_eq!(
            composer_label(&state).as_deref(),
            Some("Council · Claude Code")
        );
    }

    /// The rule's label: the model by name alone, the workflow first when it isn't solo, and
    /// ultracode as the rule's own tag, never a word in the label.
    #[test]
    fn the_input_rule_names_the_main_model_with_the_workflow_first() {
        let mut state = AppState::default();
        let status = |workflow: &str, agent: &str| argonauts_core::app::StatusLine {
            title: String::new(),
            repo: "~/code".into(),
            workflow: workflow.into(),
            ultracode: true,
            run: None,
            main_agent: agent.parse().unwrap(),
            quotas: Vec::new(),
            line: Default::default(),
            spent: None,
            limit: None,
            narrator: NarratorMode::Off,
            approval_mode: CoreApproval::Supervised,
            awake: None,
        };
        state.status = Some(status("solo", "claude-code/claude-opus-5-5"));
        assert_eq!(composer_label(&state).as_deref(), Some("Opus 5.5"));
        state.status = Some(status("council", "claude-code/claude-opus-5-5"));
        assert_eq!(
            composer_label(&state).as_deref(),
            Some("Council · Opus 5.5")
        );
        // After /model the rule names the new model, and nothing else says which CLI.
        state.status = Some(status("solo", "codex/gpt-6.1-sol"));
        let label = composer_label(&state).unwrap();
        assert!(
            !label.contains("Codex") && !label.contains("solo"),
            "{label}"
        );
        // While nobody knows the model, the CLI's name stands in for it.
        state.status = Some(status("solo", "codex"));
        assert_eq!(composer_label(&state).as_deref(), Some("Codex"));
    }

    fn workflow(id: u32, name: &str, phases: &[&str], parent: Option<StepRef>) -> WorkflowState {
        WorkflowState {
            id: WorkflowId::numbered(id),
            name: name.into(),
            from: Some("council".into()),
            description: String::new(),
            phases: phases.iter().map(|phase| (*phase).to_owned()).collect(),
            parent,
            steps: Vec::new(),
        }
    }

    fn step(workflow: u32, phase: u32, done: bool) -> StepState {
        StepState {
            step: StepRef {
                workflow: WorkflowId::numbered(workflow),
                phase,
                path: vec![0],
            },
            kind: StepKind::Restate,
            standard: StandardPhase::Understand,
            round: 1,
            end: done.then_some(StepEnd::Done),
            summary: None,
        }
    }

    #[test]
    fn the_working_line_names_only_the_phases_the_workflow_view_lists() {
        let mut run = RunState::new(RunId::new("run-01").unwrap());
        run.phase = Some(RunPhase::Contracts);
        // No workflow yet: the run's own stage is all there is to name.
        assert_eq!(run_details(&run), ["Contracts"]);

        // The goal's alignment runs nested under the workflow to come.
        let under_own = StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        };
        let mut alignment = workflow(2, "goal", &["Goal"], Some(under_own));
        alignment.steps.push(step(2, 0, false));
        run.workflows.push(alignment);
        run.last_step = Some(step(2, 0, false).step);
        assert_eq!(run_details(&run), ["Goal"]);

        // Once the run's own workflow runs, its phases name the line, with a nested
        // workflow's steps counted in the phase they run in.
        let mut own = workflow(1, "council", &["Goal", "Plan", "Build", "Merge"], None);
        own.steps.push(step(1, 0, true));
        own.steps.push(step(1, 1, false));
        run.workflows.insert(0, own);
        run.workflows[1].steps[0].end = Some(StepEnd::Done);
        run.last_step = Some(step(1, 1, false).step);
        assert_eq!(run_details(&run), ["Plan"]);

        // Between steps, the phase whose step started last.
        run.workflows[0].steps[1].end = Some(StepEnd::Done);
        assert_eq!(run_details(&run), ["Plan"]);
    }

    #[test]
    fn the_working_line_fades_on_a_reported_stall_until_the_agent_speaks() {
        let turn = TurnId::numbered(1);
        let mut state = AppState::default();
        state.mode = Some(AppMode::Answering { turn: turn.clone() });
        state.chat.turn = Some(turn.clone());
        let agent = |state: &mut AppState, event: AgentEvent| {
            state.apply(&AppEvent::Chat {
                event: ChatEvent::Agent {
                    turn: turn.clone(),
                    event,
                },
            });
        };
        let t0 = Instant::now();
        let mut bottom = crate::bottom::BottomState::new();
        let mut stalled_since = |state: &AppState, secs: u64| {
            let now = t0 + Duration::from_secs(secs);
            sync_working(state, &mut bottom, || "Rowing", now);
            let line = bottom.working.as_ref().expect("a working line");
            line.stalled_since
        };

        // Ten quiet minutes of thinking are no stall.
        agent(&mut state, AgentEvent::Reasoning { text: "hm".into() });
        assert_eq!(stalled_since(&state, 0), None);
        assert_eq!(stalled_since(&state, 600), None);

        agent(
            &mut state,
            AgentEvent::Error {
                category: argonauts_core::ErrorCategory::Stalled,
                message: "Claude Code has sent nothing for 45 seconds, not even its heartbeat"
                    .into(),
                retryable: true,
            },
        );
        // The fade runs from the stall on.
        let stalled_at = Some(t0 + Duration::from_secs(645));
        assert_eq!(stalled_since(&state, 645), stalled_at);
        assert_eq!(stalled_since(&state, 650), stalled_at);

        // Anything from the agent clears it.
        agent(
            &mut state,
            AgentEvent::TextDelta {
                text: "Done.".into(),
                parent: None,
            },
        );
        assert_eq!(stalled_since(&state, 651), None);
    }

    /// The working line offers Esc only for what Esc stops: a chat turn, a turn
    /// answered while a run works, and a run that's pausing. A run's own agents keep going.
    /// While the main agent rests and only its CLI's subagents work, there is no line at all.
    #[test]
    fn the_working_line_offers_esc_only_for_what_esc_stops() {
        let run = RunId::new("run-01").unwrap();
        let turn = TurnId::numbered(1);
        let t0 = Instant::now();
        let synced = |state: &AppState| {
            let mut bottom = crate::bottom::BottomState::new();
            sync_working(state, &mut bottom, || "Rowing", t0);
            bottom.working.map(|line| line.esc_stops)
        };
        let mut state = AppState::default();
        state.runs.insert(run.clone(), RunState::new(run.clone()));
        state.mode = Some(AppMode::Working { run: run.clone() });
        assert_eq!(synced(&state), Some(false), "a run alone");
        state.chat.turn = Some(turn.clone());
        assert_eq!(synced(&state), Some(true), "a turn answered meanwhile");
        state.chat.turn = None;
        state.mode = Some(AppMode::Pausing { run });
        assert_eq!(synced(&state), Some(true), "a pausing run stops now");

        let mut state = AppState::default();
        state.mode = Some(AppMode::Answering { turn: turn.clone() });
        state.chat.turn = Some(turn.clone());
        assert_eq!(synced(&state), Some(true), "a chat turn");
        let agent = |event| AppEvent::Chat {
            event: ChatEvent::Agent {
                turn: turn.clone(),
                event,
            },
        };
        state.apply(&agent(AgentEvent::Subagent {
            update: argonauts_core::events::session::SubagentUpdate {
                id: "toolu_a".into(),
                parent: None,
                kind: argonauts_core::events::session::SubagentKind::Subagent,
                label: Some("Count slowly".into()),
                prompt: None,
                status: argonauts_core::events::session::SubagentStatus::Running,
                last_message: None,
                log_uri: None,
                steps_visible: true,
                tokens: None,
            },
        }));
        state.apply(&agent(AgentEvent::BackgroundTasks {
            running: vec!["task-a".into()],
            ended: false,
        }));
        state.apply(&agent(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        }));
        assert_eq!(synced(&state), None, "only its subagents work");
    }

    #[test]
    fn a_chat_turns_quota_reading_is_kept_for_the_bottom_line() {
        let wall = OffsetDateTime::now_utc();
        let mut state = AppState::default();
        assert!(state.quotas.is_empty());
        let reading = QuotaReading {
            vendor: VendorId("claude-code".into()),
            status: QuotaStatus::Allowed,
            used: Some(0.71),
            resets_at: None,
            window: Some("five_hour".into()),
            source: QuotaSource::RateLimitEvent,
            observed_at: wall,
            detail: None,
            wrap_up: false,
        };
        state.apply(&AppEvent::Chat {
            event: ChatEvent::Agent {
                turn: TurnId::numbered(1),
                event: AgentEvent::Quota {
                    reading: reading.clone(),
                },
            },
        });
        assert_eq!(state.quotas["claude-code"], [reading]);
    }

    fn status_with(awake: Option<AwakeHold>) -> argonauts_core::app::StatusLine {
        argonauts_core::app::StatusLine {
            title: String::new(),
            repo: "~/code".into(),
            workflow: "council".into(),
            ultracode: false,
            run: None,
            main_agent: "claude-code".parse().unwrap(),
            quotas: Vec::new(),
            line: Default::default(),
            spent: None,
            limit: None,
            narrator: NarratorMode::Off,
            approval_mode: CoreApproval::Supervised,
            awake,
        }
    }

    /// The words the bottom line's `awake` part shows at `wall`, if any.
    fn awake_words(state: &AppState, wall: OffsetDateTime) -> Option<String> {
        let clock = bottom_line::Clock {
            now: wall,
            utc_offset: UtcOffset::UTC,
        };
        match awake_part(state, &clock)? {
            PartBody::Words { text, tone } => {
                assert_eq!(tone, Tone::Code);
                Some(text)
            }
            other => panic!("not words: {other:?}"),
        }
    }

    #[test]
    fn the_bottom_line_names_a_hold_on_sleep_you_asked_for() {
        let wall = time::macros::datetime!(2026-10-07 13:45 UTC);
        let words = |awake: Option<AwakeHold>| {
            let mut state = AppState::default();
            state.status = Some(status_with(awake));
            awake_words(&state, wall)
        };
        assert_eq!(
            words(Some(AwakeHold::UntilQuit)).as_deref(),
            Some("☕ awake until you quit")
        );
        let later = wall + time::Duration::hours(2);
        assert_eq!(
            words(Some(AwakeHold::Until { at: later })).as_deref(),
            Some("☕ awake until 3:45pm")
        );
        // A time that's up, or no hold of yours, shows nothing.
        assert_eq!(words(Some(AwakeHold::Until { at: wall })), None);
        assert_eq!(words(None), None);
    }

    #[test]
    fn the_ascii_glyph_set_drops_the_cup() {
        let mut state = AppState::default();
        state.ui = Some(UiSettings {
            glyphs: argonauts_core::settings::ui::Glyphs::Ascii,
            ..UiSettings::default()
        });
        state.status = Some(status_with(Some(AwakeHold::UntilQuit)));
        let wall = time::macros::datetime!(2026-10-07 13:45 UTC);
        assert_eq!(
            awake_words(&state, wall).as_deref(),
            Some("awake until you quit")
        );
    }
}
