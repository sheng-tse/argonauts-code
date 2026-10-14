//! A chat turn with the default agent: its answer streams into scrollback like Claude Code's,
//! its tool calls print by verbosity (`full` shows each with its result, `normal` folds quiet
//! ones into one dim line, `quiet` drops them), and Esc leaves `⎿  Interrupted · …` under it.
//! Argonauts' own calls, such as the report, never print; only Ctrl+O shows them.

use std::time::Duration;

use argonauts_core::app::{ChatEvent, TurnReply};
use argonauts_core::events::session::VendorWorkflow;
use argonauts_core::names::{TurnId, WorkerRef};
use argonauts_core::settings::protocol::SoloStyle;
use argonauts_core::settings::ui::VerbTheme;
use argonauts_core::{AgentEvent, ErrorCategory, Verbosity};

use crate::bottom::verbs::VerbStyle;
use crate::bridge::{mods, tools};
use crate::cell::{
    AssistantMessage, Cell, CollapsedGroup, Notice, ResultLine, Tone, ToolCall, ToolItem, ToolKind,
    ToolState, TurnEnd, UserMessage,
};
use crate::format::WallClock;
use crate::history::CellId;
use crate::text::sanitize_line;

use super::{Cx, emit, quiet, steps};

const INTERRUPTED: &str = "Interrupted · What should Argonauts do instead?";

/// Maps a vendor's tool name to its kind, which decides whether it folds and how the fold line
/// counts it.
pub type ToolKinds = fn(vendor: &str, name: &str) -> ToolKind;

#[derive(Debug)]
pub struct ChatTurns {
    open: Option<OpenTurn>,
    kinds: ToolKinds,
}

impl Default for ChatTurns {
    fn default() -> Self {
        Self {
            open: None,
            kinds: tools::kind_of,
        }
    }
}

#[derive(Debug)]
struct OpenTurn {
    turn: TurnId,
    worker: WorkerRef,
    /// A replayed turn keeps its cells for Ctrl+O and streams nothing.
    replaying: bool,
    /// The part of the answer streaming now. A tool call ends it, and later text starts
    /// another below the calls.
    stream: Option<CellId>,
    /// Text of the part streaming now, or of the whole answer while replaying.
    text: String,
    /// Whether that text came in deltas, which a whole `Message` then repeats.
    from_deltas: bool,
    /// Text of each part that already ended, which the finished reply repeats.
    parts: Vec<String>,
    /// Live cells for the calls since the last text, oldest first: a call each, or a fold line
    /// for quiet ones. They print in this order.
    calls: Vec<CellId>,
    /// Argonauts' own calls, held until they end and then kept for Ctrl+O only.
    own: Vec<ToolCall>,
    /// The workflows the CLI runs itself that already have their row.
    workflows: std::collections::BTreeSet<String>,
    /// Esc stopped the main agent's turn alone, its line shows, and nothing came since.
    interrupted: bool,
}

impl ChatTurns {
    /// Uses `kinds` in place of the shared tool table.
    pub fn with_tool_kinds(kinds: ToolKinds) -> Self {
        Self {
            kinds,
            ..Self::default()
        }
    }

    pub fn on_chat(&mut self, event: &ChatEvent, clock: &WallClock, cx: &mut Cx<'_>) {
        match event {
            ChatEvent::TurnStarted {
                turn,
                text,
                worker,
                continues,
                ..
            } => {
                // A failure from before your next message no longer takes `r`. A turn that
                // goes on without one, as the session's line about a failed run does, leaves
                // `r` to the failure it follows.
                if !*continues {
                    cx.model.raw_failure = None;
                }
                self.start(turn, text, worker, *continues, cx);
            }
            ChatEvent::Agent { turn, event } => {
                if let Some(open) = self.open.as_mut().filter(|open| open.turn == *turn) {
                    agent_event(open, self.kinds, event, clock, cx);
                }
            }
            ChatEvent::TurnFinished {
                turn,
                reply,
                elapsed_ms,
                ..
            } => {
                if let Some(open) = self.take(turn) {
                    self.finish(open, reply, Duration::from_millis(*elapsed_ms), clock, cx);
                }
            }
            ChatEvent::TurnCancelled { turn } => {
                if let Some(mut open) = self.take(turn) {
                    end_parts(&mut open, ToolState::Stopped, cx);
                    // A turn Esc stopped alone left the line when it stopped.
                    if !open.interrupted {
                        emit(
                            cx,
                            Cell::Custom(Box::new(ResultLine::new(Tone::Dim, INTERRUPTED))),
                        );
                    }
                }
            }
            ChatEvent::HandedOff {
                turn,
                from,
                to,
                why,
                told,
            } => {
                if let Some(open) = self.open.as_mut().filter(|open| open.turn == *turn) {
                    handed_off(open, from, to, why, told, cx);
                }
            }
            ChatEvent::Joined { turn, text } => {
                if let Some(open) = self.open.as_mut().filter(|open| open.turn == *turn) {
                    joined(open, text, cx);
                }
            }
            ChatEvent::TurnFailed { turn, error } => {
                if let Some(mut open) = self.take(turn) {
                    end_parts(&mut open, ToolState::Failed, cx);
                    let error = error.trim();
                    let text = if error.is_empty() {
                        "The main agent couldn't answer".to_owned()
                    } else {
                        format!("The main agent couldn't answer: {error}")
                    };
                    emit(cx, Notice::new(Tone::Error, text));
                }
            }
        }
    }

