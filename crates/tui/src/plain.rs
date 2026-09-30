//! Append-only plain text output for screen readers, CI logs, and pipes.
//! Lines are flat and labeled, like `claude-code: Run npm test`, with no colors or redraws.

use std::collections::HashMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use argonauts_core::{
    AgentEvent, BudgetAction, BudgetLimit, BudgetMeter, BudgetStatus, CardId, Checkpoint,
    CommitInfo, DEFAULT_SOFT_RATIO, DecisionRecord, DiffStat, GateResult, MergeCard, MergeDecision,
    Note, NoteLevel, Phase, PhaseEvent, PhaseStatus, Report, RunEvent, RunEventKind, RunFinished,
    RunOutcome, RunStarted, SessionEnded, SessionStarted, StopCause, TodoItem, TodoStatus,
    Verbosity,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::UiOptions;
use crate::channel::{MergePrompt, UiEvent};
use crate::format;
use crate::model::{
    category_word, decider_word, decision_text, phase_name, retry_word, role_word, shows,
};
use crate::sanitize::{clean, clean_line};
use crate::text::truncate;
use crate::tools::{DiffSign, ToolSummary, tail_lines};

const FIELD_MAX: usize = 300;
const NORMAL_OUTPUT_LINES: usize = 3;
const FULL_OUTPUT_LINES: usize = 12;

/// Streaming text for one source, printed a whole line at a time.
#[derive(Default)]
struct TextStream {
    text: String,
    printed: usize,
}

pub struct PlainRenderer {
    verbosity: Verbosity,
    home: Option<PathBuf>,
    streams: HashMap<String, TextStream>,
    last_message: HashMap<String, String>,
    tools: HashMap<String, ToolSummary>,
    cwd: Option<PathBuf>,
    agent: Option<String>,
    session_meter: BudgetMeter,
    budget_level: BudgetAction,
    /// The last merge card printed, so its prompt doesn't print it again.
    card_printed: Option<Option<CardId>>,
    report_printed: bool,
}

impl PlainRenderer {
    pub fn new(options: &UiOptions) -> Self {
        Self {
            verbosity: options.verbosity,
            home: options.home_dir.clone(),
            streams: HashMap::new(),
            last_message: HashMap::new(),
            tools: HashMap::new(),
            cwd: None,
            agent: None,
            session_meter: BudgetMeter::new(BudgetLimit::None, DEFAULT_SOFT_RATIO),
            budget_level: BudgetAction::Continue,
            card_printed: None,
            report_printed: false,
        }
    }

    fn shows(&self, needed: Verbosity) -> bool {
        shows(self.verbosity, needed)
    }

    fn output_limit(&self) -> usize {
        if self.shows(Verbosity::Full) {
            FULL_OUTPUT_LINES
        } else {
            NORMAL_OUTPUT_LINES
        }
    }

    pub fn event(&mut self, event: &RunEvent) -> Vec<String> {
        let mut out = Vec::new();
        match &event.kind {
            RunEventKind::RunStarted(started) => self.run_started(event, started, &mut out),
            RunEventKind::Agent(agent) => self.agent(&event.source, agent, &mut out),
            RunEventKind::Phase(phase) => self.phase(phase, &mut out),
            RunEventKind::SessionStarted(started) => self.session_started(started, &mut out),
            RunEventKind::SessionEnded(ended) => self.session_ended(ended, &mut out),
            RunEventKind::GateResult(gate) => self.gate(gate, &mut out),
            RunEventKind::Commit(commit) => self.commit(commit, &mut out),
            RunEventKind::Report(report) => self.report(report, &mut out),
            RunEventKind::Checkpoint(Checkpoint::Merge(card)) => {
                self.card(event.card_id.as_ref(), card, &mut out);
            }
            RunEventKind::Decision(record) => out.push(decision_line(record)),
            RunEventKind::Budget(status) => self.budget(status, &mut out),
            RunEventKind::Note(note) => self.note(note, &mut out),
            RunEventKind::RunFinished(finished) => self.run_finished(finished, &mut out),
        }
        out
    }

    pub fn merge_prompt(&mut self, card_id: &CardId, card: &MergeCard) -> Vec<String> {
        let mut out = Vec::new();
        let already_printed = matches!(&self.card_printed, Some(Some(id)) if id == card_id);
        if !already_printed {
            self.card(Some(card_id), card, &mut out);
        }
        out.push(format!(
            "merge {card_id}? a approve and merge (then y to confirm), d diff, c request changes, p open PR, l leave"
        ));
        out
    }

    pub fn flush(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        self.flush_streams(&mut out);
        out
    }

    fn run_started(&mut self, event: &RunEvent, started: &RunStarted, out: &mut Vec<String>) {
        let mut agent = clean_line(&started.agent);
        if let Some(model) = &started.model {
            agent.push_str(&format!("/{}", clean_line(model)));
        }
        let mut line = format!(
            "argonauts: run {}, {}, {agent}",
            event.run_id,
            clean_line(&started.mode)
        );
        if let Some(effort) = &started.effort {
            line.push_str(&format!(", effort {}", clean_line(effort)));
        }
        out.push(line);
        let repo = format::display_path(&started.repo_root, self.home.as_deref());
        let mut repo_line = format!(
            "repo: {} at {}",
            clean_line(&repo),
            format::short_sha(&started.base_commit)
        );
        if let Some(branch) = &started.base_branch {
            repo_line.push_str(&format!(" on {}", clean_line(branch)));
        }
        out.push(repo_line);
        let task = clean(&started.task);
        let mut lines = task.lines().filter(|l| !l.trim().is_empty());
        if let Some(first) = lines.next() {
            out.push(format!("task: {}", first.trim()));
        }
        if self.shows(Verbosity::Full) {
            out.extend(lines.map(|l| format!("task: {}", l.trim())));
        }
    }

    fn phase(&self, phase: &PhaseEvent, out: &mut Vec<String>) {
        let name = phase_name(phase.phase).to_lowercase();
        let detail = phase.detail.as_deref().map(clean_line);
        let line = |text: String| match &detail {
            Some(d) => format!("{text}, {d}"),
            None => text,
        };
        let finished_from = match phase.phase {
            Phase::Goal | Phase::Worktree => Verbosity::Normal,
            _ => Verbosity::Full,
        };
        match phase.status {
            PhaseStatus::Finished if phase.phase == Phase::Goal && self.shows(finished_from) => {
                out.push(format!("goal: {}", detail.unwrap_or_default()));
            }
            PhaseStatus::Started if self.shows(Verbosity::Full) => {
                out.push(line(format!("{name} started")));
            }
            PhaseStatus::Finished if self.shows(finished_from) => {
                let done = if phase.phase == Phase::Worktree {
                    "ready"
                } else {
                    "done"
                };
                out.push(line(format!("{name} {done}")));
            }
            PhaseStatus::Failed => out.push(line(format!("{name} failed"))),
            PhaseStatus::Skipped if self.shows(Verbosity::Normal) => {
                out.push(line(format!("{name} skipped")));
            }
            _ => {}
        }
    }

    fn session_started(&mut self, started: &SessionStarted, out: &mut Vec<String>) {
        let agent = clean_line(&started.agent);
        self.agent = Some(agent.clone());
        self.cwd = Some(started.cwd.clone());
        self.session_meter = BudgetMeter::new(BudgetLimit::None, DEFAULT_SOFT_RATIO);
        if self.shows(Verbosity::Normal) {
            let mut line = format!("{agent}: started as {}", role_word(started.role));
            if let Some(model) = &started.model {
                line.push_str(&format!(", model {}", clean_line(model)));
            }
            if let Some(effort) = &started.effort {
                line.push_str(&format!(", effort {}", clean_line(effort)));
            }
            out.push(line);
        }
        if self.shows(Verbosity::Full) {
            let command = format::command_line(&started.program, &started.args);
            out.push(format!(
                "{agent}: command: {}",
                truncate(&clean_line(&command), FIELD_MAX)
            ));
        }
    }

    fn session_ended(&mut self, ended: &SessionEnded, out: &mut Vec<String>) {
        self.flush_streams(out);
        let agent = self.agent.as_deref().unwrap_or("agent");
        let exit = match (ended.exit_code, ended.signal) {
            (Some(code), _) => format!("exit {code}"),
            (None, Some(signal)) => format!("signal {signal}"),
            (None, None) => "no exit code".to_owned(),
        };
        let mut line = format!(
            "{agent}: session ended, {exit}, {}, {} tokens",
            format::seconds(ended.duration_ms),
            format::tokens(self.session_meter.spend().billable_tokens())
        );
        if let Some(cause) = ended.stopped_by {
            let why = match cause {
                StopCause::Budget => "the budget",
                StopCause::User => "you",
                StopCause::Timeout => "a timeout",
            };
            line.push_str(&format!(", stopped by {why}"));
        }
        out.push(line);
    }

    fn agent(&mut self, source: &str, event: &AgentEvent, out: &mut Vec<String>) {
        let who = clean_line(source);
        let normal = self.shows(Verbosity::Normal);
        let full = self.shows(Verbosity::Full);
        match event {
            AgentEvent::Init {
                session_id, model, ..
            } if full => {
                let id = session_id.as_deref().map_or("unknown".into(), clean_line);
                let model = model.as_deref().map_or("unknown".into(), clean_line);
                out.push(format!("{who}: session {id}, model {model}"));
            }
            AgentEvent::TextDelta { text, parent: None } if normal => {
                let stream = self.streams.entry(who.clone()).or_default();
                stream.text.push_str(&clean(text));
                print_complete_lines(&who, stream, out);
            }
            AgentEvent::Message { text, parent: None } if normal => {
                self.message(&who, &clean(text), out);
            }
            AgentEvent::Message {
                text,
                parent: Some(_),
            } if full => print_labeled(&format!("{who} (subagent)"), &clean(text), out),
            AgentEvent::Reasoning { text } if full => {
                print_labeled(&format!("{who} thinking"), &clean(text), out);
            }
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => self.tool_started(&who, id, name, input, parent.is_some(), out),
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                self.tool_finished(&who, id, *ok, output.as_deref(), out);
            }
            AgentEvent::FileChanged { path, kind } if full => {
                out.push(format!(
                    "{who}: file {} {}",
                    clean_line(kind),
                    clean_line(path)
                ));
            }
            AgentEvent::Todo { items } if normal => {
                out.push(format!("{who}: todo: {}", todo_list(items)));
            }
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cached_tokens,
                cost_usd,
                ..
            } => {
                self.session_meter.observe(event);
                if full {
                    let cost = cost_usd
                        .map(|cost| format!(", {}", format::usd(cost, false)))
                        .unwrap_or_default();
                    out.push(format!(
                        "{who}: usage {input_tokens} in ({cached_tokens} cached), {output_tokens} out{cost}"
                    ));
                }
            }
            AgentEvent::Retry { reason, delay_ms } if normal => {
                let delay = delay_ms
                    .map(|ms| format!(", in {}", format::seconds(ms)))
                    .unwrap_or_default();
                out.push(format!("{who}: retrying, {}{delay}", retry_word(reason)));
            }
            AgentEvent::QuotaSignal { detail } if full => {
                let raw = truncate(&clean_line(&detail.to_string()), FIELD_MAX);
                out.push(format!("{who}: quota signal {raw}"));
            }
            AgentEvent::Error {
                category, message, ..
            } => {
                out.push(format!(
                    "{who}: error, {}: {}",
                    category_word(*category),
                    clean_line(message)
                ));
            }
            AgentEvent::Result { text, is_error, .. } => {
                self.result(&who, text.as_deref(), *is_error, out);
            }
            AgentEvent::Unknown { raw } if normal => {
                let raw = truncate(&clean_line(&raw.to_string()), FIELD_MAX);
                out.push(format!("{who}: unknown event: {raw}"));
            }
            // Everything else is hidden at this verbosity. Subagent text deltas are never shown.
            AgentEvent::Init { .. }
            | AgentEvent::TextDelta { .. }
            | AgentEvent::Message { .. }
            | AgentEvent::Reasoning { .. }
            | AgentEvent::FileChanged { .. }
            | AgentEvent::Todo { .. }
            | AgentEvent::Retry { .. }
            | AgentEvent::QuotaSignal { .. }
            | AgentEvent::Subagent { .. }
            | AgentEvent::Quota { .. }
            | AgentEvent::ContextFill { .. }
            | AgentEvent::Unknown { .. } => {}
        }
    }

    fn tool_started(
        &mut self,
        who: &str,
        id: &str,
        name: &str,
        input: &Value,
        from_subagent: bool,
        out: &mut Vec<String>,
    ) {
        if !from_subagent {
            self.flush_stream(who, out);
        }
        let summary = ToolSummary::from_call(name, input, self.cwd.as_deref());
        let (label, needed) = if from_subagent {
            (" (subagent)", Verbosity::Full)
        } else {
            ("", Verbosity::Normal)
        };
        if self.shows(needed) {
            out.push(format!("{who}{label}: {}", summary.describe()));
        }
        if self.shows(Verbosity::Full) {
            diff_lines(&summary, out);
        }
        self.tools.insert(id.to_owned(), summary);
    }

    fn tool_finished(
        &self,
        who: &str,
        id: &str,
        ok: bool,
        output: Option<&str>,
        out: &mut Vec<String>,
    ) {
        let outcome = if !ok && self.shows(Verbosity::Normal) {
            "failed"
        } else if self.shows(Verbosity::Full) {
            "done"
        } else {
            return;
        };
        let tool = self
            .tools
            .get(id)
            .map_or_else(|| format!("tool {}", clean_line(id)), ToolSummary::describe);
        out.push(format!("{who}: {tool} {outcome}"));
        output_lines(output, self.output_limit(), out);
    }

    fn result(&mut self, who: &str, text: Option<&str>, is_error: bool, out: &mut Vec<String>) {
        self.flush_stream(who, out);
        let normal = self.shows(Verbosity::Normal);
        if normal && let Some(text) = text.map(clean) {
            let text = text.trim();
            let repeated = self.last_message.get(who).is_some_and(|last| last == text);
            if !text.is_empty() && !repeated {
                self.message(who, text, out);
            }
        }
        if is_error {
            out.push(format!("{who}: finished with an error"));
        } else if normal {
            out.push(format!("{who}: finished"));
        }
    }

    fn message(&mut self, who: &str, text: &str, out: &mut Vec<String>) {
        let stream = self.streams.remove(who).unwrap_or_default();
        print_rest(who, &stream, out);
        let streamed = stream.text.trim();
        let already_streamed = !streamed.is_empty() && streamed == text.trim();
        if !already_streamed {
            print_lines(who, text, out);
        }
        self.last_message
            .insert(who.to_owned(), text.trim().to_owned());
    }

    fn flush_stream(&mut self, who: &str, out: &mut Vec<String>) {
        if let Some(stream) = self.streams.remove(who) {
            print_rest(who, &stream, out);
            let text = stream.text.trim();
            if !text.is_empty() {
                self.last_message.insert(who.to_owned(), text.to_owned());
            }
        }
    }

    fn flush_streams(&mut self, out: &mut Vec<String>) {
        let sources: Vec<String> = self.streams.keys().cloned().collect();
        for source in sources {
            self.flush_stream(&source, out);
        }
    }

    fn gate(&self, gate: &GateResult, out: &mut Vec<String>) {
        let command = clean_line(&gate.command.join(" "));
        let took = format::seconds(gate.duration_ms);
        if gate.passed {
            if self.shows(Verbosity::Normal) {
                out.push(format!("gate passed: {command} ({took})"));
            }
            if self.shows(Verbosity::Full) {
                output_lines(Some(&gate.output), FULL_OUTPUT_LINES, out);
            }
            return;
        }
        let exit = gate
            .exit_code
            .map_or_else(|| "no exit code".to_owned(), |c| format!("exit {c}"));
        out.push(format!("gate failed: {command}, {exit} ({took})"));
        output_lines(Some(&gate.output), self.output_limit(), out);
    }

    fn commit(&self, commit: &CommitInfo, out: &mut Vec<String>) {
        match &commit.sha {
            Some(sha) if self.shows(Verbosity::Normal) => out.push(format!(
                "commit: {} on {}, {}",
                format::short_sha(&clean_line(sha)),
                clean_line(&commit.branch),
                plain_stat(&commit.stat)
            )),
            Some(_) => {}
            None => out.push("commit: nothing to commit, the agent changed no files".to_owned()),
        }
    }

    fn report(&mut self, report: &Report, out: &mut Vec<String>) {
        self.report_printed = true;
        out.push(report_line(report));
        if self.shows(Verbosity::Normal) {
            for concern in &report.concerns {
                out.push(format!("concern: {}", clean_line(concern)));
            }
        }
        if self.shows(Verbosity::Full) {
            for file in &report.files_changed {
                out.push(format!("changed: {}", clean_line(file)));
            }
            for test in &report.tests_run {
                out.push(format!("tested: {}", clean_line(test)));
            }
        }
    }

    fn card(&mut self, card_id: Option<&CardId>, card: &MergeCard, out: &mut Vec<String>) {
        self.card_printed = Some(card_id.cloned());
        let label = card_id.map_or_else(|| "card".to_owned(), ToString::to_string);
        let target = card
            .target_branch
            .as_deref()
            .map_or_else(|| "the base branch".to_owned(), clean_line);
        out.push(format!(
            "merge card {label}: {} into {target}, {}",
            clean_line(&card.branch),
            plain_stat(&card.stat)
        ));
        if !card.gates.is_empty() {
            let passed = card.gates.iter().filter(|g| g.passed).count();
            out.push(format!("gates: {passed} of {} passed", card.gates.len()));
        }
        if let Some(status) = &card.budget {
            out.push(format!(
                "spent: {}",
                format::spend(&status.spend, status.limit)
            ));
        }
        if let Some(cause) = card.stopped_by {
            let text = format::stopped_card(cause, card.report.is_some());
            out.push(format!("stopped: {}", text.replace(" · ", ", ")));
        }
        if let Some(report) = card.report.as_ref().filter(|_| !self.report_printed) {
            out.push(report_line(report));
        }
    }

    fn budget(&mut self, status: &BudgetStatus, out: &mut Vec<String>) {
        if status.level == self.budget_level {
            return;
        }
        self.budget_level = status.level;
        // What the conductor does about it arrives as a note.
        let limit = format::budget_limit(status);
        match status.level {
            BudgetAction::Continue => {}
            BudgetAction::WrapUp => {
                let used = status
                    .fraction_used
                    .map_or_else(|| "most".to_owned(), |f| format!("{:.0}%", f * 100.0));
                out.push(format!("budget: {used} used of {limit}"));
            }
            BudgetAction::Stop => out.push(format!("budget: used up, {limit}")),
        }
    }

    fn note(&self, note: &Note, out: &mut Vec<String>) {
        let (label, needed) = match note.level {
            NoteLevel::Info => ("note", Verbosity::Normal),
            NoteLevel::Warn => ("warning", Verbosity::Quiet),
            NoteLevel::Error => ("error", Verbosity::Quiet),
        };
        if self.shows(needed) {
            print_labeled(label, &clean(&note.text), out);
        }
    }

    fn run_finished(&mut self, finished: &RunFinished, out: &mut Vec<String>) {
        self.flush_streams(out);
        let outcome = match finished.outcome {
            RunOutcome::Merged => "merged",
            RunOutcome::BranchLeft => "branch left",
            RunOutcome::Failed => "failed",
            RunOutcome::Cancelled => "cancelled",
        };
        out.push(format!("run finished: {outcome}"));
        if let Some(detail) = &finished.detail {
            for line in clean(detail).lines().filter(|l| !l.trim().is_empty()) {
                out.push(format!("  {}", line.trim_end()));
            }
        }
    }
}

