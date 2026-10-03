//! The live region's own rows from the app state: the working line, the narrator's live line,
//! queued messages, the mode line, the status line with its quota chips, and the title.

use std::time::{Duration, Instant};

use argonauts_core::app::{AppMode, QuotaChip};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::card::CardState;
use argonauts_core::events::run::{Party, RunMode, RunPhase};
use argonauts_core::events::session::{Delivery, QueuedMessage};
use argonauts_core::input::MentionTarget;
use argonauts_core::limits::{HealthState, QuotaStatus};
use argonauts_core::settings::approvals::ApprovalMode as CoreApproval;
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::ui::{NarratorMode, VerbTheme};
use argonauts_core::{KeyScheme, Spend};
use time::{OffsetDateTime, UtcOffset};

use crate::bottom::verbs::VerbStyle;
use crate::bottom::{Activity, ApprovalMode, ModeLine, StatusPart, VimMode, WorkingLine};
use crate::cell::Tone;
use crate::format::{self, ClockTime};
use crate::state::{AppState, ChatState, Doing, RunState, binding_reading};

use super::Cx;

/// A quota reading older than this is drawn dim, since the vendor may have moved on.
pub const QUOTA_STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// A chip turns the warning color here, and never gives way on a narrow screen.
const QUOTA_WARN: f64 = 0.9;

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

    apply_keys(&mut bottom.mode, keys);
    if let Some(approval) = controller_approval(state)
        && bottom.synced.approval != Some(approval)
    {
        bottom.mode.approval = approval;
        bottom.synced.approval = Some(approval);
    }
    bottom.mode.background_agents = state.agents_running();
    let on_screen = usize::from(bottom.card.is_some());
    bottom.mode.needs_you = state.needs_you().len().saturating_sub(on_screen);

    let wall = OffsetDateTime::now_utc();
    bottom.status_line = status_parts(state, wall);
    bottom.enter_sends = enter_sends(state);
    bottom.run_takes_text = run_takes_text(state);
    bottom.shell_running = state.shell.is_some();
    bottom.exit_note = exit_note(state);
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
    match &state.mode {
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
        }
        Work::Pausing(_) => {
            line.tokens_down = 0;
            line.activity = Activity::None;
            line.details = vec!["esc again to stop now".to_owned()];
            line.timed = false;
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
        details.push(format!("{done}/{total} cards"));
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
        RunPhase::Retro => "Retro",
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
            texts.push(message.text.clone());
            places.push(destination(run, message, &state.names));
        }
    }
    for text in &state.chat.queued {
        texts.push(text.clone());
        places.push(None);
    }
    (texts, places)
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

/// The run's workflow as its panel row names it, `solo` for a solo run, or the preset a new
/// run would use, with `all hands` after it while that's on.
fn composer_label(state: &AppState) -> Option<String> {
    let label = workflow_label(state);
    let all_hands = state.status.as_ref().is_some_and(|status| status.all_hands);
    match label {
        Some(label) if all_hands => Some(format!("{label} · {ALL_HANDS_LABEL}")),
        None if all_hands => Some(ALL_HANDS_LABEL.to_owned()),
        label => label,
    }
}

/// All hands on the input rule, lowercase like the preset beside it.
const ALL_HANDS_LABEL: &str = "all hands";

fn workflow_label(state: &AppState) -> Option<String> {
    if let Some(run) = focus_run(state) {
        if run.mode == Some(RunMode::Solo) {
            return Some("solo".to_owned());
        }
        if let Some(workflow) = super::panel::workflow_name(run) {
            return Some(workflow.to_owned());
        }
    }
    state
        .status
        .as_ref()
        .map(|status| status.preset.clone())
        .filter(|preset| !preset.is_empty())
        .or_else(|| state.welcome.as_ref().map(|welcome| welcome.preset.clone()))
        .filter(|preset| !preset.is_empty())
}