    fn take(&mut self, turn: &TurnId) -> Option<OpenTurn> {
        if self.open.as_ref().is_some_and(|open| open.turn == *turn) {
            self.open.take()
        } else {
            None
        }
    }

    /// `continues`: the turn goes on from the one before, which showed your message already.
    fn start(
        &mut self,
        turn: &TurnId,
        text: &str,
        worker: &WorkerRef,
        continues: bool,
        cx: &mut Cx<'_>,
    ) {
        // A turn that never heard its end leaves what it had in scrollback.
        if let Some(mut stale) = self.open.take() {
            end_parts(&mut stale, ToolState::Stopped, cx);
        }
        let message = UserMessage::new(text)
            .with_keywords(cx.keywords_of(worker))
            .with_pictures(cx.model.bottom.composer.picture_numbers_in(text));
        let replaying = cx.replaying();
        if !continues {
            if replaying {
                cx.model.history.keep(message);
            } else {
                echo(message, cx);
            }
        }
        self.open = Some(OpenTurn {
            turn: turn.clone(),
            worker: worker.clone(),
            replaying,
            stream: None,
            text: String::new(),
            from_deltas: false,
            parts: Vec::new(),
            calls: Vec::new(),
            own: Vec::new(),
            workflows: std::collections::BTreeSet::new(),
            interrupted: false,
        });
    }

    fn finish(
        &mut self,
        mut open: OpenTurn,
        reply: &TurnReply,
        elapsed: Duration,
        clock: &WallClock,
        cx: &mut Cx<'_>,
    ) {
        let markdown = match reply {
            TurnReply::Answer { markdown }
            | TurnReply::Change { markdown, .. }
            | TurnReply::Unclear { markdown, .. } => markdown.as_str(),
        };
        settle_tools(&mut open, ToolState::Stopped, cx);
        // What already printed above the calls isn't printed again.
        let rest = after_parts(markdown, &open.parts);
        if open.replaying {
            if !rest.trim().is_empty() {
                cx.model.history.keep(AssistantMessage::new(rest));
            }
        } else if let Some(id) = open.stream.take() {
            cx.model.history.finish_stream_with(id, rest);
        } else if !rest.trim().is_empty() {
            let message = AssistantMessage::new(rest).with_stamp(stamp(&open.worker, clock));
            cx.model.history.push(message);
        }
        keep_own(&mut open, Some(ToolState::Stopped), cx);
        // Solo in your checkout has no run to say what your words asked, so the reply does.
        if let TurnReply::Change {
            triage,
            instructions,
            ..
        } = reply
            && triage.solo == Some(SoloStyle::Direct)
        {
            steps::direct_asked(instructions, cx);
        }
        // A change goes on as a run, or a turn in your checkout, whose own end gets the line.
        let changes = matches!(reply, TurnReply::Change { .. });
        if cx.settings.turn_duration && !changes {
            let style = match cx.settings.verbs {
                VerbTheme::Voyage => VerbStyle::Voyage,
                VerbTheme::Plain => VerbStyle::Plain,
            };
            let label = format!("{} for", cx.model.verb_picker.done(style));
            let end = TurnEnd::new(label, elapsed, Some(clock.clock_now()));
            emit(cx, end);
        }
    }
}

