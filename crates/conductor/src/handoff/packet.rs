//! The vendor-neutral handoff packet, built by code from one card's events: the card, the goal
//! hash, notes and decisions, the branch and commit, what happened (files read and edited,
//! commands and results, one line each), the last to-do list, gate results, why it stopped, the
//! last report, and the budget left. It becomes the `Handoff` prompt part.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use argonauts_core::events::card::{CardEvent, CardSpec, CardState, GateOutcome};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::events::session::{InputPart, InputPartKind, SessionEvent};
use argonauts_core::events::shared::{BlackboardEvent, BlackboardKind};
use argonauts_core::events::vendor::BudgetEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::{HandoffPacket, WorkSoFar};
use argonauts_core::names::SessionKey;
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::steps::card::CardReport;
use argonauts_core::{AgentEvent, DiffStat, TodoItem, TodoStatus};
use serde_json::Value;

use super::why::{one_line, stop_line};

/// The most "what happened" lines a packet keeps, the latest ones.
const MAX_HAPPENED: usize = 60;
const PATH_CHARS: usize = 200;
const COMMAND_CHARS: usize = 160;
const RESULT_CHARS: usize = 120;
const REPORT_CHARS: usize = 600;

pub fn build(card: &CardSpec, events: &[RunEvent]) -> HandoffPacket {
    build_with_budget(card, events, card.budget.limit)
}

/// Like [`build`], with the card's budget when the card doesn't set one itself.
pub fn build_with_budget(
    card: &CardSpec,
    events: &[RunEvent],
    limit: Option<BudgetAmount>,
) -> HandoffPacket {
    let mut reader = Reader::new(card);
    for event in events {
        reader.read(event);
    }
    reader.finish(limit)
}

/// The packet as the `Handoff` prompt part a new agent reads.
pub fn prompt_part(packet: &HandoffPacket) -> InputPart {
    let text = render(packet);
    InputPart {
        kind: InputPartKind::Handoff,
        label: "handoff".to_owned(),
        approx_tokens: u32::try_from(text.chars().count().div_ceil(4)).unwrap_or(u32::MAX),
        text,
        source: Some(packet.card.id.to_string()),
    }
}

/// Walks the events once. An event belongs to the card when it carries the card's id, or comes
/// from one of the card's sessions; the goal and design decisions are run-wide.
struct Reader<'a> {
    card: &'a CardSpec,
    run: Option<String>,
    sessions: HashSet<SessionKey>,
    goal: Option<String>,
    decisions: HashMap<String, String>,
    note_sources: Vec<String>,
    posted_decisions: Vec<String>,
    branch: Option<String>,
    commit: Option<String>,
    diff: Option<DiffStat>,
    committed: DiffStat,
    happened: Vec<String>,
    commands: HashMap<String, usize>,
    read_paths: HashSet<String>,
    edited_paths: HashSet<String>,
    last_todo: Vec<TodoItem>,
    gates: Vec<GateOutcome>,
    ended: Option<String>,
    state_reason: Option<String>,
    last_words: Option<CardReport>,
    last_message: Option<String>,
    used: Used,
    extended: Vec<BudgetAmount>,
}

#[derive(Default)]
struct Used {
    usd: f64,
    tokens: u64,
    minutes: f64,
}

impl<'a> Reader<'a> {
    fn new(card: &'a CardSpec) -> Self {
        Self {
            card,
            run: None,
            sessions: HashSet::new(),
            goal: None,
            decisions: HashMap::new(),
            note_sources: Vec::new(),
            posted_decisions: Vec::new(),
            branch: None,
            commit: None,
            diff: None,
            committed: DiffStat::default(),
            happened: Vec::new(),
            commands: HashMap::new(),
            read_paths: HashSet::new(),
            edited_paths: HashSet::new(),
            last_todo: Vec::new(),
            gates: Vec::new(),
            ended: None,
            state_reason: None,
            last_words: None,
            last_message: None,
            used: Used::default(),
            extended: Vec::new(),
        }
    }