fn decision_line(record: &DecisionRecord) -> String {
    format!(
        "decision by {}: {}",
        decider_word(record.by),
        decision_text(&record.decision)
    )
}

fn report_line(report: &Report) -> String {
    format!(
        "report: {}, {}",
        report.status.as_str().replace('_', " "),
        clean_line(&report.summary)
    )
}

/// `write hello.txt (done), report (in progress)`.
fn todo_list(items: &[TodoItem]) -> String {
    items
        .iter()
        .map(|item| {
            let status = match item.status {
                TodoStatus::Completed => "done",
                TodoStatus::InProgress => "in progress",
                TodoStatus::Pending => "pending",
            };
            format!("{} ({status})", clean_line(&item.text))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `1 file, +1 −0`, with commas that read better aloud than dots.
fn plain_stat(stat: &DiffStat) -> String {
    format!(
        "{}, +{} −{}",
        format::count(stat.files_changed as u64, "file", "files"),
        stat.insertions,
        stat.deletions
    )
}

fn diff_lines(summary: &ToolSummary, out: &mut Vec<String>) {
    for line in &summary.diff {
        let sign = match line.sign {
            DiffSign::Added => '+',
            DiffSign::Removed => '-',
        };
        out.push(format!("    {sign} {}", line.text));
    }
}

fn print_labeled(label: &str, text: &str, out: &mut Vec<String>) {
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        out.push(format!("{label}: {line}"));
    }
}

fn print_lines(who: &str, text: &str, out: &mut Vec<String>) {
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        out.push(format!("{who}: {}", line.trim_end()));
    }
}

fn print_complete_lines(who: &str, stream: &mut TextStream, out: &mut Vec<String>) {
    if let Some(end) = stream.text.rfind('\n')
        && end >= stream.printed
    {
        print_lines(who, &stream.text[stream.printed..end], out);
        stream.printed = end + 1;
    }
}

fn print_rest(who: &str, stream: &TextStream, out: &mut Vec<String>) {
    if let Some(rest) = stream.text.get(stream.printed..) {
        print_lines(who, rest, out);
    }
}

fn output_lines(output: Option<&str>, max: usize, out: &mut Vec<String>) {
    let Some(output) = output else { return };
    let (lines, skipped) = tail_lines(output, max);
    if skipped > 0 {
        out.push(format!("    … {skipped} earlier lines"));
    }
    out.extend(
        lines
            .into_iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| format!("    {l}")),
    );
}