/// The `❯` line for the turn's prompt. The composer echoes what you send while idle, grey
/// until the request starts, so that one turns bright, drawing the keywords of the CLI that took
/// the turn, which may not be the one the input drew; a prompt that waited in the queue, or came
/// from elsewhere, gets its line here.
fn echo(message: UserMessage, cx: &mut Cx<'_>) {
    let history = &mut cx.model.history;
    let pending = history
        .active_cells()
        .find(|(_, cell)| matches!(cell, Cell::User(message) if message.pending))
        .map(|(id, _)| id);
    if let Some(id) = pending {
        if let Some(echoed) = history.active_mut(id).and_then(Cell::as_user_mut) {
            echoed.keywords = message.keywords;
            echoed.start();
        }
        history.finish_active(id);
        return;
    }
    let last = history
        .held_cells()
        .last()
        .or_else(|| history.cells().last());
    if !matches!(last, Some(Cell::User(_))) {
        history.push(message);
    }
}

/// Your message the model read mid-turn shows where it read it, after what ran before, as
/// Claude Code's send now shows it.
fn joined(open: &mut OpenTurn, text: &str, cx: &mut Cx<'_>) {
    end_stream(open, cx);
    print_done(open, cx);
    let message = UserMessage::new(text)
        .with_keywords(cx.keywords_of(&open.worker))
        .with_pictures(cx.model.bottom.composer.picture_numbers_in(text));
    if open.replaying {
        cx.model.history.keep(message);
    } else {
        cx.model.history.push(message);
    }
}

fn agent_event(
    open: &mut OpenTurn,
    kinds: ToolKinds,
    event: &AgentEvent,
    clock: &WallClock,
    cx: &mut Cx<'_>,
) {
    // More from the main agent, so the line a stopped turn left isn't its last word.
    if matches!(
        event,
        AgentEvent::TextDelta { parent: None, .. }
            | AgentEvent::Message { parent: None, .. }
            | AgentEvent::ToolStarted { parent: None, .. }
    ) {
        open.interrupted = false;
    }
    match event {
        AgentEvent::TextDelta { text, parent: None } => {
            open.from_deltas = true;
            deliver(open, text, clock, cx);
        }
        AgentEvent::Message { text, parent: None } => {
            if open.from_deltas {
                return;
            }
            if !open.text.trim().is_empty() {
                deliver(open, "\n\n", clock, cx);
            }
            deliver(open, text, clock, cx);
        }
        AgentEvent::ToolStarted {
            id,
            name,
            input,
            parent: None,
        } => {
            if open.replaying {
                return;
            }
            let vendor = open.worker.worker.clone();
            // It doesn't end the answer streaming, so the answer stays one block. Claude
            // Code's own workflow shows as the workflow it reports, not as the script it ran.
            if tools::is_own(name) || is_vendor_workflow(&vendor, name) {
                let call = tools::start_call(&vendor, name, input).with_key(id.clone());
                open.own.push(call);
                return;
            }
            end_stream(open, cx);
            let mut call = tools::start_call(&vendor, name, input).with_key(id.clone());
            call.kind = kinds(&vendor, name);
            add_call(open, call, cx);
            print_done(open, cx);
        }
        AgentEvent::ToolFinished {
            id,
            ok,
            output,
            parent: None,
        } => {
            let key = Some(id.as_str());
            if let Some(call) = open.own.iter_mut().find(|call| call.key.as_deref() == key) {
                tools::finish_call(call, *ok, output.as_deref());
                return;
            }
            let history = &mut cx.model.history;
            for cell in &open.calls {
                let call = match history.active_mut(*cell) {
                    Some(Cell::Tool(call)) => Some(call).filter(|call| call.key.as_deref() == key),
                    Some(Cell::Group(group)) => group
                        .calls
                        .iter_mut()
                        .find(|call| call.key.as_deref() == key),
                    _ => None,
                };
                if let Some(call) = call {
                    tools::finish_call(call, *ok, output.as_deref());
                    break;
                }
            }
            print_done(open, cx);
        }
        AgentEvent::FilePatch {
            id,
            path,
            hunks,
            parent: None,
        } => {
            let key = Some(id.as_str());
            let history = &mut cx.model.history;
            for cell in &open.calls {
                let call = match history.active_mut(*cell) {
                    Some(Cell::Tool(call)) => Some(call).filter(|call| call.key.as_deref() == key),
                    Some(Cell::Group(group)) => group
                        .calls
                        .iter_mut()
                        .find(|call| call.key.as_deref() == key),
                    _ => None,
                };
                if let Some(call) = call {
                    tools::take_patch(call, path, hunks);
                    break;
                }
            }
        }
        AgentEvent::ModOutput {
            kind,
            plugin,
            text,
            timeout_ms,
        } => {
            let line = mods::show(*kind, plugin.as_deref(), text.as_deref(), *timeout_ms, cx);
            if let Some(line) = line {
                if !open.replaying {
                    end_stream(open, cx);
                    print_done(open, cx);
                }
                emit(cx, Cell::Custom(Box::new(line)));
            }
        }
        AgentEvent::Workflow { workflow } => {
            if open.replaying || !open.workflows.insert(workflow.id.clone()) {
                return;
            }
            end_stream(open, cx);
            print_done(open, cx);
            emit(cx, workflow_row(workflow));
        }
        AgentEvent::Error {
            category: ErrorCategory::Interrupted,
            ..
        } => interrupted(open, cx),
        _ => {}
    }
}

