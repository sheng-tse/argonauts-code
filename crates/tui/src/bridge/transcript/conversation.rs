//! A conversation turn with the main agent: its answer streams into scrollback like Claude Code's,
//! its tool calls print by verbosity (`full` shows each with its result, `normal` folds quiet
//! ones into one dim line, `quiet` drops them), and Esc leaves `⎿  Interrupted · …` under it.
//! Argonauts Code's own calls, such as the report, never print; only Ctrl+O shows them.

use std::time::Duration;

use argonauts_core::app::{ConversationEvent, SignedOut, TurnReply};
use argonauts_core::base::names::{ModelRef, TurnId};
use argonauts_core::input::{ParsedInput, parse_input_with};
use argonauts_core::runs::events::session::CliWorkflowRun;
use argonauts_core::settings::protocol::CaptainKind;
use argonauts_core::settings::ui::VerbTheme;
use argonauts_core::{AgentEvent, ErrorCategory, Verbosity};

use crate::base::format::WallClock;
use crate::base::text::sanitize_line;
use crate::bottom::working::verbs::VerbStyle;
use crate::bridge::{plugin_output, tools};
use crate::cells::cell::history::CellId;
use crate::cells::cell::{
    AssistantMessage, Cell, CollapsedGroup, Notice, ResultLine, Tone, ToolCall, ToolItem, ToolKind,
    ToolState, Trailing, TurnEnd, UserMessage, WorkflowResult,
};

use super::own_calls::{self, Own};
use super::{Cx, emit, quiet, steps};

const INTERRUPTED: &str = "Interrupted · What should Argonauts Code do instead?";

/// Maps an agent's tool name to its kind, which decides whether it folds and how the fold line
/// counts it.
pub type ToolKinds = fn(agent_id: &str, name: &str, input: &serde_json::Value) -> ToolKind;

#[derive(Debug)]
pub struct ConversationTurns {
    open: Option<OpenTurn>,
    kinds: ToolKinds,
}

impl Default for ConversationTurns {
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
    worker: ModelRef,
    /// A replayed turn keeps its cells for Ctrl+O and streams nothing.
    replaying: bool,
    /// The part of the answer streaming now. A tool call ends it, and later text starts
    /// another below the calls.
    stream: Option<CellId>,
    /// Text of the part streaming now, or of the whole answer while replaying.
    text: String,
    /// Whether that text came in deltas, which a whole `Message` then repeats.
    from_deltas: bool,
    /// How much of that text went to the screen. A JSON object that may end it waits, so a
    /// result the agent writes out at the end never shows.
    shown: usize,
    /// Text of each part that already ended, which the finished reply repeats.
    parts: Vec<String>,
    /// Live cells for the calls since the last text, oldest first: a call each, or a fold line
    /// for quiet ones. They print in this order.
    calls: Vec<CellId>,
    /// Argonauts Code's own calls, held until they end and then kept for Ctrl+O only.
    own: Vec<ToolCall>,
    /// The workflows the CLI runs itself that already have their row.
    workflows: std::collections::BTreeSet<String>,
    /// Esc stopped the main agent's turn alone, its line shows, and nothing came since.
    interrupted: bool,
    /// The answer missed its format and the agent was asked for it again. What it says from
    /// then on repeats the answer, so only the reply it ends with prints.
    retried: bool,
    /// The calls drawn as a plain line of what the session asked of Argonauts Code, by id.
    plain: Vec<String>,
    /// The session's calls to start a workflow, by id, which draw a line only if refused.
    starts: Vec<String>,
    /// A start turned down, held until the turn ends: a later start that works drops it.
    refused: Option<ToolCall>,
}

impl ConversationTurns {
    /// Uses `kinds` in place of the shared tool table.
    pub fn with_tool_kinds(kinds: ToolKinds) -> Self {
        Self {
            kinds,
            ..Self::default()
        }
    }