    fn read(&mut self, event: &RunEvent) {
        self.run.get_or_insert_with(|| event.run_id.to_string());
        match &event.kind {
            RunEventKind::Goal(GoalEvent::Approved { file, .. }) => {
                self.goal = Some(file.sha256.clone());
            }
            RunEventKind::Goal(GoalEvent::ChangeResolved {
                new_sha256: Some(sha),
                ..
            }) => self.goal = Some(sha.clone()),
            RunEventKind::Plan(PlanEvent::Decided(decision)) => {
                let line = format!(
                    "{} {}: {}",
                    decision.id,
                    one_line(&decision.title, RESULT_CHARS),
                    one_line(&decision.choice, RESULT_CHARS)
                );
                self.decisions.insert(decision.id.to_string(), line);
            }
            _ => {}
        }
        let own_card = event.card_id.as_ref() == Some(&self.card.id);
        if own_card && let Some(session) = &event.session {
            self.sessions.insert(session.clone());
        }
        let own_session = event
            .session
            .as_ref()
            .is_some_and(|session| self.sessions.contains(session));
        if let RunEventKind::Blackboard(BlackboardEvent::Posted(message)) = &event.kind
            && message.kind == BlackboardKind::Decision
            && (own_card || message.refs.iter().any(|r| r == self.card.id.as_str()))
        {
            self.posted_decisions
                .push(one_line(&message.text, RESULT_CHARS));
        }
        if !(own_card || own_session) {
            return;
        }
        match &event.kind {
            RunEventKind::Card(card) => self.card_event(card),
            RunEventKind::Session(session) => self.session_event(session),
            RunEventKind::Agent(agent) => self.agent_event(agent),
            RunEventKind::Gate(outcome) => {
                match self.gates.iter_mut().find(|gate| gate.name == outcome.name) {
                    Some(gate) => *gate = outcome.clone(),
                    None => self.gates.push(outcome.clone()),
                }
            }
            RunEventKind::Budget(BudgetEvent::Extended { amount, .. }) => {
                self.extended.push(*amount);
            }
            _ => {}
        }
    }

    fn card_event(&mut self, event: &CardEvent) {
        match event {
            CardEvent::Worktree { branch, .. } => self.branch = Some(branch.clone()),
            CardEvent::Committed(info) => {
                self.branch = Some(info.branch.clone());
                if let Some(sha) = &info.sha {
                    self.commit = Some(sha.clone());
                    self.committed.files_changed += info.stat.files_changed;
                    self.committed.insertions += info.stat.insertions;
                    self.committed.deletions += info.stat.deletions;
                }
            }
            CardEvent::Diff { stat, .. } => self.diff = Some(*stat),
            CardEvent::Reported(report) => self.last_words = Some((**report).clone()),
            CardEvent::State {
                to: CardState::NeedsDecision | CardState::Failed | CardState::Escalated,
                reason,
                ..
            } => self.state_reason = Some(one_line(reason, RESULT_CHARS)),
            _ => {}
        }
    }