/// The run plain text and the budget meter are about: the one the app mode names, else the
/// attached one.
fn focus_run(state: &AppState) -> Option<&RunState> {
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
        CoreApproval::Shadow => ApprovalMode::Shadow,
        CoreApproval::Auto => ApprovalMode::Auto,
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

/// Only what the startup header lacks: the quota chips, the budget meter, and the narrator's
/// spend, with the default agent once it differs from the header's. The preset never shows,
/// since the input's top rule names it.
fn status_parts(state: &AppState, wall: OffsetDateTime) -> Vec<StatusPart> {
    let mut parts = Vec::new();
    if let Some(status) = &state.status {
        let header = state.welcome.as_ref();
        if header.and_then(|welcome| welcome.default_agent.as_ref()) != Some(&status.default_agent)
        {
            parts.push(StatusPart::model(state.names.worker(&status.default_agent)));
        }
    }
    let facts = |chip: &QuotaChip| ChipFacts {
        now: wall,
        utc_offset: state.terminal.utc_offset.unwrap_or(UtcOffset::UTC),
        spend_usd: vendor_spend(state, &chip.label),
    };
    for chip in chips(state) {
        parts.extend(quota_part(&chip, &facts(&chip)));
    }
    parts.extend(budget_meter(state));
    parts.extend(narrator_spend(state));
    parts
}

/// The controller's chips with newer readings and health from the run's events folded in. A
/// vendor with more than one chip (agy splits by model family) keeps the controller's.
fn chips(state: &AppState) -> Vec<QuotaChip> {
    let mut chips = state
        .status
        .as_ref()
        .map(|status| status.quotas.clone())
        .unwrap_or_default();
    let vendor_of = |chip: &QuotaChip| {
        chip.reading
            .as_ref()
            .map(|reading| reading.vendor.0.clone())
            .or_else(|| vendor_for_label(&chip.label).map(str::to_owned))
    };
    let vendors: Vec<Option<String>> = chips.iter().map(vendor_of).collect();
    for (chip, vendor) in chips.iter_mut().zip(&vendors) {
        let Some(vendor) = vendor else { continue };
        if vendors
            .iter()
            .filter(|other| other.as_ref() == Some(vendor))
            .count()
            > 1
        {
            continue;
        }
        if let Some(readings) = state.quotas.get(vendor) {
            let mut all = readings.clone();
            all.extend(chip.reading.clone());
            chip.reading = binding_reading(&all).cloned();
        }
        if let Some(health) = state.health.get(vendor) {
            chip.health = health.state;
        }
    }
    for (vendor, readings) in &state.quotas {
        if vendors.iter().any(|known| known.as_ref() == Some(vendor)) {
            continue;
        }
        chips.push(QuotaChip {
            label: label_for_vendor(vendor),
            reading: binding_reading(readings).cloned(),
            health: state
                .health
                .get(vendor)
                .map_or(HealthState::Healthy, |health| health.state),
        });
    }
    chips
}

/// What a chip can't say about itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipFacts {
    pub now: OffsetDateTime,
    pub utc_offset: UtcOffset,
    /// Spend on that vendor, for an agent without a quota.
    pub spend_usd: Option<f64>,
}