/// Blocks, so run it on a blocking thread. When `input` ends before an answer, the card's
/// branch is left alone.
pub fn run_plain<W: Write, R: BufRead>(
    mut events: mpsc::UnboundedReceiver<UiEvent>,
    mut out: W,
    mut input: R,
    options: &UiOptions,
) -> io::Result<()> {
    let mut renderer = PlainRenderer::new(options);
    while let Some(event) = events.blocking_recv() {
        match event {
            UiEvent::Run(event) => write_lines(&mut out, &renderer.event(&event))?,
            UiEvent::MergeCardShown(prompt) => {
                write_lines(&mut out, &renderer.flush())?;
                write_lines(
                    &mut out,
                    &renderer.merge_prompt(&prompt.card_id, &prompt.card),
                )?;
                if options.bell {
                    out.write_all(b"\x07")?;
                }
                let decision = ask_for_decision(&mut out, &mut input, &prompt)?;
                // The conductor may have stopped waiting; then there is nothing left to tell it.
                let _ = prompt.reply.send(decision);
            }
        }
    }
    write_lines(&mut out, &renderer.flush())?;
    out.flush()
}

/// The bell rings only when stdout is a terminal, so logs and pipes stay clean.
pub async fn run_plain_stdio(
    events: mpsc::UnboundedReceiver<UiEvent>,
    mut options: UiOptions,
) -> anyhow::Result<()> {
    options.bell &= io::stdout().is_terminal();
    tokio::task::spawn_blocking(move || {
        run_plain(events, io::stdout().lock(), io::stdin().lock(), &options)
    })
    .await??;
    Ok(())
}