    fn session_event(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Input(input) => {
                for part in &input.parts {
                    if matches!(part.kind, InputPartKind::Notes | InputPartKind::Decisions) {
                        let source = part.source.as_deref().unwrap_or(&part.label);
                        let source = one_line(source, PATH_CHARS);
                        if !self.note_sources.contains(&source) {
                            self.note_sources.push(source);
                        }
                    }
                }
            }
            SessionEvent::Started(_) => self.last_message = None,
            SessionEvent::FileDiff(diff) => self.edited(&diff.path, None),
            SessionEvent::Ended(end) => {
                self.ended = Some(stop_line(&end.reason));
                self.used.usd += end.spend.cost_usd;
                self.used.tokens = self.used.tokens.saturating_add(end.spend.billable_tokens());
                self.used.minutes += end.duration_ms as f64 / 60_000.0;
            }
            _ => {}
        }
    }

    fn agent_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolStarted {
                id, name, input, ..
            } => match Step::of(name, input) {
                Step::Command(command) => {
                    self.commands.insert(id.clone(), self.happened.len());
                    self.happened.push(format!("ran `{command}`"));
                }
                Step::Read(path) => {
                    if self.read_paths.insert(path.clone()) {
                        self.happened.push(format!("read {path}"));
                    }
                }
                Step::Edit(path) => self.edited(&path, None),
                Step::Other(what) => self.happened.push(format!("used {what}")),
            },
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                if let Some(index) = self.commands.remove(id) {
                    let verdict = if *ok { "ok" } else { "failed" };
                    let tail = output
                        .as_deref()
                        .and_then(|text| text.lines().rev().find(|line| !line.trim().is_empty()))
                        .map(|line| format!(", {}", one_line(line, RESULT_CHARS)))
                        .unwrap_or_default();
                    if let Some(line) = self.happened.get_mut(index) {
                        let _ = write!(line, ": {verdict}{tail}");
                    }
                }
            }
            AgentEvent::FileChanged { path, kind } => self.edited(path, Some(kind)),
            AgentEvent::Todo { items } => self.last_todo = items.clone(),
            AgentEvent::Message { text, parent: None } if !text.trim().is_empty() => {
                self.last_message = Some(one_line(text, REPORT_CHARS));
            }
            _ => {}
        }
    }

    /// One line per edited file, the first time it changes.
    fn edited(&mut self, path: &str, kind: Option<&str>) {
        let path = one_line(path, PATH_CHARS);
        if !self.edited_paths.insert(path.clone()) {
            return;
        }
        match kind {
            Some(kind) => self
                .happened
                .push(format!("edited {path} ({})", one_line(kind, 20))),
            None => self.happened.push(format!("edited {path}")),
        }
    }

    fn finish(self, limit: Option<BudgetAmount>) -> HandoffPacket {
        let mut notes: Vec<String> = self
            .note_sources
            .iter()
            .map(|source| {
                self.decisions
                    .get(source)
                    .cloned()
                    .unwrap_or_else(|| source.clone())
            })
            .collect();
        notes.extend(
            self.posted_decisions
                .iter()
                .map(|text| format!("decision: {text}")),
        );
        let branch = self.branch.clone().unwrap_or_else(|| match &self.run {
            Some(run) => format!("argonauts/{run}/{}", self.card.id),
            None => String::new(),
        });
        let why_stopped = self
            .ended
            .clone()
            .or_else(|| self.state_reason.clone())
            .unwrap_or_else(|| "it stopped without a recorded reason".to_owned());
        HandoffPacket {
            card: self.card.clone(),
            goal_sha256: self.goal.clone(),
            notes,
            work: WorkSoFar {
                branch,
                commit: self.commit.clone(),
                stat: self.diff.unwrap_or(self.committed),
            },
            happened: latest(self.happened.clone()),
            last_todo: self.last_todo.clone(),
            gates: self.gates.clone(),
            why_stopped,
            last_words: self.last_words.clone(),
            last_message: self.last_message.clone(),
            budget_left: self.budget_left(limit),
            transcript: None,
            summary: None,
        }
    }

    /// Quota can't be measured per card, so a quota budget has no amount left to show.
    fn budget_left(&self, limit: Option<BudgetAmount>) -> Option<BudgetAmount> {
        let limit = limit?;
        let spent = match limit.unit {
            BudgetUnit::Usd => self.used.usd,
            BudgetUnit::Tokens => self.used.tokens as f64,
            BudgetUnit::Minutes => self.used.minutes,
            BudgetUnit::Quota => return None,
        };
        let extra: f64 = self
            .extended
            .iter()
            .filter(|amount| amount.unit == limit.unit)
            .map(|amount| amount.amount)
            .sum();
        Some(BudgetAmount {
            unit: limit.unit,
            amount: (limit.amount + extra - spent).max(0.0),
        })
    }
}

/// The last [`MAX_HAPPENED`] lines, after one that counts the rest.
fn latest(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() <= MAX_HAPPENED {
        return lines;
    }
    let dropped = lines.len() - (MAX_HAPPENED - 1);
    let mut kept = vec![format!("({dropped} earlier steps left out)")];
    kept.extend(lines.drain(dropped..));
    kept
}

/// What a tool call did, read from vendor-neutral hints in its name and input.
pub(super) enum Step {
    Command(String),
    Read(String),
    Edit(String),
    Other(String),
}