/// `Claude 5h 71% used`, `Codex down`, `agy out until 3:45 PM`, `OpenRouter $0.06`, or `Local`,
/// with the tone for 90% used and a stale reading. A quota not read yet, or not readable, shows
/// nothing, since a chip like `Codex ?` only takes room.
pub fn quota_part(chip: &QuotaChip, facts: &ChipFacts) -> Option<StatusPart> {
    let label = chip.label.trim();
    if chip.health == HealthState::Down {
        return Some(StatusPart::new(format!("{label} down"), Tone::Warning));
    }
    let Some(reading) = &chip.reading else {
        if label == "Local" {
            return Some(StatusPart::no_quota(label));
        }
        if has_quota(label) {
            return None;
        }
        return Some(match facts.spend_usd.filter(|spend| *spend > 0.0) {
            Some(spend) => StatusPart::no_quota(format!("{label} {}", format::usd(spend))),
            None => StatusPart::no_quota(label),
        });
    };
    match reading.status {
        QuotaStatus::Rejected => {
            let text = match reading.resets_at {
                Some(at) => format!("{label} out until {}", clock_time(at, facts)),
                None => format!("{label} out"),
            };
            return Some(StatusPart::new(text, Tone::Warning));
        }
        QuotaStatus::Unknown => return None,
        QuotaStatus::Allowed | QuotaStatus::Warning => {}
    }
    let window = reading.window.as_deref().map(window_name);
    let name = match window.as_deref() {
        Some(window) if !window.is_empty() => format!("{label} {window}"),
        _ => label.to_owned(),
    };
    let stale = Duration::try_from(facts.now - reading.observed_at)
        .is_ok_and(|age| age > QUOTA_STALE_AFTER);
    let Some(used) = reading.used else {
        // Some readings say only that the vendor warned, not how much is used.
        return Some(match reading.status {
            QuotaStatus::Warning => StatusPart::new(format!("{name} near limit"), Tone::Warning),
            _ if stale => StatusPart::stale(format!("{name} ok"), 0),
            _ => StatusPart::quota(format!("{name} ok"), 0, Tone::Normal),
        });
    };
    let used = used.clamp(0.0, 1.0);
    let percent = (used * 100.0).round() as u8;
    let text = format!("{name} {percent}% used");
    let warn = used >= QUOTA_WARN;
    Some(match (stale, warn) {
        (true, true) => StatusPart::quota(text, 100, Tone::Dim),
        (true, false) => StatusPart::stale(text, percent),
        (false, true) => StatusPart::quota(text, percent.max(90), Tone::Warning),
        (false, false) => StatusPart::quota(text, percent, Tone::Normal),
    })
}

/// `3:45 PM` today, or `Mon 9:00 AM` on another day.
fn clock_time(at: OffsetDateTime, facts: &ChipFacts) -> String {
    let local = at.to_offset(facts.utc_offset);
    let clock = ClockTime::new(local.hour(), local.minute()).map_or_else(
        || format!("{:02}:{:02}", local.hour(), local.minute()),
        |t| t.to_string(),
    );
    if local.date() == facts.now.to_offset(facts.utc_offset).date() {
        clock
    } else {
        let day = local.weekday().to_string();
        format!("{} {clock}", &day[..3.min(day.len())])
    }
}

/// `five_hour` → `5h`, `seven_day` → `7d`, and other windows in plain words.
fn window_name(window: &str) -> String {
    match window.trim() {
        "five_hour" | "5h" => "5h".to_owned(),
        "seven_day" | "weekly" | "7d" => "7d".to_owned(),
        "seven_day_opus" => "7d Opus".to_owned(),
        "seven_day_sonnet" => "7d Sonnet".to_owned(),
        "daily" | "one_day" => "1d".to_owned(),
        other => other.replace('_', " "),
    }
}

/// Subscription CLIs report a quota, so a chip without a reading means the reading failed.
fn has_quota(label: &str) -> bool {
    matches!(label, "Claude" | "Codex") || label == "agy" || label.starts_with("agy ")
}

fn vendor_for_label(label: &str) -> Option<&'static str> {
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

/// What this app's runs spent on the vendor a chip names.
fn vendor_spend(state: &AppState, label: &str) -> Option<f64> {
    let vendor = vendor_for_label(label).unwrap_or(label);
    let spend: f64 = state
        .runs
        .values()
        .flat_map(|run| run.sessions.values())
        .filter(|session| session.vendor.eq_ignore_ascii_case(vendor))
        .filter_map(|session| session.cost_usd)
        .sum();
    (spend > 0.0).then_some(spend)
}