fn write_lines<W: Write>(out: &mut W, lines: &[String]) -> io::Result<()> {
    for line in lines {
        writeln!(out, "{}", clean_line(line))?;
    }
    out.flush()
}

fn ask_for_decision<W: Write, R: BufRead>(
    out: &mut W,
    input: &mut R,
    prompt: &MergePrompt,
) -> io::Result<MergeDecision> {
    let target = prompt
        .card
        .target_branch
        .as_deref()
        .map_or_else(|| "the starting branch".to_owned(), clean_line);
    loop {
        let Some(answer) = ask(out, input, "choice [a/d/c/p/l]: ")? else {
            writeln!(out)?;
            writeln!(out, "no answer, leaving the branch")?;
            return Ok(MergeDecision::Leave);
        };
        match parse_choice(&answer) {
            // Two answers, so one stray line can't merge.
            Some(Choice::Decide(MergeDecision::Approve)) => {
                let question = format!("merge into {target}? type y to confirm: ");
                let confirmed = ask(out, input, &question)?.is_some_and(|reply| {
                    matches!(reply.trim().to_lowercase().as_str(), "y" | "yes")
                });
                if confirmed {
                    return Ok(MergeDecision::Approve);
                }
                writeln!(out, "not merged")?;
            }
            Some(Choice::Decide(decision)) => return Ok(decision),
            Some(Choice::Diff) => write_diff(out, prompt.diff.as_deref())?,
            Some(Choice::RequestChanges) => {
                let note = ask(out, input, "what should change (Enter for no note): ")?
                    .map(|n| clean_line(n.trim()))
                    .filter(|n| !n.is_empty());
                return Ok(MergeDecision::RequestChanges { note });
            }
            None => writeln!(out, "type a, d, c, p, or l")?,
        }
    }
}