/// Esc stopped the main agent's turn alone: what it had so far ends here with
/// the line Esc leaves, as on Claude Code's screen. The turn stays open for the subagents it
/// left running and for what it says once they are done.
fn interrupted(open: &mut OpenTurn, cx: &mut Cx<'_>) {
    if open.interrupted {
        return;
    }
    settle_tools(open, ToolState::Stopped, cx);
    if open.replaying {
        let said = std::mem::take(&mut open.text);
        if !said.trim().is_empty() {
            cx.model.history.keep(AssistantMessage::new(said.clone()));
            open.parts.push(said);
        }
    } else {
        end_stream(open, cx);
    }
    emit(
        cx,
        Cell::Custom(Box::new(ResultLine::new(Tone::Dim, INTERRUPTED))),
    );
    open.interrupted = true;
}

/// Claude Code's Workflow tool, which runs a script the views mirror from its reports.
fn is_vendor_workflow(vendor: &str, name: &str) -> bool {
    vendor == "claude-code" && name == "Workflow"
}

/// `Workflow(release-check) · Claude Code's own · ctrl+t to open`, as a crew run's workflow
/// starts, with what it does under it.
fn workflow_row(workflow: &VendorWorkflow) -> ToolCall {
    let name = Some(workflow.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or("workflow");
    let mut call = ToolCall::call(
        ToolKind::Other,
        "Workflow",
        sanitize_line(name).into_owned(),
    )
    .with_state(ToolState::Done)
    .with_tail(" · Claude Code's own · ctrl+t to open");
    if !workflow.description.trim().is_empty() {
        call.push(ToolItem::text(
            sanitize_line(workflow.description.trim()).into_owned(),
        ));
    }
    call
}

/// Streams answer text, below any calls made so far.
fn deliver(open: &mut OpenTurn, text: &str, clock: &WallClock, cx: &mut Cx<'_>) {
    open.text.push_str(text);
    if open.replaying {
        return;
    }
    settle_tools(open, ToolState::Stopped, cx);
    let history = &mut cx.model.history;
    let id = match open.stream {
        Some(id) => id,
        None => {
            let id = history.start_stream();
            history.stamp_stream(id, stamp(&open.worker, clock));
            open.stream = Some(id);
            id
        }
    };
    history.push_stream(id, text);
}

/// Ends the part of the answer streaming now, so a call goes under it.
fn end_stream(open: &mut OpenTurn, cx: &mut Cx<'_>) {
    if open.replaying {
        return;
    }
    if let Some(id) = open.stream.take() {
        cx.model.history.finish_stream(id);
    }
    if !open.text.trim().is_empty() {
        open.parts.push(std::mem::take(&mut open.text));
    }
    open.text.clear();
    open.from_deltas = false;
    keep_own(open, None, cx);
}

/// Keeps Argonauts' own calls that ended for the transcript overlay, after the answer they
/// followed. Once the turn ends, `end` settles any still running and they go too. Kept cells
/// never print, and the overlay draws each once, so a call waits here until it's done.
fn keep_own(open: &mut OpenTurn, end: Option<ToolState>, cx: &mut Cx<'_>) {
    let mut running = Vec::new();
    for mut call in std::mem::take(&mut open.own) {
        if call.is_running() {
            match end {
                None => {
                    running.push(call);
                    continue;
                }
                Some(ToolState::Failed) => call.finish(false),
                Some(_) => call.stop(),
            }
        }
        cx.model.history.keep(call);
    }
    open.own = running;
}

/// At `full` each call gets its own cell, at `normal` quiet calls in a row share a fold line,
/// and at `quiet` they all share one that goes without a trace.
fn add_call(open: &mut OpenTurn, call: ToolCall, cx: &mut Cx<'_>) {
    let folds = match cx.verbosity() {
        Verbosity::Full => false,
        Verbosity::Normal => call.kind.is_quiet(),
        Verbosity::Quiet => true,
    };
    let history = &mut cx.model.history;
    if !folds {
        open.calls.push(history.push_active(call));
        return;
    }
    let last = open.calls.last().and_then(|id| history.active_mut(*id));
    match last.and_then(Cell::as_group_mut) {
        Some(group) => group.push(call),
        None => {
            let mut group = CollapsedGroup::new();
            group.push(call);
            open.calls.push(history.push_active(group));
        }
    }
}

/// Prints the finished calls at the front. A fold line waits while it's the newest, since the
/// next quiet call joins it.
fn print_done(open: &mut OpenTurn, cx: &mut Cx<'_>) {
    if quiet(cx) {
        return;
    }
    let history = &mut cx.model.history;
    while let Some(&id) = open.calls.first() {
        let done = match history.active(id) {
            Some(Cell::Tool(call)) => !call.is_running(),
            Some(Cell::Group(group)) => !group.is_running() && open.calls.len() > 1,
            _ => true,
        };
        if !done {
            break;
        }
        history.finish_active(id);
        open.calls.remove(0);
    }
}

/// Prints the calls once text follows them or the turn ends, marking any still running as
/// `state`. At `quiet` they go without a trace.
fn settle_tools(open: &mut OpenTurn, state: ToolState, cx: &mut Cx<'_>) {
    let quiet = quiet(cx);
    let history = &mut cx.model.history;
    for id in std::mem::take(&mut open.calls) {
        if quiet {
            history.remove_active(id);
            continue;
        }
        let calls = match history.active_mut(id) {
            Some(Cell::Tool(call)) => std::slice::from_mut(call),
            Some(Cell::Group(group)) => group.calls.as_mut_slice(),
            _ => &mut [],
        };
        for call in calls.iter_mut().filter(|call| call.is_running()) {
            match state {
                ToolState::Failed => call.finish(false),
                _ => call.stop(),
            }
        }
        history.finish_active(id);
    }
}

/// What of the reply follows the parts that already printed. Each part is matched in turn with
/// the whitespace between them left out, however the controller joined them. A reply that
/// doesn't repeat them prints what follows the last one, or whole when it isn't there.
fn after_parts<'a>(reply: &'a str, parts: &[String]) -> &'a str {
    let parts: Vec<&str> = parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect();
    let mut rest = reply;
    for part in &parts {
        match rest.trim_start().strip_prefix(part) {
            Some(after) => rest = after,
            None => {
                let last = parts.last().copied().unwrap_or_default();
                rest = reply
                    .rfind(last)
                    .map_or(reply, |at| &reply[at + last.len()..]);
                break;
            }
        }
    }
    rest.trim_start()
}

