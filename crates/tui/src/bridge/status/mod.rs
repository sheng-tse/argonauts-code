//! The live region's own rows from the app state: the working line, the narrator's live line,
//! queued messages, the input rule's label, the bottom line, and the title. The bottom line's
//! parts are [`bottom_line`]'s.

use std::time::{Duration, Instant};

use argonauts_core::agents::runtimes::runtime;
use argonauts_core::app::{AppMode, AwakeHold, ChoiceKind, CommandWork};
use argonauts_core::input::MentionTarget;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::run::{Party, RunPhase};
use argonauts_core::runs::events::session::{Delivery, QueuedMessage};
use argonauts_core::runs::events::task::TaskState;
use argonauts_core::settings::approvals::ApprovalMode as CoreApproval;
use argonauts_core::settings::bottom_line::AWAKE;
use argonauts_core::settings::ui::{NarratorMode, Notices, UiSettings, VerbTheme};
use argonauts_core::{KeyScheme, TodoStatus};
use time::UtcOffset;

use crate::base::format;
use crate::base::input::keymap::{Action, Keymap};
use crate::bottom::line::PartBody;
use crate::bottom::working::verbs::VerbStyle;
use crate::bottom::{Activity, ApprovalMode, BottomRow, BottomState, Notice, VimMode, WorkingLine};
use crate::cells::cell::Tone;
use crate::frame::state::{AppState, ConversationState, Doing, RunState};

use super::{Cx, bottom_line};

#[cfg(test)]
mod workflow_tests;