/// Writes `question` and reads one line of answer, or `None` when `input` has ended.
fn ask<W: Write, R: BufRead>(
    out: &mut W,
    input: &mut R,
    question: &str,
) -> io::Result<Option<String>> {
    out.write_all(question.as_bytes())?;
    out.flush()?;
    let mut line = String::new();
    match input.read_line(&mut line)? {
        0 => Ok(None),
        _ => Ok(Some(line)),
    }
}

fn write_diff<W: Write>(out: &mut W, diff: Option<&str>) -> io::Result<()> {
    let Some(diff) = diff else {
        return writeln!(out, "no diff came with this card");
    };
    for line in clean(diff).lines() {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Choice {
    Decide(MergeDecision),
    Diff,
    RequestChanges,
}

fn parse_choice(answer: &str) -> Option<Choice> {
    match answer.trim().to_lowercase().as_str() {
        "a" | "approve" => Some(Choice::Decide(MergeDecision::Approve)),
        "p" | "pr" | "open pr" => Some(Choice::Decide(MergeDecision::OpenPr)),
        "l" | "leave" => Some(Choice::Decide(MergeDecision::Leave)),
        "d" | "diff" => Some(Choice::Diff),
        "c" | "changes" | "request changes" => Some(Choice::RequestChanges),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use argonauts_core::{KeyScheme, Phase};
    use serde_json::json;

    use super::*;
    use crate::testing::{DIFF, Script, merge_card, solo_run_events};

    fn options(verbosity: Verbosity) -> UiOptions {
        UiOptions {
            verbosity,
            keys: KeyScheme::Standard,
            color: false,
            bell: false,
            home_dir: Some("/home/you".into()),
        }
    }

    fn render_all(events: &[RunEvent], verbosity: Verbosity) -> Vec<String> {
        let mut renderer = PlainRenderer::new(&options(verbosity));
        let mut lines: Vec<String> = events.iter().flat_map(|e| renderer.event(e)).collect();
        lines.extend(renderer.flush());
        lines
    }

    #[test]
    fn normal_solo_run() {
        insta::assert_snapshot!(render_all(&solo_run_events(), Verbosity::Normal).join("\n"));
    }

    #[test]
    fn quiet_solo_run() {
        insta::assert_snapshot!(render_all(&solo_run_events(), Verbosity::Quiet).join("\n"));
    }

    #[test]
    fn full_solo_run() {
        insta::assert_snapshot!(render_all(&solo_run_events(), Verbosity::Full).join("\n"));
    }

    #[test]
    fn streamed_text_prints_whole_lines_once() {
        let mut s = Script::new();
        s.session_started();
        s.text("first line\nsec");
        s.text("ond line");
        s.message("first line\nsecond line");
        s.text("after");
        s.tool("t", "Read", json!({"file_path": "a"}));
        let lines = render_all(&s.events, Verbosity::Normal);
        assert_eq!(
            lines,
            vec![
                "claude-code: started as owner, model haiku, effort low",
                "claude-code: first line",
                "claude-code: second line",
                "claude-code: after",
                "claude-code: Read a",
            ]
        );
    }

    #[test]
    fn whole_messages_without_deltas_print() {
        let mut s = Script::new();
        s.message("done\n\nall good");
        s.agent(AgentEvent::Result {
            text: Some("done\n\nall good".into()),
            structured: None,
            is_error: false,
        });
        let lines = render_all(&s.events, Verbosity::Normal);
        assert_eq!(
            lines,
            vec![
                "claude-code: done",
                "claude-code: all good",
                "claude-code: finished"
            ]
        );
    }

    #[test]
    fn failures_show_even_when_quiet() {
        let mut s = Script::new();
        s.session_started();
        s.agent(AgentEvent::Error {
            category: argonauts_core::ErrorCategory::UsageLimit,
            message: "limit reached".into(),
            retryable: false,
        });
        s.phase(Phase::Worktree, PhaseStatus::Failed, Some("disk full"));
        s.gate(&["npm", "test"], false, "1 failed");
        let lines = render_all(&s.events, Verbosity::Quiet);
        assert_eq!(
            lines,
            vec![
                "claude-code: error, usage limit: limit reached",
                "worktree failed, disk full",
                "gate failed: npm test, exit 1 (0.0 s)",
                "    1 failed",
            ]
        );
    }

    #[test]
    fn unknown_events_are_never_dropped() {
        let mut s = Script::new();
        s.agent(AgentEvent::unknown_line("{\"type\":\"brand_new\"}"));
        let lines = render_all(&s.events, Verbosity::Normal);
        assert_eq!(
            lines,
            vec!["claude-code: unknown event: {\"type\":\"brand_new\"}"]
        );
    }

    #[test]
    fn output_has_no_escape_sequences() {
        let mut s = Script::new();
        s.run_started("\x1b]0;evil\x07Fix \x1b[31mit");
        s.session_started();
        s.text("\x1b[2Jcleared\n");
        s.tool("t", "Bash", json!({"command": "echo \u{1b}[5mblink"}));
        s.finish_tool("t", false, Some("\x1b[31mred output\x1b[0m"));
        s.gate(&["\x1b[1mtest"], false, "\x1b[33mwarn");
        let lines = render_all(&s.events, Verbosity::Full);
        assert!(!lines.is_empty());
        for line in &lines {
            assert!(!line.chars().any(|c| c.is_control()), "{line:?}");
        }
        assert!(lines.contains(&"task: Fix it".to_owned()));
    }

    fn run_with_input(answer: &str, diff: Option<&str>) -> (MergeDecision, String) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (prompt, mut decision) =
            MergePrompt::new(CardId::numbered(1), merge_card(), diff.map(str::to_owned));
        tx.send(UiEvent::MergeCardShown(Box::new(prompt))).unwrap();
        drop(tx);
        let mut out = Vec::new();
        run_plain(
            rx,
            &mut out,
            Cursor::new(answer.to_owned()),
            &options(Verbosity::Normal),
        )
        .unwrap();
        (
            decision.try_recv().unwrap(),
            String::from_utf8(out).unwrap(),
        )
    }

    #[test]
    fn plain_merge_prompt_reads_the_answer() {
        let (decision, out) = run_with_input("a\ny\n", None);
        assert_eq!(decision, MergeDecision::Approve);
        assert!(out.contains("merge card card-001: argonauts/run-20260929-101500-a1b2/card-001"));
        assert!(out.contains("merge card-001? a approve and merge"));
        assert!(out.contains("merge into main? type y to confirm"));
    }

    #[test]
    fn plain_approval_needs_a_confirmation() {
        let (decision, out) = run_with_input("a\nl\n", None);
        assert_eq!(decision, MergeDecision::Leave);
        assert!(out.contains("not merged"), "{out}");
        let (decision, _) = run_with_input("a\n", None);
        assert_eq!(decision, MergeDecision::Leave);
    }

    #[test]
    fn a_stopped_card_says_its_work_may_be_partial() {
        let mut card = merge_card();
        card.stopped_by = Some(argonauts_core::StopCause::Budget);
        card.report = None;
        let mut renderer = PlainRenderer::new(&options(Verbosity::Normal));
        let lines = renderer.merge_prompt(&CardId::numbered(1), &card);
        assert!(
            lines.contains(&"stopped: budget hit, no report, partial work".to_owned()),
            "{lines:?}"
        );
    }

    #[test]
    fn the_goal_is_shown() {
        let mut s = Script::new();
        s.phase(
            Phase::Goal,
            PhaseStatus::Finished,
            Some("your prompt, recorded as the goal · sha256 9f86d08"),
        );
        let lines = render_all(&s.events, Verbosity::Normal);
        assert_eq!(
            lines,
            ["goal: your prompt, recorded as the goal · sha256 9f86d08"]
        );
    }

    #[test]
    fn plain_merge_prompt_shows_the_diff_then_asks_again() {
        let (decision, out) = run_with_input("d\nx\nl\n", Some(DIFF));
        assert_eq!(decision, MergeDecision::Leave);
        assert!(out.contains("+hi"));
        assert!(out.contains("type a, d, c, p, or l"));
    }

    #[test]
    fn plain_merge_prompt_takes_a_change_note() {
        let (decision, _) = run_with_input("c\nuse a fixture\n", None);
        assert_eq!(
            decision,
            MergeDecision::RequestChanges {
                note: Some("use a fixture".into())
            }
        );
    }

    #[test]
    fn plain_merge_prompt_leaves_on_end_of_input() {
        let (decision, out) = run_with_input("", None);
        assert_eq!(decision, MergeDecision::Leave);
        assert!(out.contains("no answer, leaving the branch"));
    }

    #[test]
    fn card_is_not_printed_twice() {
        let mut renderer = PlainRenderer::new(&options(Verbosity::Normal));
        let mut s = Script::new();
        s.checkpoint();
        let first = renderer.event(&s.events[0]);
        assert!(first[0].starts_with("merge card card-001"));
        let prompt = renderer.merge_prompt(&CardId::numbered(1), &merge_card());
        assert_eq!(prompt.len(), 1);
    }

    #[test]
    fn choices() {
        assert_eq!(
            parse_choice(" A \n"),
            Some(Choice::Decide(MergeDecision::Approve))
        );
        assert_eq!(parse_choice("diff"), Some(Choice::Diff));
        assert_eq!(parse_choice("c"), Some(Choice::RequestChanges));
        assert_eq!(parse_choice("maybe"), None);
    }
}