/// `↷ Claude Code hit its usage limit · Codex continues with GPT-5.6 Luna`, with what the
/// next agent was told under it. What the first agent did stays above, and the next one's
/// work streams below as the same turn.
fn handed_off(
    open: &mut OpenTurn,
    from: &WorkerRef,
    to: &WorkerRef,
    why: &str,
    told: &str,
    cx: &mut Cx<'_>,
) {
    end_parts(open, ToolState::Stopped, cx);
    open.parts.clear();
    open.text.clear();
    open.from_deltas = false;
    open.worker = to.clone();
    if open.replaying {
        return;
    }
    let names = &cx.state.names;
    let next = names.harness(&to.worker);
    let text = match names.model(&to.worker, to.model.as_deref()) {
        Some(model) => format!(
            "{} {why} · {next} continues with {model}",
            names.harness(&from.worker)
        ),
        None => format!("{} {why} · {next} continues", names.harness(&from.worker)),
    };
    let mut notice = Notice::new(Tone::Normal, text).with_glyph("↷");
    if !told.trim().is_empty() {
        notice = notice.with_detail(format!("Told: {told}"));
    }
    emit(cx, notice);
}

/// What a stopped or failed turn leaves: its calls and the answer as far as it got.
fn end_parts(open: &mut OpenTurn, state: ToolState, cx: &mut Cx<'_>) {
    settle_tools(open, state, cx);
    if open.replaying {
        if !open.text.trim().is_empty() {
            cx.model
                .history
                .keep(AssistantMessage::new(open.text.clone()));
        }
        return;
    }
    end_stream(open, cx);
    keep_own(open, Some(state), cx);
}

/// Shown above the answer in the transcript overlay, like `10:42 AM claude-code/haiku`. The
/// overlay is the detail view, so it keeps the id.
fn stamp(worker: &WorkerRef, clock: &WallClock) -> String {
    format!("{} {worker}", clock.clock_now())
}