/// The run's spend against its limit, then the busiest live session's context against its
/// cap: `$0.21/$5.00 · ctx 150k/200k`.
fn budget_meter(state: &AppState) -> Vec<StatusPart> {
    let mut parts = Vec::new();
    let Some(run) = focus_run(state) else {
        if let Some(status) = &state.status
            && let Some(spent) = &status.spent
        {
            parts.push(StatusPart::spend(spend_text(
                spent,
                spent.input_tokens + spent.output_tokens,
                status.limit.as_ref(),
            )));
        }
        return parts;
    };
    let limit = run
        .budget
        .as_ref()
        .and_then(|budget| budget.limit)
        .or_else(|| state.status.as_ref().and_then(|status| status.limit));
    let tokens = run
        .budget
        .as_ref()
        .map_or(run.spent.total_tokens(), |budget| {
            budget.spent.total_tokens()
        });
    parts.push(StatusPart::spend(spend_text(
        &run.spend(),
        tokens,
        limit.as_ref(),
    )));
    let busiest = run
        .running_sessions()
        .filter_map(|session| session.context)
        .filter(|context| context.cap > 0)
        .max_by(|a, b| {
            let share = |c: &argonauts_core::views::ContextUse| c.used as f64 / c.cap as f64;
            share(a).total_cmp(&share(b))
        });
    if let Some(context) = busiest {
        let full = context.used as f64 >= context.cap as f64 * QUOTA_WARN;
        let tone = if full { Tone::Warning } else { Tone::Dim };
        parts.push(StatusPart::context(
            format!(
                "ctx {}/{}",
                format::tokens(context.used),
                format::tokens(context.cap)
            ),
            tone,
        ));
    }
    parts
}

/// `$0.21/$5.00`, `48.2k/2m tokens`, or the spend alone without a limit it can be read against.
fn spend_text(spent: &Spend, tokens: u64, limit: Option<&BudgetAmount>) -> String {
    let spent = format::spend(spent);
    match limit {
        Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount,
        }) => format!("{spent}/{}", format::usd(*amount)),
        Some(BudgetAmount {
            unit: BudgetUnit::Tokens,
            amount,
        }) => format!(
            "{}/{} tokens",
            format::tokens(tokens),
            format::tokens(amount.max(0.0) as u64)
        ),
        Some(BudgetAmount {
            unit: BudgetUnit::Minutes,
            amount,
        }) => format!("{spent} · limit {}m", amount.max(0.0).round() as u64),
        Some(BudgetAmount {
            unit: BudgetUnit::Quota,
            amount,
        }) => format!(
            "{spent} · limit {}% of quota",
            (amount.clamp(0.0, 1.0) * 100.0).round() as u64
        ),
        None => spent,
    }
}

/// `narrator $0.06/$0.50` while it's on, in the warning color once capped.
fn narrator_spend(state: &AppState) -> Option<StatusPart> {
    let status = state.narrator_status.as_ref()?;
    let on = match status.mode {
        NarratorMode::Off => false,
        NarratorMode::Live => true,
        NarratorMode::OnDemand => status.spent.cost_usd > 0.0,
    };
    if !on {
        return None;
    }
    let tone = if status.capped {
        Tone::Warning
    } else {
        Tone::Dim
    };
    Some(StatusPart::narrator(
        format!(
            "narrator {}/{}",
            format::spend(&status.spent),
            cap_text(&status.cap)
        ),
        tone,
    ))
}