    pub fn on_conversation(
        &mut self,
        event: &ConversationEvent,
        clock: &WallClock,
        cx: &mut Cx<'_>,
    ) {
        match event {
            ConversationEvent::TurnStarted {
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
            ConversationEvent::Agent { turn, event } => {
                if let Some(open) = self.open.as_mut().filter(|open| open.turn == *turn) {
                    agent_event(open, self.kinds, event, clock, cx);
                }
            }
            ConversationEvent::TurnFinished {
                turn,
                reply,
                elapsed_ms,
                ..
            } => {
                if let Some(open) = self.take(turn) {
                    self.finish(open, reply, Duration::from_millis(*elapsed_ms), clock, cx);
                }
            }
            ConversationEvent::TurnCancelled { turn } => {
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
            // A watchdog stopped it: the line says what it saw.
            ConversationEvent::TurnStopped { turn, why } => {
                if let Some(mut open) = self.take(turn) {
                    end_parts(&mut open, ToolState::Stopped, cx);
                    emit(
                        cx,
                        Cell::Custom(Box::new(ResultLine::new(Tone::Warning, why.trim()))),
                    );
                }
            }
            ConversationEvent::FellBack {
                turn,
                from,
                to,
                why,
                told,
                signed_out,
            } => {
                if let Some(open) = self.open.as_mut().filter(|open| open.turn == *turn) {
                    fell_back(open, from, to, why, told, signed_out.as_ref(), cx);
                }
            }
            ConversationEvent::Joined { turn, text } => {
                if let Some(open) = self.open.as_mut().filter(|open| open.turn == *turn) {
                    joined(open, text, cx);
                }
            }
            ConversationEvent::AnswerRetried { turn } => {
                if let Some(open) = self.open.as_mut().filter(|open| open.turn == *turn) {
                    retried(open, cx);
                }
            }
            ConversationEvent::TurnFailed { turn, error } => {
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
            // The Context tab's count; nothing prints.
            ConversationEvent::ContextLeft { .. } => {}
            // Live, the command printed as you typed it.
            ConversationEvent::CommandTyped { text, .. } if cx.replaying() => {
                emit(cx, UserMessage::command(text.clone()));
            }
            ConversationEvent::CommandTyped { .. } => {}
            // Live, the command printed it as it answered.
            ConversationEvent::CommandReplied { reply } if cx.replaying() => {
                emit(cx, Cell::Custom(Box::new(ResultLine::of_reply(reply))));
            }
            ConversationEvent::CommandReplied { .. } => {}
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
        worker: &ModelRef,
        continues: bool,
        cx: &mut Cx<'_>,
    ) {
        // A turn that never heard its end leaves what it had in scrollback.
        if let Some(mut stale) = self.open.take() {
            end_parts(&mut stale, ToolState::Stopped, cx);
        }
        // A turn that runs one of the CLI's own commands echoes the command as you typed it.
        let runs_a_command = matches!(
            parse_input_with(text, cx.state.cli_commands.as_ref()),
            Ok(ParsedInput::Slash { .. })
        );
        let message = UserMessage::new(text)
            .with_keywords(cx.keywords_of(worker))
            .with_pictures(cx.model.bottom.composer.picture_numbers_in(text))
            .running_a_command(runs_a_command);
        let replaying = cx.history_only();
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
            shown: 0,
            parts: Vec::new(),
            calls: Vec::new(),
            own: Vec::new(),
            workflows: std::collections::BTreeSet::new(),
            interrupted: false,
            retried: false,
            plain: Vec::new(),
            starts: Vec::new(),
            refused: None,
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
        say_refused(&mut open, cx);
        // Captain in your checkout has no run to say what your words asked, so the reply does.
        if let TurnReply::Change {
            captain_or_crew,
            instructions,
            ..
        } = reply
            && captain_or_crew.captain == Some(CaptainKind::Direct)
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
            // A turn drawn again from its record ended then, and the record keeps no clock.
            let done = (!cx.replaying()).then(|| clock.clock_now());
            let end = TurnEnd::new(label, elapsed, done);
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
            | AgentEvent::ToolPending { parent: None, .. }
            | AgentEvent::ToolStarted { parent: None, .. }
    ) {
        open.interrupted = false;
    }
    match event {
        // Asked again, the agent repeats its answer, which the reply brings whole.
        AgentEvent::TextDelta { parent: None, .. } | AgentEvent::Message { parent: None, .. }
            if open.retried => {}
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
        // The call's row appears as the model starts writing it, with its name alone.
        AgentEvent::ToolPending {
            id,
            name,
            parent: None,
        } => {
            let agent_id = open.worker.agent.clone();
            if open.replaying || kept_out(&agent_id, name) || Own::named(name).is_some() {
                return;
            }
            end_stream(open, cx);
            let mut call = tools::writing_call(&agent_id, name).with_key(id.clone());
            call.kind = kinds(&agent_id, name, &serde_json::Value::Null);
            // A shell call doesn't say yet whether it only looks around, so it waits in the
            // fold line before it, and leaves it once its command shows it does more.
            let fold = folds(&call, cx)
                || (call.kind == ToolKind::Shell
                    && cx.verbosity() == Verbosity::Normal
                    && fold_is_newest(open, cx));
            add_call_folded(open, call, fold, cx);
            print_done(open, cx);
        }
        AgentEvent::ToolDropped { id, parent: None } => drop_writing(open, id, cx),
        AgentEvent::ToolStarted {
            id,
            name,
            input,
            parent: None,
        } => {
            if open.replaying {
                return;
            }
            let agent_id = open.worker.agent.clone();
            // What the session asks of Argonauts Code reads as a plain line of what it did.
            let own = Own::named(name);
            if let Some(line) = own.and_then(|own| own.line(input)) {
                end_stream(open, cx);
                open.plain.push(id.clone());
                add_call_folded(open, line.with_key(id.clone()), false, cx);
                print_done(open, cx);
                return;
            }
            // It doesn't end the answer streaming, so the answer stays one block. Claude
            // Code's own workflow shows as the workflow it reports, not as the script it ran.
            if own.is_some() || kept_out(&agent_id, name) {
                if own == Some(Own::StartWorkflow) {
                    open.starts.push(id.clone());
                }
                let call = tools::start_call(&agent_id, name, input).with_key(id.clone());
                open.own.push(call);
                return;
            }
            // Paths show relative to the folder the conversation works in.
            let places = crate::frame::state::Places::of_conversation(cx.state);
            let mut call =
                tools::start_call_in(&agent_id, name, input, &places).with_key(id.clone());
            call.kind = kinds(&agent_id, name, input);
            // A shell call shows on its own while its command is being written, and joins
            // the fold once it turns out only to look around.
            let fold = folds(&call, cx);
            match writing_place(open, id, cx) {
                Some(in_fold) if in_fold == fold => {
                    if let Some(written) = writing_mut(open, id, cx) {
                        written.fill_in(call);
                    }
                    return;
                }
                Some(_) => drop_writing(open, id, cx),
                None => {}
            }
            end_stream(open, cx);
            add_call_folded(open, call, fold, cx);
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
                if open.starts.contains(id) {
                    open.refused = own_calls::turned_down(*ok, output.as_deref());
                }
                return;
            }
            let plain = open.plain.contains(id);
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
                    if plain {
                        // Its answer is the session's to read; the line already says it.
                        call.finish(*ok);
                    } else {
                        tools::finish_call(call, *ok, output.as_deref());
                    }
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
            let places = crate::frame::state::Places::of_conversation(cx.state);
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
                    tools::take_patch_in(call, path, hunks, &places);
                    break;
                }
            }
        }
        AgentEvent::PluginOutput {
            kind,
            plugin,
            text,
            timeout_ms,
        } => {
            let line =
                plugin_output::show(*kind, plugin.as_deref(), text.as_deref(), *timeout_ms, cx);
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

/// The answer so far missed its format and the agent is asked for it again. If none of it
/// printed yet it goes; rows that printed stay, and the reply leaves out what they said.
fn retried(open: &mut OpenTurn, cx: &mut Cx<'_>) {
    open.retried = true;
    let dropped = open.replaying
        || open
            .stream
            .is_some_and(|id| cx.model.history.drop_unprinted_stream(id));
    if dropped {
        open.stream = None;
        open.text.clear();
        open.shown = 0;
        open.from_deltas = false;
    } else {
        end_stream(open, cx);
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
fn is_cli_workflow_run(agent_id: &str, name: &str) -> bool {
    agent_id == "claude-code" && name == "Workflow"
}

/// Calls the transcript leaves to Ctrl+O: how a session talks to Argonauts Code, the CLI's own
/// tool lookups, and Claude Code's own workflow, which shows as the workflow it reports.
fn kept_out(agent_id: &str, name: &str) -> bool {
    tools::is_own(name) || tools::is_tool_lookup(name) || is_cli_workflow_run(agent_id, name)
}

/// `Workflow(release-check · <what it does>)` once its script exists and reports, as Claude
/// Code titles its own call, and under it that it runs in the background and what opens it.
fn workflow_row(workflow: &CliWorkflowRun) -> ToolCall {
    let name = Some(workflow.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or("workflow");
    let title = match workflow.description.trim() {
        "" => sanitize_line(name).into_owned(),
        description => format!("{} · {}", sanitize_line(name), sanitize_line(description)),
    };
    let mut call = ToolCall::call(ToolKind::Other, "Workflow", title).with_state(ToolState::Done);
    call.push(ToolItem::Workflow(WorkflowResult::Running));
    call
}

/// Streams answer text, below any calls made so far.
fn deliver(open: &mut OpenTurn, text: &str, clock: &WallClock, cx: &mut Cx<'_>) {
    open.text.push_str(text);
    if open.replaying {
        return;
    }
    settle_tools(open, ToolState::Stopped, cx);
    let upto = tools::json_tail_from(&open.text, open.shown);
    show(open, upto, clock, cx);
}

/// Streams the answer's text up to byte `upto`.
fn show(open: &mut OpenTurn, upto: usize, clock: &WallClock, cx: &mut Cx<'_>) {
    if upto <= open.shown {
        return;
    }
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
    history.push_stream(id, &open.text[open.shown..upto]);
    open.shown = upto;
}

/// Ends the part of the answer streaming now, so a call goes under it. Text held back as a
/// possible closing JSON object wasn't the close after all, so it shows.
fn end_stream(open: &mut OpenTurn, cx: &mut Cx<'_>) {
    if open.replaying {
        return;
    }
    if open.shown < open.text.len()
        && let Some(id) = open.stream
    {
        cx.model.history.push_stream(id, &open.text[open.shown..]);
    }
    if let Some(id) = open.stream.take() {
        cx.model.history.finish_stream(id);
    }
    if !open.text.trim().is_empty() {
        open.parts.push(std::mem::take(&mut open.text));
    }
    open.text.clear();
    open.shown = 0;
    open.from_deltas = false;
    keep_own(open, None, cx);
}

/// Keeps Argonauts Code's own calls that ended for the transcript overlay, after the answer they
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

/// Whether the newest live cell is a fold line, which a call being written may join.
fn fold_is_newest(open: &OpenTurn, cx: &Cx<'_>) -> bool {
    open.calls
        .last()
        .and_then(|cell| cx.model.history.active(*cell))
        .is_some_and(|cell| matches!(cell, Cell::Group(_)))
}

/// At `full` each call gets its own cell, at `normal` quiet calls in a row share a fold line,
/// and at `quiet` they all share one that goes without a trace. A shell call folds at
/// `normal` only when it just looks around, as in Claude Code.
pub(super) fn folds(call: &ToolCall, cx: &Cx<'_>) -> bool {
    match cx.verbosity() {
        Verbosity::Full => false,
        Verbosity::Normal if call.kind == ToolKind::Shell => tools::only_looks(call),
        Verbosity::Normal => call.kind.is_quiet(),
        Verbosity::Quiet => true,
    }
}

fn add_call_folded(open: &mut OpenTurn, call: ToolCall, folds: bool, cx: &mut Cx<'_>) {
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

/// The call with `key` the model is still writing, among the live calls.
fn writing_mut<'a>(open: &OpenTurn, key: &str, cx: &'a mut Cx<'_>) -> Option<&'a mut ToolCall> {
    let cell = open
        .calls
        .iter()
        .copied()
        .find(|cell| match cx.model.history.active(*cell) {
            Some(Cell::Tool(call)) => is_writing(call, key),
            Some(Cell::Group(group)) => group.calls.iter().any(|call| is_writing(call, key)),
            _ => false,
        })?;
    match cx.model.history.active_mut(cell)? {
        Cell::Tool(call) => Some(call),
        Cell::Group(group) => group.calls.iter_mut().find(|call| is_writing(call, key)),
        _ => None,
    }
}

/// Whether the call with `key` the model is writing sits in a fold line, or `None` when no
/// such call is live.
fn writing_place(open: &OpenTurn, key: &str, cx: &Cx<'_>) -> Option<bool> {
    open.calls
        .iter()
        .find_map(|cell| match cx.model.history.active(*cell) {
            Some(Cell::Tool(call)) if is_writing(call, key) => Some(false),
            Some(Cell::Group(group)) if group.calls.iter().any(|call| is_writing(call, key)) => {
                Some(true)
            }
            _ => None,
        })
}

fn is_writing(call: &ToolCall, key: &str) -> bool {
    call.state == ToolState::Writing && call.key.as_deref() == Some(key)
}

/// The model never finished writing the call with `key`, so its row goes.
fn drop_writing(open: &mut OpenTurn, key: &str, cx: &mut Cx<'_>) {
    let history = &mut cx.model.history;
    open.calls.retain(|cell| match history.active_mut(*cell) {
        Some(Cell::Tool(call)) if is_writing(call, key) => {
            history.remove_active(*cell);
            false
        }
        Some(Cell::Group(group)) => {
            group.calls.retain(|call| !is_writing(call, key));
            if group.calls.is_empty() {
                history.remove_active(*cell);
                return false;
            }
            true
        }
        _ => true,
    });
}

/// Prints the calls once text follows them or the turn ends, marking any still running as
/// `state`. At `quiet` they go without a trace, and a call the model never finished writing
/// goes at any verbosity.
fn settle_tools(open: &mut OpenTurn, state: ToolState, cx: &mut Cx<'_>) {
    let quiet = quiet(cx);
    let history = &mut cx.model.history;
    for id in std::mem::take(&mut open.calls) {
        let unwritten = match history.active_mut(id) {
            Some(Cell::Tool(call)) => call.state == ToolState::Writing,
            Some(Cell::Group(group)) => {
                group.calls.retain(|call| call.state != ToolState::Writing);
                group.calls.is_empty()
            }
            _ => false,
        };
        if quiet || unwritten {
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
/// next agent was told under it, or the one line of a CLI that turned out signed out. What the
/// first agent did stays above, and the next one's work streams below as the same turn.
fn fell_back(
    open: &mut OpenTurn,
    from: &ModelRef,
    to: &ModelRef,
    why: &str,
    told: &str,
    signed_out: Option<&SignedOut>,
    cx: &mut Cx<'_>,
) {
    end_parts(open, ToolState::Stopped, cx);
    open.parts.clear();
    open.text.clear();
    open.shown = 0;
    open.from_deltas = false;
    open.worker = to.clone();
    if open.replaying {
        return;
    }
    let names = &cx.state.names;
    if let Some(out) = signed_out {
        let line = signed_out_line(&names.cli(&from.agent), out, &names.cli(&to.agent));
        emit(cx, line);
        return;
    }
    let next = names.cli(&to.agent);
    let text = match names.model(&to.agent, to.model.as_deref()) {
        Some(model) => format!(
            "{} {why} · {next} continues with {model}",
            names.cli(&from.agent)
        ),
        None => format!("{} {why} · {next} continues", names.cli(&from.agent)),
    };
    let mut notice = Notice::new(Tone::Normal, text).with_glyph("↷");
    if !told.trim().is_empty() {
        notice = notice.with_detail(format!("Told: {told}"));
    }
    emit(cx, notice);
}

/// `● Claude Code is signed out (its sign-in was revoked) · run claude /login · Codex answers
/// meanwhile`: one line, with the command that signs it in again at full strength.
fn signed_out_line(cli: &str, out: &SignedOut, next: &str) -> Notice {
    let mut trailing = Vec::new();
    let why = out.why.trim();
    if !why.is_empty() {
        trailing.push(Trailing::Dim(format!(" ({why})")));
    }
    if let Some(login) = out
        .login
        .as_deref()
        .map(str::trim)
        .filter(|login| !login.is_empty())
    {
        trailing.push(Trailing::Dim(" · run ".to_owned()));
        trailing.push(Trailing::Command(login.to_owned()));
    }
    trailing.push(Trailing::Dim(format!(" · {next} answers meanwhile")));
    Notice::headline(Tone::Warning, format!("{cli} is signed out"), None).with_trailing(trailing)
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
        say_refused(open, cx);
        return;
    }
    end_stream(open, cx);
    keep_own(open, Some(state), cx);
    say_refused(open, cx);
}

/// The line of a start turned down that no later start in the turn made good.
fn say_refused(open: &mut OpenTurn, cx: &mut Cx<'_>) {
    if let Some(line) = open.refused.take() {
        emit(cx, line);
    }
}

/// Shown above the answer in the transcript overlay, like `10:42 AM claude-code/haiku`. The
/// overlay is the detail view, so it keeps the id.
fn stamp(worker: &ModelRef, clock: &WallClock) -> String {
    format!("{} {worker}", clock.clock_now())
}