impl Step {
    pub(super) fn of(name: &str, input: &Value) -> Self {
        let text = |key: &str| input.get(key).and_then(Value::as_str);
        let command = match input.get("command").or_else(|| input.get("cmd")) {
            Some(Value::String(command)) => Some(command.clone()),
            Some(Value::Array(parts)) => Some(
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        };
        if let Some(command) = command {
            return Self::Command(one_line(&command, COMMAND_CHARS));
        }
        let lower = name.to_ascii_lowercase();
        let name = one_line(name, 40);
        let path = ["file_path", "path", "filePath", "file", "notebook_path"]
            .into_iter()
            .find_map(text)
            .map(|path| one_line(path, PATH_CHARS));
        let names_any = |words: &[&str]| words.iter().any(|word| lower.contains(word));
        match path {
            Some(path) if names_any(&["edit", "write", "patch", "create", "replace"]) => {
                Self::Edit(path)
            }
            Some(path) if names_any(&["read", "view", "open", "cat"]) => Self::Read(path),
            Some(path) => Self::Other(format!("{name} on {path}")),
            None => match ["pattern", "query", "url"].into_iter().find_map(text) {
                Some(detail) => Self::Other(format!("{name}: {}", one_line(detail, RESULT_CHARS))),
                None => Self::Other(name),
            },
        }
    }
}

fn render(packet: &HandoffPacket) -> String {
    let card = &packet.card;
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line(format!(
        "Handoff · {} {}",
        card.id,
        one_line(&card.title, RESULT_CHARS)
    ));
    line(
        "Another agent started this card and stopped. Its work so far is committed on your \
         branch. Continue from there."
            .to_owned(),
    );
    line(String::new());
    line(format!("Task: {}", one_line(&card.task, REPORT_CHARS)));
    if !card.owned_paths.is_empty() {
        line(format!("Owned paths: {}", card.owned_paths.join(", ")));
    }
    if !card.read_only_paths.is_empty() {
        line(format!(
            "Read-only paths: {}",
            card.read_only_paths.join(", ")
        ));
    }
    if !card.criteria.is_empty() {
        line(format!("Goal criteria: {}", card.criteria.join(", ")));
    }
    if let Some(goal) = &packet.goal_sha256 {
        line(format!("Goal: sha256 {goal}"));
    }
    if !packet.notes.is_empty() {
        line("Notes and decisions:".to_owned());
        for note in &packet.notes {
            line(format!("  {note}"));
        }
    }
    let work = &packet.work;
    let stat = work.stat;
    let at = match &work.commit {
        Some(commit) => format!("commit {} on {}", short(commit), work.branch),
        None => format!("nothing committed yet on {}", work.branch),
    };
    line(format!(
        "Work so far: {at}: {} files, +{} -{}",
        stat.files_changed, stat.insertions, stat.deletions
    ));
    if !packet.happened.is_empty() {
        line("What happened:".to_owned());
        for step in &packet.happened {
            line(format!("  {step}"));
        }
    }
    if !packet.last_todo.is_empty() {
        line("Last to-do list:".to_owned());
        for item in &packet.last_todo {
            let mark = match item.status {
                TodoStatus::Completed => "[x]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Pending => "[ ]",
            };
            line(format!("  {mark} {}", one_line(&item.text, RESULT_CHARS)));
        }
    }
    if !packet.gates.is_empty() {
        line("Gates:".to_owned());
        for gate in &packet.gates {
            let verdict = if gate.passed { "passed" } else { "failed" };
            line(format!(
                "  {}: {verdict}, {}",
                gate.name,
                one_line(&gate.summary, RESULT_CHARS)
            ));
        }
    }
    line(format!("Why it stopped: {}", packet.why_stopped));
    if let Some(report) = &packet.last_words {
        line(format!(
            "Its last report ({:?}): {}",
            report.status,
            one_line(&report.summary, REPORT_CHARS)
        ));
        for concern in &report.concerns {
            line(format!("  concern: {}", one_line(concern, RESULT_CHARS)));
        }
    }
    if let Some(message) = &packet.last_message {
        line(format!("Its last message: {message}"));
    }
    if let Some(left) = packet.budget_left {
        line(format!("Budget left: {}", amount(left)));
    }
    out
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn amount(amount: BudgetAmount) -> String {
    match amount.unit {
        BudgetUnit::Usd => format!("${:.2}", amount.amount),
        BudgetUnit::Tokens => format!("{:.0} tokens", amount.amount),
        BudgetUnit::Minutes => format!("{:.0} minutes", amount.amount),
        BudgetUnit::Quota => format!("{:.0}% of quota", amount.amount * 100.0),
    }
}

#[cfg(test)]
mod tests;