/// `0.50 usd` → `$0.50`; other caps as written.
fn cap_text(cap: &str) -> String {
    let mut words = cap.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(amount), Some(unit), None) if unit.eq_ignore_ascii_case("usd") => amount
            .parse::<f64>()
            .map_or_else(|_| cap.trim().to_owned(), format::usd),
        _ => cap.trim().to_owned(),
    }
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
            .map(|status| format!("Enter asks {}", state.names.worker(&status.default_agent))),
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
    use argonauts_core::events::run::{ParkReason, RunPhase};
    use argonauts_core::events::workflow::StepEnd;
    use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};
    use argonauts_core::names::{TurnId, VendorId, WorkflowId};
    use argonauts_core::workflow::{StandardPhase, StepKind, StepRef};
    use argonauts_core::{AgentEvent, RunId};

    use super::*;
    use crate::state::{StepState, WorkflowState};

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

    /// The input's rule names the workflow the run follows, as the panel row does: the preset
    /// until the lead picks, then the pick.
    #[test]
    fn the_input_rule_names_the_workflow_the_lead_picked() {
        let id = RunId::new("run-20260930-100000-a1b2").unwrap();
        let mut state = AppState::default();
        let mut run = crate::state::RunState::new(id.clone());
        run.mode = Some(RunMode::Crew);
        run.preset = Some("peer-crew".into());
        let under_own = StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        };
        run.workflows
            .push(workflow(2, "goal", &["Goal"], Some(under_own)));
        state.runs.insert(id.clone(), run);
        state.mode = Some(AppMode::Working { run: id.clone() });
        assert_eq!(composer_label(&state).as_deref(), Some("peer-crew"));

        let run = state.runs.get_mut(&id).unwrap();
        run.workflows
            .insert(0, workflow(1, "solo-reviewed", &["Build"], None));
        assert_eq!(composer_label(&state).as_deref(), Some("solo-reviewed"));
    }

    #[test]
    fn the_input_rule_shows_all_hands_beside_the_preset_while_it_is_on() {
        let mut state = AppState::default();
        let status = |all_hands: bool| argonauts_core::app::StatusLine {
            title: String::new(),
            repo: "~/code".into(),
            preset: "peer-crew".into(),
            all_hands,
            run: None,
            default_agent: "claude-code".parse().unwrap(),
            quotas: Vec::new(),
            spent: None,
            limit: None,
            narrator: NarratorMode::Off,
            approval_mode: CoreApproval::Supervised,
        };
        state.status = Some(status(false));
        assert_eq!(composer_label(&state).as_deref(), Some("peer-crew"));
        state.status = Some(status(true));
        assert_eq!(
            composer_label(&state).as_deref(),
            Some("peer-crew · all hands")
        );

        // A solo run started with /solo still says so.
        let id = RunId::new("run-20260930-100000-a1b2").unwrap();
        let mut run = crate::state::RunState::new(id.clone());
        run.mode = Some(RunMode::Solo);
        state.runs.insert(id.clone(), run);
        state.mode = Some(AppMode::Working { run: id });
        assert_eq!(composer_label(&state).as_deref(), Some("solo · all hands"));
    }

    fn workflow(id: u32, name: &str, phases: &[&str], parent: Option<StepRef>) -> WorkflowState {
        WorkflowState {
            id: WorkflowId::numbered(id),
            name: name.into(),
            from: Some("peer-crew".into()),
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
        let mut own = workflow(1, "peer-crew", &["Goal", "Plan", "Build", "Merge"], None);
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

    #[test]
    fn a_chat_turns_quota_reading_shows_on_the_mode_line() {
        let wall = OffsetDateTime::now_utc();
        let mut state = AppState::default();
        assert!(status_parts(&state, wall).is_empty());
        let reading = QuotaReading {
            vendor: VendorId("claude-code".into()),
            status: QuotaStatus::Allowed,
            used: Some(0.71),
            resets_at: None,
            window: Some("five_hour".into()),
            source: QuotaSource::RateLimitEvent,
            observed_at: wall,
            detail: None,
        };
        state.apply(&AppEvent::Chat {
            event: ChatEvent::Agent {
                turn: TurnId::numbered(1),
                event: AgentEvent::Quota { reading },
            },
        });
        let parts: Vec<String> = status_parts(&state, wall)
            .iter()
            .map(|part| part.text.clone())
            .collect();
        assert_eq!(parts, ["Claude 5h 71% used"]);
    }
}