/// Brings every row under the transcript in line with the state. Runs after every event and
/// action, so it must be cheap and idempotent.
pub fn sync(cx: &mut Cx<'_>) {
    let state = cx.state;
    let keys = cx.out.vim.map_or(cx.settings.keys, |on| {
        if on {
            KeyScheme::Vim
        } else {
            KeyScheme::Default
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
    let stretch = bottom.synced.work.clone();
    sync_working(
        state,
        bottom,
        |work| match work {
            Some(work) => picker.command(work, verbs),
            None => picker.working(verbs),
        },
        now,
    );
    // A new stretch of work may get a tip, which stays while it goes on.
    let fresh = bottom.synced.work != stretch;
    if let Some(line) = bottom.working.as_mut() {
        if !cx.settings.tips {
            line.tip = None;
        } else if fresh {
            line.tip = picker.tip();
        }
    }
    bottom.narrator = narrator_live.then(|| live_narration(state)).flatten();
    let (queued, queued_where) = queued(state);
    bottom.queued = queued;
    bottom.queued_where = queued_where;
    if let Some(label) = composer_label(state) {
        bottom.composer.label = Some(label);
    }
    bottom.composer.ultracode = state.status.as_ref().is_some_and(|status| status.ultracode);
    // What counts as a real command beside Argonauts Code's own, copied only when it changes.
    if bottom.composer.cli_commands.as_ref() != state.cli_commands.as_ref() {
        bottom.composer.cli_commands = state.cli_commands.clone();
    }
    let mod_commands = crate::mods::own_command_names(state.mods.as_ref());
    if bottom.composer.mod_commands != mod_commands {
        bottom.composer.mod_commands = mod_commands;
    }
    bottom.workflow_key = workflow_key(cx.settings);

    apply_keys(&mut bottom.mode, keys);
    if let Some(approval) = controller_approval(state)
        && bottom.synced.approval != Some(approval)
    {
        bottom.mode.approval = approval;
        bottom.synced.approval = Some(approval);
    }
    bottom.mode.background_agents = state.agents_running();
    let on_screen = usize::from(bottom.question.is_some());
    bottom.mode.needs_you =
        state.needs_you().len().saturating_sub(on_screen) + state.background_waiting();

    bottom.send_now = state.conversation.send_now && state.conversation.turn.is_some();
    bottom.send_now_hint = send_now_hint(state, cx.settings);
    sync_notice(state, cx.settings, bottom);

    let now = argonauts_core::base::clock::now_utc();
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
    let waiting = state.conversation.send_now
        && state.conversation.turn.is_some()
        && !state.conversation.queued.is_empty();
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
fn sync_notice(state: &AppState, settings: &UiSettings, bottom: &mut BottomState) {
    let drawn = state
        .conversation
        .notice
        .as_ref()
        .filter(|_| settings.notices == Notices::WhenEnabled);
    match drawn {
        Some(drawn) if bottom.notice.drawn != *drawn => bottom.notice.redraw(drawn.clone()),
        Some(_) => {}
        None => bottom.notice = Notice::default(),
    }
}

/// Vim keys start in INSERT, and the screen switches modes itself.
pub fn apply_keys(mode: &mut BottomRow, keys: KeyScheme) {
    match keys {
        KeyScheme::Vim if mode.vim.is_none() => mode.vim = Some(VimMode::Insert),
        KeyScheme::Vim => {}
        KeyScheme::Default => mode.vim = None,
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
    Conversation(&'a ConversationState),
    Run(&'a RunState),
    Pausing(&'a RunState),
    /// A command you typed at work in the background, such as `/update`.
    Command(CommandWork),
}

impl Work<'_> {
    /// Whether the agent this work follows is writing a workflow's script. Nothing of the
    /// workflow shows until the script exists, so the working line is where that's said. A
    /// crew counts only while one agent runs, as its words do.
    fn writes_a_workflow(&self) -> bool {
        match self {
            Work::Conversation(conversation) => {
                let cli = conversation
                    .worker
                    .as_ref()
                    .map_or("", |worker| worker.agent.as_str());
                conversation
                    .being_written
                    .iter()
                    .any(|(_, tool)| super::cli_workflow_run::is_call(cli, tool))
            }
            // Once the run's agents have begun and until the shape is known, the workflow is
            // being written: a CLI's Workflow call, a main agent's design, a script's planner
            // and the runner that starts the script.
            Work::Run(run) if !run.shape_known() => run.sessions_in_order().next().is_some(),
            Work::Run(run) => {
                let mut running = run.running_sessions();
                match (running.next(), running.next()) {
                    (Some(only), None) => only.tools.iter().any(|tool| {
                        tool.is_pending()
                            && super::cli_workflow_run::is_call(&only.worker.agent, &tool.name)
                    }),
                    _ => false,
                }
            }
            Work::Pausing(_) | Work::Command(_) => false,
        }
    }

    fn key(&self) -> String {
        match self {
            Work::Conversation(conversation) => {
                format!(
                    "turn:{}",
                    conversation.turn.as_ref().map_or("", |turn| turn.as_str())
                )
            }
            Work::Run(run) => format!("run:{}", run.id),
            Work::Pausing(run) => format!("pausing:{}", run.id),
            Work::Command(work) => format!("{COMMAND_KEY}{work:?}"),
        }
    }

    /// Whether Argonauts Code said the stream stalled. A crew's run stalls only once every agent
    /// still running has, since one at work keeps the run going.
    fn stalled(&self) -> bool {
        match self {
            Work::Conversation(conversation) => conversation.stalled,
            Work::Run(run) => {
                let mut running = run.running_sessions().peekable();
                running.peek().is_some() && running.all(|session| session.stalled)
            }
            Work::Pausing(_) | Work::Command(_) => false,
        }
    }

    /// A number that changes whenever the agents this work follows send anything, so the line
    /// can tell how long they've been quiet. A crew counts as heard from when any agent is.
    fn pulse(&self) -> Option<u64> {
        match self {
            Work::Conversation(conversation) => Some(conversation.heard),
            Work::Run(run) => run
                .running_sessions()
                .map(|session| session.last_event.unix_timestamp_nanos() as u64)
                .max(),
            Work::Pausing(_) | Work::Command(_) => None,
        }
    }

    /// The task in hand and the one after it, from the CLI's own task list. A crew's agents
    /// each keep a list, so only a lone agent's shows.
    fn tasks(&self) -> (Option<String>, Option<String>) {
        let todos = match self {
            Work::Conversation(conversation) => conversation.todos.as_slice(),
            Work::Run(run) => {
                let mut running = run.running_sessions();
                match (running.next(), running.next()) {
                    (Some(only), None) => only.todos.as_slice(),
                    _ => &[],
                }
            }
            Work::Pausing(_) | Work::Command(_) => &[],
        };
        let first = |status: TodoStatus| {
            todos
                .iter()
                .find(|todo| todo.status == status && !todo.text.trim().is_empty())
                .map(|todo| todo.text.clone())
        };
        (first(TodoStatus::InProgress), first(TodoStatus::Pending))
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
        Some(AppMode::Answering { .. }) if state.conversation.waits_on_background() => None,
        Some(AppMode::Answering { .. }) => Some(Work::Conversation(&state.conversation)),
        Some(AppMode::Working { run }) => run_work(run),
        Some(AppMode::Pausing { run }) => state.runs.get(run).map(Work::Pausing),
        Some(AppMode::Idle | AppMode::Waiting { .. }) => state.command_work.map(Work::Command),
        None if state.conversation.turn.is_some() => Some(Work::Conversation(&state.conversation)),
        None => {
            let Some(run) = state.attached_run().filter(|run| run.is_active()) else {
                return state.command_work.map(Work::Command);
            };
            run_work(&run.id)
        }
    }
}

/// A question waits on your answer, and neither an agent nor Argonauts Code's checks or merges are
/// at work on the run, so nothing is going on but the wait. The line steps aside, as Claude Code's
/// spinner does while a permission prompt is up, and comes back with the run's own clock once you
/// answer.
fn waits_on_you(run: &RunState) -> bool {
    let task_at_work = run.tasks.values().any(|task| {
        matches!(
            task.state,
            Some(
                TaskState::Running | TaskState::Checking | TaskState::InReview | TaskState::Merging
            )
        )
    });
    run.approvals.iter().any(|open| open.opened.needs_you)
        && run.running_sessions().next().is_none()
        && !task_at_work
}

/// How the key of a command's work begins, so the line keeps its clock from one kind of the
/// command's work to the next.
const COMMAND_KEY: &str = "command:";

/// `pick` gives the word for a new line: a turn's, or the one that fits a command's work.
fn sync_working(
    state: &AppState,
    bottom: &mut crate::bottom::BottomState,
    pick: impl FnOnce(Option<CommandWork>) -> &'static str,
    now: Instant,
) {
    let work = current_work(state);
    bottom.waits_for = waits_for_workflows(state, work.as_ref()).map(|count| {
        let noun = if count == 1 { "workflow" } else { "workflows" };
        format!("Waiting for {count} {noun} to finish")
    });
    let Some(work) = work.filter(|_| bottom.waits_for.is_none()) else {
        bottom.working = None;
        bottom.synced.work = None;
        return;
    };
    let key = work.key();
    let same = bottom.synced.work.as_deref() == Some(key.as_str());
    let last = bottom.working.take();
    // `/update` checks once it has updated: the word changes, the clock goes on.
    let command_goes_on = key.starts_with(COMMAND_KEY)
        && bottom
            .synced
            .work
            .as_deref()
            .is_some_and(|was| was.starts_with(COMMAND_KEY));
    let since = last
        .as_ref()
        .filter(|_| command_goes_on)
        .map(|line| line.started_at);
    let previous = last.filter(|_| same);

    let mut line = match &previous {
        Some(line) => line.clone(),
        None => WorkingLine::new(
            verb_for(&work, pick),
            since.unwrap_or_else(|| started_at(&work, now)),
        ),
    };
    (line.task, line.next) = work.tasks();
    match work.pulse() {
        // Anything new from the agent counts as hearing from it.
        Some(pulse) if previous.is_none() || line.pulse != pulse => {
            line.pulse = pulse;
            line.heard_at = Some(now);
        }
        Some(_) => {}
        None => line.heard_at = None,
    }
    match &work {
        Work::Conversation(conversation) => {
            line.tokens_down = conversation.tokens_out;
            line.activity = activity(
                previous.as_ref().map(|l| &l.activity),
                conversation.doing,
                now,
            );
            line.effort = state
                .status
                .as_ref()
                .and_then(|status| status.line.main_effort.clone());
            line.details.clear();
            line.hide_while_text_streams = true;
            line.esc_stops = true;
            line.retry = crate::bottom::RetryNote::follow(line.retry, conversation, now);
        }
        Work::Run(run) => {
            line.tokens_down = run.tokens_out();
            let doing = doing_between(run.running_sessions().map(|session| session.doing));
            line.activity = activity(previous.as_ref().map(|l| &l.activity), doing, now);
            // A lone agent's words read like a conversation turn's; a crew's would jump between
            // agents.
            line.says_activity = run.running_sessions().nth(1).is_none();
            line.details = run_details(run);
            // Esc leaves a run's agents going. It still stops a conversation turn answered
            // meanwhile.
            line.esc_stops = state.conversation.turn.is_some();
        }
        Work::Pausing(_) => {
            line.tokens_down = 0;
            line.activity = Activity::None;
            line.details = vec!["esc again to stop now".to_owned()];
            line.timed = false;
            line.esc_stops = true;
        }
        // Nothing an agent sends: the time alone. Esc stops `/update` and leaves Doctor's
        // check after it to finish.
        Work::Command(work) => {
            line.tokens_down = 0;
            line.activity = Activity::None;
            line.details.clear();
            line.esc_stops = *work == CommandWork::Updating;
        }
    }
    // A stall Argonauts Code reported turns the line red whatever the agent seemed to be doing.
    line.stalled_since = if work.stalled() {
        line.stalled_since.or(Some(now))
    } else {
        None
    };
    say_what_is_written(&mut line, work.writes_a_workflow());
    bottom.working = Some(line);
    bottom.synced.work = Some(key);
}

/// How many workflows the work waits for while nothing else of it goes on: its agents' turns
/// are over and the workflows their CLIs run in the background haven't ended. The
/// conversation's own CLI waits as a run's agents do.
fn waits_for_workflows(state: &AppState, work: Option<&Work<'_>>) -> Option<usize> {
    let count = match work {
        Some(Work::Run(run)) => {
            let mut waiting = 0;
            for session in run.running_sessions() {
                match session.waits_for_workflows() {
                    0 => return None,
                    count => waiting += count,
                }
            }
            waiting
        }
        Some(Work::Conversation(_) | Work::Pausing(_) | Work::Command(_)) => return None,
        None if state.conversation.waits_on_background() => state
            .conversation
            .workflows
            .iter()
            .filter(|workflow| !workflow.ended())
            .count(),
        None => return None,
    };
    (count > 0).then_some(count)
}

/// While the agent writes a workflow's script the line says so in its word's place, and the
/// word comes back once the script exists.
fn say_what_is_written(line: &mut WorkingLine, workflow: bool) {
    match (workflow, line.usual_verb.take()) {
        (true, Some(usual)) => line.usual_verb = Some(usual),
        (true, None) => {
            let writing = super::cli_workflow_run::WRITING.to_owned();
            line.usual_verb = Some(std::mem::replace(&mut line.verb, writing));
        }
        (false, Some(usual)) => line.verb = usual,
        (false, None) => {}
    }
}

/// A new word for each stretch of work; the same turn or run keeps its word.
fn verb_for(work: &Work<'_>, pick: impl FnOnce(Option<CommandWork>) -> &'static str) -> String {
    match work {
        Work::Pausing(_) => "Pausing".to_owned(),
        Work::Command(work) => pick(Some(*work)).to_owned(),
        Work::Conversation(_) | Work::Run(_) => pick(None).to_owned(),
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

/// What the agents running at once are doing between them, for the line's color. One inside a
/// tool call is work going on, however quiet the rest are, and so is one that thinks beside
/// another that doesn't. They are thinking only when every one of them is. Their silence is
/// all there is to go by only when none of them works a tool or thinks.
fn doing_between(running: impl Iterator<Item = Doing>) -> Doing {
    let (mut some_think, mut some_dont) = (false, false);
    for doing in running {
        match doing {
            Doing::RunningTool => return Doing::RunningTool,
            Doing::Thinking => some_think = true,
            Doing::Nothing | Doing::Writing => some_dont = true,
        }
    }
    match (some_think, some_dont) {
        (true, false) => Doing::Thinking,
        // Only a crew gets here, and its line never says what one agent does, so this holds
        // the color the way a running tool does and adds no words.
        (true, true) => Doing::RunningTool,
        (false, _) => Doing::Nothing,
    }
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

/// Phase, tasks, agents, and spend, as in `Build · 2/5 tasks · 3 agents · $0.21`. The phase is
/// one the workflow view lists: a run that is its task's CLI workflow names that workflow's
/// phase at work, and neither the plan it carries out nor that plan's tasks. Only a run with
/// no workflow names its own stage. A workflow still being written has none of these yet, so
/// the line says only its time and tokens.
fn run_details(run: &RunState) -> Vec<String> {
    let mut details = Vec::new();
    if !run.shape_known() {
        return details;
    }
    let cli_own = super::panel::the_runs_own(run);
    let phase = match (cli_own, run.shown_workflow()) {
        (Some(own), _) => crate::frame::state::phase_at_work(own),
        (None, Some(_)) => run.workflow_phase(),
        (None, None) => run.phase.map(phase_name),
    };
    details.extend(
        phase
            .filter(|phase| !phase.trim().is_empty())
            .map(argonauts_core::base::text::shown_name),
    );
    let (done, total) = run.tasks_done();
    let tasks_counted = total > 0 && cli_own.is_none();
    if tasks_counted {
        details.push(format!("{done}/{total} tasks"));
    }
    // A run counted in tasks leaves its agents to the rows under it; one with no tasks to count
    // says how many agents are at work.
    match run.running_sessions().count() {
        _ if tasks_counted => {}
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
        RunPhase::Understand => "Understand",
        RunPhase::Planning => "Planning",
        RunPhase::Dividing => "Dividing",
        RunPhase::Contracts => "Contracts",
        RunPhase::Build => "Build",
        RunPhase::FinalCheck => "Final check",
        RunPhase::Merge => "Merge",
        RunPhase::RunReview => "Run review",
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

/// Your undelivered guidance and mentions in the attached run, then conversation text waiting for
/// its turn, each with where it goes when that's one agent.
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
    for text in &state.conversation.queued {
        texts.push(text.clone());
        places.push(None);
    }
    (texts, places)
}

/// What you typed, without the line the engine frames it with for the agents, such as
/// `Guidance m-001 from you:`, which would take the place of your words on the screen.
fn own_words(text: &str) -> &str {
    match text.split_once('\n') {
        Some((frame, words)) if frame.ends_with(':') && frame.contains(" from you") => words,
        _ => text,
    }
}

/// `to Opus 5.5 on Claude Code, after its current tool call` or `to agy, at its
/// next turn`.
fn destination(run: &RunState, message: &QueuedMessage, names: &DisplayNames) -> Option<String> {
    let to = match &message.to {
        MentionTarget::All => return None,
        MentionTarget::Worker(worker) => names.worker(worker),
        MentionTarget::Task(task) => task.to_string(),
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
/// the workflow's name first when it isn't Captain. Which CLI the model runs on is
/// the marked chip on the bottom line. A run's own workflow shows on its panel row, never here.
/// Under Council, where you write to every seat, the rule says only `council`, as Claude Code
/// labels its own.
fn composer_label(state: &AppState) -> Option<String> {
    let workflow = workflow_label(state).filter(|workflow| workflow != CAPTAIN);
    if workflow
        .as_deref()
        .is_some_and(|workflow| workflow.eq_ignore_ascii_case(COUNCIL))
    {
        return Some(COUNCIL.to_owned());
    }
    match (workflow, main_model(state)) {
        (Some(workflow), Some(model)) => Some(format!(
            "{} · {model}",
            argonauts_core::base::text::shown_name(&workflow)
        )),
        (workflow, model) => workflow
            .map(|workflow| argonauts_core::base::text::shown_name(&workflow))
            .or(model),
    }
}

/// The workflow in which the main session's CLI does everything itself.
const CAPTAIN: &str = "captain";

/// The workflow in which the seats talk it over with you before anyone works.
const COUNCIL: &str = "council";

/// The model the main session runs, or its CLI's name while nobody knows the model.
fn main_model(state: &AppState) -> Option<String> {
    let agent = state
        .status
        .as_ref()
        .map(|status| &status.main_agent)
        .or_else(|| state.welcome.as_ref()?.main_agent.as_ref())?;
    let agent = state.names.running(agent);
    let cli = agent.agent.as_str();
    Some(
        state
            .names
            .model(cli, agent.model.as_deref())
            .unwrap_or_else(|| state.names.cli(cli)),
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
        .filter(|chosen| !chosen.is_empty())
        .or_else(|| {
            state
                .welcome
                .as_ref()
                .map(|welcome| welcome.workflow.clone())
        })
        .filter(|chosen| !chosen.is_empty())
}

/// The run plain text and the spend part are about: the one the app mode names, else the
/// attached one.
pub(super) fn focus_run(state: &AppState) -> Option<&RunState> {
    let named = match &state.mode {
        Some(
            AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Waiting { run, .. },
        ) => state.runs.get(run),
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

/// `☕` while `/awake` holds the computer's sleep off, and `☕ 3:45pm` for a hold until a time,
/// which shows nowhere else on screen.
fn awake_part(state: &AppState, clock: &bottom_line::Clock) -> Option<PartBody> {
    let until = match state.status.as_ref()?.awake.as_ref()? {
        AwakeHold::UntilQuit => None,
        AwakeHold::Until { at } if *at <= clock.now => return None,
        AwakeHold::Until { at } => Some(bottom_line::clock_time(*at, clock)),
    };
    // The ASCII glyph set is for fonts that draw symbols at the wrong width, so the cup is a word.
    let ascii = state.ui.as_ref().is_some_and(|ui| {
        crate::base::theme::Glyphs::for_settings(ui.glyphs, ui.ambiguous_width).is_ascii()
    });
    let cup = if ascii { "awake" } else { "☕" };
    let text = match until {
        Some(time) => format!("{cup} {time}"),
        None => cup.to_owned(),
    };
    Some(PartBody::Words {
        text,
        tone: Tone::Code,
    })
}

/// The adapter id behind a chip's short name.
pub(super) fn agent_for_label(label: &str) -> Option<&'static str> {
    Some(match label.trim() {
        "Claude" => "claude-code",
        "Codex" => "codex",
        "agy" => "antigravity-cli",
        "OpenCode" => "opencode",
        "OpenRouter" => "openrouter",
        _ => return None,
    })
}

/// The short names chips use for agents.
pub fn label_for_agent(agent_id: &str) -> String {
    match agent_id {
        "claude-code" => "Claude",
        "codex" => "Codex",
        "antigravity-cli" => "agy",
        "opencode" => "OpenCode",
        "openrouter" => "OpenRouter",
        "local" => "Local",
        other if runtime(other).is_some() => "Local",
        other => other,
    }
    .to_owned()
}

/// Where Enter sends a draft now, while a run works or a question waits.
/// What Enter does with what you typed while a run works.
pub(crate) const GUIDES_THE_RUN: &str = "Enter sends guidance to the run";

fn enter_sends(state: &AppState) -> Option<String> {
    match &state.mode {
        Some(AppMode::Working { .. }) => Some(GUIDES_THE_RUN.to_owned()),
        Some(AppMode::Pausing { .. }) => Some("Enter cancels the pause".to_owned()),
        Some(AppMode::Waiting { .. }) => Some("Enter resumes the run".to_owned()),
        Some(AppMode::Answering { .. }) => Some("Enter sends it after this answer".to_owned()),
        Some(AppMode::Idle) | None => state
            .status
            .as_ref()
            .map(|status| format!("Enter asks {}", state.names.worker(&status.main_agent))),
    }
}

/// Whether Enter sends a draft to a run rather than to the main agent.
fn run_takes_text(state: &AppState) -> bool {
    match &state.mode {
        Some(AppMode::Working { .. } | AppMode::Pausing { .. } | AppMode::Waiting { .. }) => true,
        Some(AppMode::Answering { .. } | AppMode::Idle) => false,
        None => state.attached_run().is_some_and(|run| run.is_active()),
    }
}

/// Added to the exit hint: runs keep going under their run host after the app leaves, and
/// keep the computer awake while they do when the app says their host holds sleep off.
fn exit_note(state: &AppState) -> Option<String> {
    let awake = state
        .status
        .as_ref()
        .is_some_and(|status| status.line.runs_keep_awake);
    let note = match state.runs.values().filter(|run| run.is_active()).count() {
        0 => return None,
        1 if awake => "the run keeps going and keeps the computer awake".to_owned(),
        1 => "the run keeps going".to_owned(),
        runs if awake => format!("{runs} runs keep going and keep the computer awake"),
        runs => format!("{runs} runs keep going"),
    };
    Some(note)
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::{AppEvent, ConversationEvent};
    use argonauts_core::base::names::{AgentId, TurnId, WorkflowId};
    use argonauts_core::models::limits::{PlanUsageReading, PlanUsageSource, PlanUsageStatus};
    use argonauts_core::runs::events::run::{RunMode, RunPhase, WaitReason};
    use argonauts_core::runs::events::workflow::StepEnd;
    use argonauts_core::workflows::workflow::{StandardPhase, StepKind, StepRef};
    use argonauts_core::{AgentEvent, RunId};
    use time::OffsetDateTime;

    use super::*;
    use crate::frame::state::{StepState, WorkflowState};

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
    fn a_run_waiting_at_a_question_still_takes_text() {
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
                AppMode::Waiting {
                    run: run.clone(),
                    reason: WaitReason::Approval,
                },
                true,
            ),
        ] {
            state.mode = Some(mode.clone());
            assert_eq!(run_takes_text(&state), takes, "{mode:?}");
        }
    }

    /// The input's rule names your workflow, what your next message would use, so it keeps
    /// its name while a run follows a workflow the main agent picked, or runs as Captain.
    #[test]
    fn the_input_rule_keeps_your_workflow_whatever_the_run_follows() {
        let id = RunId::new("run-20260930-100000-a1b2").unwrap();
        let mut state = AppState::default();
        state.status = Some(argonauts_core::app::HeaderAndBottomLine {
            title: String::new(),
            repo: "~/code".into(),
            workflow: "council".into(),
            ultracode: true,
            ultra: None,
            run: None,
            main_agent: "claude-code".parse().unwrap(),
            plan_chips: Vec::new(),
            line: Default::default(),
            spent: None,
            limit: None,
            narrator: NarratorMode::Off,
            approval_mode: CoreApproval::Supervised,
            awake: None,
        });
        let mut run = crate::frame::state::RunState::new(id.clone());
        run.mode = Some(RunMode::Crew);
        run.workflow = Some("council".into());
        run.workflows
            .push(workflow(1, "maker-checker", &["Build"], None));
        state.runs.insert(id.clone(), run);
        state.mode = Some(AppMode::Working { run: id.clone() });
        assert_eq!(composer_label(&state).as_deref(), Some("council"));

        state.runs.get_mut(&id).unwrap().mode = Some(RunMode::Captain);
        assert_eq!(composer_label(&state).as_deref(), Some("council"));
    }

    /// The rule's label: the model by name alone, the workflow first when it isn't Captain,
    /// `council` alone under Council, and ultracode as the rule's own tag, never a word in the
    /// label.
    #[test]
    fn the_input_rule_names_the_main_model_with_the_workflow_first() {
        let mut state = AppState::default();
        let status = |workflow: &str, agent: &str| argonauts_core::app::HeaderAndBottomLine {
            title: String::new(),
            repo: "~/code".into(),
            workflow: workflow.into(),
            ultracode: true,
            ultra: None,
            run: None,
            main_agent: agent.parse().unwrap(),
            plan_chips: Vec::new(),
            line: Default::default(),
            spent: None,
            limit: None,
            narrator: NarratorMode::Off,
            approval_mode: CoreApproval::Supervised,
            awake: None,
        };
        state.status = Some(status("captain", "claude-code/claude-opus-5-5"));
        assert_eq!(composer_label(&state).as_deref(), Some("Opus 5.5"));
        state.status = Some(status("council", "claude-code/claude-opus-5-5"));
        assert_eq!(composer_label(&state).as_deref(), Some("council"));
        state.status = Some(status("maker-checker", "claude-code/claude-opus-5-5"));
        assert_eq!(
            composer_label(&state).as_deref(),
            Some("Maker-checker · Opus 5.5")
        );
        // After /model the rule names the new model, and nothing else says which CLI.
        state.status = Some(status("captain", "codex/gpt-6.1-sol"));
        let label = composer_label(&state).unwrap();
        assert!(
            !label.contains("Codex") && !label.contains("captain"),
            "{label}"
        );
        // While nobody knows the model, the CLI's name stands in for it.
        state.status = Some(status("captain", "codex"));
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
            kind: StepKind::Read,
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
        // A log from before runs declared their workflows, its reader done: the run's own
        // stage is all there is to name.
        let mut reader = crate::frame::state::SessionState::new(
            argonauts_core::base::names::SessionKey::numbered(1),
            "claude-code".parse().unwrap(),
            "claude-code",
            argonauts_core::base::names::RoleName::Reader,
            time::macros::datetime!(2026-09-30 10:00 UTC),
        );
        reader.status = crate::frame::state::SessionStatus::Done;
        run.session_order.push(reader.key.clone());
        run.sessions.insert(reader.key.clone(), reader);
        assert_eq!(run_details(&run), ["Contracts"]);

        // Understand runs nested under the workflow to come.
        let under_own = StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        };
        let mut understand = workflow(2, "understanding", &["Understand"], Some(under_own));
        understand.steps.push(step(2, 0, false));
        run.workflows.push(understand);
        run.last_step = Some(step(2, 0, false).step);
        assert_eq!(run_details(&run), ["Understand"]);

        // Once the run's own workflow runs, its phases name the line, with a nested
        // workflow's steps counted in the phase they run in.
        let mut own = workflow(
            1,
            "council",
            &["Understand", "Plan", "Build", "Merge"],
            None,
        );
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
        state.conversation.turn = Some(turn.clone());
        let agent = |state: &mut AppState, event: AgentEvent| {
            state.apply(&AppEvent::Conversation {
                event: ConversationEvent::Agent {
                    turn: turn.clone(),
                    event,
                },
            });
        };
        let t0 = Instant::now();
        let mut bottom = crate::bottom::BottomState::new();
        let mut stalled_since = |state: &AppState, secs: u64| {
            let now = t0 + Duration::from_secs(secs);
            sync_working(state, &mut bottom, |_| "Rowing", now);
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
    fn the_working_line_names_the_task_in_hand_and_notes_when_the_agent_was_heard() {
        use argonauts_core::TodoItem;
        let turn = TurnId::numbered(1);
        let mut state = AppState::default();
        state.mode = Some(AppMode::Answering { turn: turn.clone() });
        state.conversation.turn = Some(turn.clone());
        let agent = |state: &mut AppState, event: AgentEvent| {
            state.apply(&AppEvent::Conversation {
                event: ConversationEvent::Agent {
                    turn: turn.clone(),
                    event,
                },
            });
        };
        let t0 = Instant::now();
        let mut bottom = crate::bottom::BottomState::new();
        let mut synced = |state: &AppState, secs: u64| {
            sync_working(
                state,
                &mut bottom,
                |_| "Rowing",
                t0 + Duration::from_secs(secs),
            );
            bottom.working.clone().expect("a working line")
        };
        let todo = |text: &str, status| TodoItem {
            text: text.to_owned(),
            status,
        };

        // With no list the verb stands, and the turn's start counts as hearing from it.
        let line = synced(&state, 0);
        assert_eq!((line.task.as_deref(), line.next.as_deref()), (None, None));
        assert_eq!(line.heard_at, Some(t0));

        agent(
            &mut state,
            AgentEvent::Todo {
                items: vec![
                    todo("Read the parser", TodoStatus::Completed),
                    todo("Add the due date column", TodoStatus::InProgress),
                    todo("Run the tests", TodoStatus::Pending),
                    todo("Write the notes", TodoStatus::Pending),
                ],
            },
        );
        let line = synced(&state, 5);
        assert_eq!(line.task.as_deref(), Some("Add the due date column"));
        assert_eq!(line.next.as_deref(), Some("Run the tests"));
        assert_eq!(line.verb, "Rowing", "the verb waits behind the task");
        assert_eq!(line.heard_at, Some(t0 + Duration::from_secs(5)));

        // A sync that brings nothing new leaves the clock where it was.
        assert_eq!(
            synced(&state, 18).heard_at,
            Some(t0 + Duration::from_secs(5))
        );
        agent(
            &mut state,
            AgentEvent::TextDelta {
                text: "On it.".into(),
                parent: None,
            },
        );
        assert_eq!(
            synced(&state, 19).heard_at,
            Some(t0 + Duration::from_secs(19))
        );

        // With every task done the verb is back.
        agent(
            &mut state,
            AgentEvent::Todo {
                items: vec![todo("Read the parser", TodoStatus::Completed)],
            },
        );
        let line = synced(&state, 20);
        assert_eq!((line.task.as_deref(), line.next.as_deref()), (None, None));
    }

    /// A crew's line takes its color from every running agent. While one of them is inside a
    /// tool call, or thinks beside one that doesn't, the line stays as it is through any
    /// silence. With all of them thinking it goes toward amber, and it goes red only when none
    /// works a tool or thinks.
    #[test]
    fn a_crews_working_line_stays_its_color_while_an_agent_runs_a_tool() {
        use argonauts_core::base::names::{RoleName, SessionKey};
        use ratatui::style::Color;
        use time::macros::datetime;

        use crate::base::draw::render::{Clock, RenderCtx};
        use crate::base::theme::{Role, Theme};
        use crate::frame::state::{SessionState, SessionStatus};

        let run_id = RunId::new("run-01").unwrap();
        let state_with = |doing: &[Doing]| {
            let mut run = RunState::new(run_id.clone());
            for (at, doing) in doing.iter().enumerate() {
                let mut session = SessionState::new(
                    SessionKey::numbered(at as u32 + 1),
                    "claude-code".parse().unwrap(),
                    "claude-code",
                    RoleName::Worker,
                    datetime!(2026-09-30 10:00 UTC),
                );
                session.status = SessionStatus::Working;
                session.doing = *doing;
                run.session_order.push(session.key.clone());
                run.sessions.insert(session.key.clone(), session);
            }
            let mut state = AppState::default();
            state.runs.insert(run_id.clone(), run);
            state.mode = Some(AppMode::Working {
                run: run_id.clone(),
            });
            state
        };
        let theme = Theme::dark();
        let t0 = Instant::now();
        // The glyph's color and the line's words after `secs` with nothing new from any agent.
        let after = |doing: &[Doing], secs: u64| {
            let state = state_with(doing);
            let mut bottom = crate::bottom::BottomState::new();
            sync_working(&state, &mut bottom, |_| "Rowing", t0);
            let now = t0 + Duration::from_secs(secs);
            sync_working(&state, &mut bottom, |_| "Rowing", now);
            let line = bottom.working.expect("a working line");
            let drawn = line.line(&RenderCtx::new(100, &theme, Clock::new(t0, now)));
            let words: String = drawn.spans.iter().map(|span| &*span.content).collect();
            (drawn.spans[0].style.fg, words)
        };
        let color = |role| theme.style(role).fg;
        assert!(matches!(color(Role::Accent), Some(Color::Rgb(..))));

        let tool = Doing::RunningTool;
        for agents in [
            &[tool, tool][..],
            &[tool, tool, tool],
            &[tool, Doing::Nothing],
            &[tool, Doing::Thinking],
            // One in a long think while another waits for its next token is an ordinary crew.
            &[Doing::Thinking, Doing::Nothing],
            &[Doing::Thinking, Doing::Writing],
            &[Doing::Nothing, Doing::Writing, Doing::Thinking],
        ] {
            for secs in [9, 15, 20, 60] {
                let (glyph, words) = after(agents, secs);
                assert_eq!(glyph, color(Role::Accent), "{agents:?} at {secs}s");
                assert!(!words.contains("running tool"), "{words}");
                assert!(!words.contains("think"), "{words}");
            }
        }
        let (glyph, words) = after(&[Doing::Thinking, Doing::Thinking], 60);
        assert_eq!(glyph, color(Role::Warn), "a long think is amber");
        assert!(!words.contains("thought"), "{words}");
        for nobody_works in [
            &[Doing::Nothing, Doing::Nothing][..],
            &[Doing::Nothing, Doing::Writing],
            &[Doing::Writing, Doing::Writing, Doing::Nothing],
        ] {
            assert_eq!(
                after(nobody_works, 60).0,
                color(Role::Danger),
                "{nobody_works:?}: none of them thinks or runs a tool"
            );
        }
        // A lone agent still says what it does.
        let (glyph, words) = after(&[tool], 60);
        assert_eq!(glyph, color(Role::Accent));
        assert!(words.contains("running tool for 1m"), "{words}");
    }

    /// The row from the picture's example numbers, at the picture's width.
    #[test]
    fn updated_row_is_its_picture() {
        use crate::base::theme::{Glyphs, Theme};
        use crate::testing::pictures::picture_check::assert_rows_match;

        let read = |ext: &str| {
            std::fs::read_to_string(format!(
                "{}/tests/golden/updated_row.{ext}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        let txt: Vec<String> = read("txt")
            .lines()
            .map(|row| row.trim_end().to_owned())
            .collect();
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let row = crate::base::kit::updated_row("Updated to 0.1.1 · /release-notes", &theme);
        assert_rows_match(
            "updated_row",
            &[row],
            (&txt.join("\n"), &read("html")),
            7..8,
            &theme,
        );
    }

    /// The working line offers Esc only for what Esc stops: a conversation turn, a turn
    /// answered while a run works, and a run that's pausing. A run's own agents keep going.
    /// While the main agent rests and only its CLI's subagents work, there is no line at all.
    #[test]
    fn the_working_line_offers_esc_only_for_what_esc_stops() {
        let run = RunId::new("run-01").unwrap();
        let turn = TurnId::numbered(1);
        let t0 = Instant::now();
        let synced = |state: &AppState| {
            let mut bottom = crate::bottom::BottomState::new();
            sync_working(state, &mut bottom, |_| "Rowing", t0);
            bottom.working.map(|line| line.esc_stops)
        };
        let mut state = AppState::default();
        state.runs.insert(run.clone(), RunState::new(run.clone()));
        state.mode = Some(AppMode::Working { run: run.clone() });
        assert_eq!(synced(&state), Some(false), "a run alone");
        state.conversation.turn = Some(turn.clone());
        assert_eq!(synced(&state), Some(true), "a turn answered meanwhile");
        state.conversation.turn = None;
        state.mode = Some(AppMode::Pausing { run });
        assert_eq!(synced(&state), Some(true), "a pausing run stops now");

        let mut state = AppState::default();
        state.mode = Some(AppMode::Idle);
        state.command_work = Some(CommandWork::Updating);
        assert_eq!(synced(&state), Some(true), "/update stops");
        state.command_work = Some(CommandWork::Checking);
        assert_eq!(
            synced(&state),
            Some(false),
            "Doctor's check after it finishes"
        );

        let mut state = AppState::default();
        state.mode = Some(AppMode::Answering { turn: turn.clone() });
        state.conversation.turn = Some(turn.clone());
        assert_eq!(synced(&state), Some(true), "a conversation turn");
        let agent = |event| AppEvent::Conversation {
            event: ConversationEvent::Agent {
                turn: turn.clone(),
                event,
            },
        };
        state.apply(&agent(AgentEvent::Subagent {
            update: argonauts_core::runs::events::session::SubagentUpdate {
                id: "toolu_a".into(),
                parent: None,
                kind: argonauts_core::runs::events::session::SubagentKind::Subagent,
                label: Some("Count slowly".into()),
                prompt: None,
                status: argonauts_core::runs::events::session::SubagentStatus::Running,
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

    /// While a turn's CLI retries its request, the working line says which retry it is,
    /// and the same retry keeps the moment it goes as later events come.
    #[test]
    fn the_working_line_follows_a_conversation_turns_retries() {
        use std::time::Duration;

        use crate::frame::state::TurnRetry;

        let turn = TurnId::numbered(1);
        let t0 = Instant::now();
        let mut state = AppState::default();
        state.mode = Some(AppMode::Answering { turn: turn.clone() });
        state.conversation.turn = Some(turn);
        state.conversation.retry = Some(TurnRetry {
            try_number: 2,
            delay_ms: Some(4_000),
        });
        let mut bottom = crate::bottom::BottomState::new();
        let retry_at = |at: Duration, bottom: &mut crate::bottom::BottomState| {
            sync_working(&state, bottom, |_| "Rowing", t0 + at);
            bottom.working.as_ref().and_then(|line| line.retry)
        };
        let first = retry_at(Duration::ZERO, &mut bottom).expect("the retry");
        assert_eq!(first.try_number, 2);
        assert_eq!(first.at, Some(t0 + Duration::from_secs(4)));
        assert_eq!(retry_at(Duration::from_secs(1), &mut bottom), Some(first));
    }

    #[test]
    fn a_conversation_turns_plan_usage_reading_is_kept_for_the_bottom_line() {
        let wall = argonauts_core::base::clock::now_utc();
        let mut state = AppState::default();
        assert!(state.plan_windows.is_empty());
        let reading = PlanUsageReading {
            agent_id: AgentId("claude-code".into()),
            status: PlanUsageStatus::Allowed,
            used: Some(0.71),
            resets_at: None,
            window: Some("five_hour".into()),
            source: PlanUsageSource::RateLimitEvent,
            observed_at: wall,
            detail: None,
            wrap_up: false,
        };
        state.apply(&AppEvent::Conversation {
            event: ConversationEvent::Agent {
                turn: TurnId::numbered(1),
                event: AgentEvent::PlanUsage {
                    reading: reading.clone(),
                },
            },
        });
        assert_eq!(state.plan_windows["claude-code"], [reading]);
    }

    fn status_with(awake: Option<AwakeHold>) -> argonauts_core::app::HeaderAndBottomLine {
        argonauts_core::app::HeaderAndBottomLine {
            title: String::new(),
            repo: "~/code".into(),
            workflow: "council".into(),
            ultracode: false,
            ultra: None,
            run: None,
            main_agent: "claude-code".parse().unwrap(),
            plan_chips: Vec::new(),
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
        assert_eq!(words(Some(AwakeHold::UntilQuit)).as_deref(), Some("☕"));
        let later = wall + time::Duration::hours(2);
        assert_eq!(
            words(Some(AwakeHold::Until { at: later })).as_deref(),
            Some("☕ 3:45pm")
        );
        // A time that's up, or no hold of yours, shows nothing.
        assert_eq!(words(Some(AwakeHold::Until { at: wall })), None);
        assert_eq!(words(None), None);
    }

    #[test]
    fn the_ascii_glyph_set_puts_a_word_for_the_cup() {
        let mut state = AppState::default();
        state.ui = Some(UiSettings {
            glyphs: argonauts_core::settings::ui::Glyphs::Ascii,
            ..UiSettings::default()
        });
        state.status = Some(status_with(Some(AwakeHold::UntilQuit)));
        let wall = time::macros::datetime!(2026-10-07 13:45 UTC);
        assert_eq!(awake_words(&state, wall).as_deref(), Some("awake"));
    }

    #[test]
    fn the_exit_hint_says_when_runs_left_going_keep_the_computer_awake() {
        let mut state = AppState::default();
        state.status = Some(status_with(None));
        assert_eq!(exit_note(&state), None);
        for n in 1..=2 {
            let run = RunId::new(format!("run-0{n}")).unwrap();
            state.runs.insert(run.clone(), RunState::new(run));
        }
        assert_eq!(exit_note(&state).as_deref(), Some("2 runs keep going"));
        if let Some(status) = state.status.as_mut() {
            status.line.runs_keep_awake = true;
        }
        assert_eq!(
            exit_note(&state).as_deref(),
            Some("2 runs keep going and keep the computer awake")
        );
        state.runs.pop_first();
        assert_eq!(
            exit_note(&state).as_deref(),
            Some("the run keeps going and keeps the computer awake")
        );
    }
}
