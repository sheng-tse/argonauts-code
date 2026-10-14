//! The resume picker: one list of earlier conversations, waiting or stopped runs, and the CLIs'
//! own sessions for this folder, newest first, with where each is from, when it was last used,
//! and how long it is. Enter carries one on, `b` branches from it, and `a` lists every folder's
//! sessions. Waiting runs never expire.
//!
//! Each run's agents' sessions fold under it, and → shows them: a row per session, with the
//! helpers its CLI started inside it counted on that row. A session carries on through its CLI's
//! own resume once its run ended, one whose CLI can't resume opens its transcript, and a helper
//! only comes back with its session. `/` narrows the list as you type, since its letters are
//! keys: x stops a run, d shows its diff, and r its raw log.

use std::collections::HashSet;

use argonauts_core::RunId;
use argonauts_core::agent_sessions::{AgentSession, AgentSessionRef, Resume};
use argonauts_core::api::{DiffScope, RunState as RunPhase, RunSummary};
use argonauts_core::app::{ConversationSummary, UiCommand, ViewData, ViewRequest};
use argonauts_core::cli_sessions::{CliSession, CliSessionRef};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::ConversationId;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Widget};
use time::OffsetDateTime;

use crate::format::{WallClock, agent_label};
use crate::kit::State;
use crate::state::AppState;
use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

use super::list::{self, ListState};
use super::panes::{self, Marks};
use super::{View, ViewCtx, ViewOutcome};

/// Cells kept clear after a column's text.
const GAP: usize = 2;

/// The cells before a row's columns: the selection mark and the needs-you mark, a blank after
/// each.
const MARGIN: usize = 4;

/// The columns' widths. The title takes what's left of the row, up to [`TITLE_MOST`]. Where a
/// row is from holds the longest of the names, a CLI's or the app's own.
const FROM: usize = 18;
const LAST_USED: usize = 14;
const LENGTH: usize = 10;
const TITLE_MOST: usize = 62;
const TITLE_LEAST: usize = 16;

/// The box's own rows: its two borders, the title and the blank under it, the heading, and the
/// blank and the keys under the rows.
const AROUND: usize = 7;

/// How much of a CLI session's id the list shows, unless two listed share that start.
const SHORT_ID: usize = 8;

/// A session its CLI wrote to this lately may still be open in it, so Enter asks first.
const MAYBE_OPEN: time::Duration = time::Duration::minutes(2);

#[derive(Debug)]
pub struct ResumePicker {
    pub runs: Vec<RunSummary>,
    pub conversations: Vec<ConversationSummary>,
    /// The main agent CLI's own sessions.
    pub sessions: Vec<CliSession>,
    /// What the Id column shows for each session, such as `codex:019a5770`.
    session_ids: Vec<String>,
    /// The agents' sessions of the runs read so far, each run's in the order they started.
    pub agents: Vec<AgentSession>,
    /// The runs whose agents `agents` holds. Another run's are asked for when you open it.
    read: HashSet<RunId>,
    /// The runs whose agents show under them.
    open: HashSet<RunId>,
    /// What you typed to narrow the list.
    filter: String,
    /// Typing goes to the filter, from `/` until Esc.
    filtering: bool,
    /// What shows, by when each was last used, newest first.
    rows: Vec<Row>,
    pub list: ListState,
    /// Shows times like `2h ago`; tests pin it.
    pub clock: WallClock,
    /// The run a first `x` asked to stop.
    confirm_stop: Option<RunId>,
    /// The session a first Enter warned may still be open in its CLI.
    confirm_open: Option<CliSessionRef>,
    /// The CLIs' sessions listed are every folder's.
    every_folder: bool,
    page: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Conversation(usize),
    Run(usize),
    Session(usize),
    /// Under its run.
    Agent(usize),
}

/// A row by what it lists, which a fresh list or a new filter keeps selected.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Picked {
    Conversation(ConversationId),
    Run(RunId),
    Session(CliSessionRef),
    Agent(AgentSessionRef),
}

impl ResumePicker {
    pub fn new(runs: Vec<RunSummary>) -> Self {
        Self::with_conversations(runs, Vec::new())
    }

    pub fn with_conversations(
        runs: Vec<RunSummary>,
        conversations: Vec<ConversationSummary>,
    ) -> Self {
        Self::listing(runs, conversations, Vec::new())
    }

    pub fn listing(
        runs: Vec<RunSummary>,
        conversations: Vec<ConversationSummary>,
        sessions: Vec<CliSession>,
    ) -> Self {
        let mut picker = Self {
            runs,
            conversations,
            session_ids: short_ids(&sessions),
            sessions,
            agents: Vec::new(),
            read: HashSet::new(),
            open: HashSet::new(),
            filter: String::new(),
            filtering: false,
            rows: Vec::new(),
            list: ListState::default(),
            clock: WallClock::local(),
            confirm_stop: None,
            confirm_open: None,
            every_folder: false,
            page: 1,
        };
        picker.rows = picker.merged();
        picker
    }

    /// The agents' sessions of the runs in `read`, each listed under its run once you open it.
    pub fn with_agents(mut self, agents: Vec<AgentSession>, read: Vec<RunId>) -> Self {
        self.agents = agents;
        self.read = read.into_iter().collect();
        self.rows = self.merged();
        self
    }

    /// Each list is newest first already, so a merge keeps that. On a tie a conversation comes
    /// first, then a run. A run's agents follow it while it's open, or while what you typed
    /// matches them, and a row that doesn't match what you typed is left out.
    fn merged(&self) -> Vec<Row> {
        let words = search_words(&self.filter);
        let mut rows =
            Vec::with_capacity(self.runs.len() + self.conversations.len() + self.sessions.len());
        let (mut conversation, mut run, mut session) = (0, 0, 0);
        loop {
            let heads = [
                self.conversations
                    .get(conversation)
                    .map(|c| (c.updated, Row::Conversation(conversation))),
                self.runs.get(run).map(|r| (r.updated, Row::Run(run))),
                self.sessions
                    .get(session)
                    .map(|s| (s.updated, Row::Session(session))),
            ];
            let Some((_, next)) = heads
                .into_iter()
                .flatten()
                .reduce(|best, head| if head.0 > best.0 { head } else { best })
            else {
                break;
            };
            match next {
                Row::Conversation(_) => conversation += 1,
                Row::Run(_) => run += 1,
                Row::Session(_) => session += 1,
                Row::Agent(_) => {}
            }
            match next {
                Row::Run(r) => rows.extend(self.run_rows(r, &words)),
                row if self.matches(row, &words) => rows.push(row),
                _ => {}
            }
        }
        rows
    }

    /// A run's row, then those of its agents that show.
    fn run_rows(&self, r: usize, words: &[String]) -> Vec<Row> {
        let run = &self.runs[r].run;
        let agents = self
            .agents
            .iter()
            .enumerate()
            .filter(|(_, agent)| &agent.run == run)
            .map(|(a, _)| Row::Agent(a));
        let shown: Vec<Row> = if words.is_empty() {
            agents.filter(|_| self.open.contains(run)).collect()
        } else {
            agents.filter(|row| self.matches(*row, words)).collect()
        };
        if shown.is_empty() && !self.matches(Row::Run(r), words) {
            return Vec::new();
        }
        std::iter::once(Row::Run(r)).chain(shown).collect()
    }

    /// Whether the row's words hold every word you typed.
    fn matches(&self, row: Row, words: &[String]) -> bool {
        if words.is_empty() {
            return true;
        }
        let text = self.search_text(row).to_lowercase();
        words.iter().all(|word| text.contains(word.as_str()))
    }

    /// What a row is found by: its id and title, what it ran on, and an agent's run too.
    fn search_text(&self, row: Row) -> String {
        match row {
            Row::Conversation(i) => {
                let conversation = &self.conversations[i];
                let worker = conversation
                    .worker
                    .as_ref()
                    .map_or_else(String::new, |worker| {
                        format!("{worker} {}", agent_label(&worker.agent))
                    });
                format!(
                    "{} {} {worker}",
                    conversation.conversation, conversation.title
                )
            }
            Row::Run(i) => {
                let run = &self.runs[i];
                let state = state_word(run.state);
                format!("{} {} {state} {}", run.run, run.title, run.stopped_at)
            }
            Row::Session(i) => {
                let session = &self.sessions[i];
                let branch = session.branch.as_deref().unwrap_or_default();
                let cli = agent_label(&session.agent_id.0);
                let id = &self.session_ids[i];
                format!("{id} {} {cli} {branch}", session.title)
            }
            Row::Agent(a) => {
                let agent = &self.agents[a];
                [
                    agent.run.to_string(),
                    agent.session.to_string(),
                    agent.role.as_str().to_owned(),
                    agent.label.clone().unwrap_or_default(),
                    agent.worker.to_string(),
                    agent_label(&agent.worker.agent),
                    agent.last_message.clone().unwrap_or_default(),
                    agent.helper_counts(),
                ]
                .join(" ")
            }
        }
    }

    fn picked(&self, row: Row) -> Picked {
        match row {
            Row::Conversation(i) => {
                Picked::Conversation(self.conversations[i].conversation.clone())
            }
            Row::Run(i) => Picked::Run(self.runs[i].run.clone()),
            Row::Session(i) => Picked::Session(self.sessions[i].reference()),
            Row::Agent(a) => Picked::Agent(self.agents[a].reference()),
        }
    }

    /// A row's kind and id, such as `agent s-001`, which tests tell rows apart by.
    #[cfg(test)]
    fn named(&self, row: Row) -> String {
        match row {
            Row::Conversation(i) => format!("conversation {}", self.conversations[i].conversation),
            Row::Run(i) => format!("run {}", self.runs[i].run),
            Row::Session(i) => format!("session {}", self.session_ids[i]),
            Row::Agent(a) => format!("agent {}", self.agents[a].session),
        }
    }

    /// Lists the rows again, keeping `keep` selected when it's still listed, else the first.
    fn relist(&mut self, keep: Option<Picked>) {
        self.rows = self.merged();
        let index = keep
            .and_then(|keep| self.rows.iter().position(|row| self.picked(*row) == keep))
            .unwrap_or(0);
        self.list.move_to(index, self.rows.len(), self.page);
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.list.selected).copied()
    }

    fn selected(&self) -> Option<&RunSummary> {
        match self.selected_row()? {
            Row::Run(i) => self.runs.get(i),
            _ => None,
        }
    }

    fn selected_conversation(&self) -> Option<&ConversationSummary> {
        match self.selected_row()? {
            Row::Conversation(i) => self.conversations.get(i),
            _ => None,
        }
    }

    fn selected_session(&self) -> Option<(&CliSession, &str)> {
        match self.selected_row()? {
            Row::Session(i) => Some((self.sessions.get(i)?, self.session_ids.get(i)?)),
            _ => None,
        }
    }

    fn selected_agent(&self) -> Option<&AgentSession> {
        match self.selected_row()? {
            Row::Agent(i) => self.agents.get(i),
            _ => None,
        }
    }

    /// More than runs, so the list is of conversations.
    fn mixed(&self) -> bool {
        !self.conversations.is_empty() || !self.sessions.is_empty() || !self.agents.is_empty()
    }

    /// Where a row is from: Argonauts Code's own conversations and runs, or a CLI by its name
    /// and color.
    fn from(&self, row: Row, names: &DisplayNames, theme: &Theme) -> (String, Style) {
        let cli = |id: &str| names.cli(&sanitize_line(id));
        match row {
            Row::Conversation(_) | Row::Run(_) => (
                "Argonauts Code".to_owned(),
                theme.style(Role::Text).add_modifier(Modifier::BOLD),
            ),
            Row::Session(i) => {
                let id = self.sessions[i].agent_id.0.as_str();
                (cli(id), theme.style(Role::for_cli(id)))
            }
            Row::Agent(a) => {
                let id = self.agents[a].worker.agent.as_str();
                (cli(id), theme.style(Role::for_cli(id)))
            }
        }
    }

    fn title(&self, row: Row) -> String {
        match row {
            Row::Run(i) => clean(&self.runs[i].title),
            Row::Conversation(i) => clean(&self.conversations[i].title),
            Row::Session(i) => {
                let session = &self.sessions[i];
                let title = clean(&session.title);
                // Among every folder's sessions, each says where it ran.
                match session.folder.file_name().filter(|_| self.every_folder) {
                    Some(folder) => format!("{title} · in {}", clean(&folder.to_string_lossy())),
                    None => title,
                }
            }
            // Indented under its run.
            Row::Agent(a) => format!("  {}", agent_title(&self.agents[a])),
        }
    }

    fn updated(&self, row: Row) -> OffsetDateTime {
        match row {
            Row::Run(i) => self.runs[i].updated,
            Row::Conversation(i) => self.conversations[i].updated,
            Row::Session(i) => self.sessions[i].updated,
            Row::Agent(a) => self.agents[a].updated,
        }
    }

    /// How long a conversation or a session is, or the state of a run or an agent's session.
    fn length(&self, row: Row, theme: &Theme) -> (String, Style) {
        let dim = theme.style(Role::Dim);
        match row {
            Row::Conversation(i) => {
                let conversation = &self.conversations[i];
                let length = match (conversation.turns, conversation.runs) {
                    (0, runs) => counted(runs, "run"),
                    (turns, _) => counted(turns, "turn"),
                };
                (length, dim)
            }
            Row::Session(i) => (counted(self.sessions[i].turns, "turn"), dim),
            Row::Run(i) => {
                let state = self.runs[i].state;
                (state_word(state).to_owned(), theme.style(state_role(state)))
            }
            Row::Agent(a) => (agent_state(&self.agents[a]).to_owned(), dim),
        }
    }

    /// The heading, then a row for each listed, `width` cells wide at most.
    fn lines(&self, width: usize, names: &DisplayNames, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let picked = theme.style(Role::Question);
        let title_width = width
            .saturating_sub(MARGIN + FROM + LAST_USED + LENGTH)
            .clamp(TITLE_LEAST, TITLE_MOST);
        let mut lines = vec![Line::from(vec![
            Span::raw(" ".repeat(MARGIN)),
            Span::styled(cell("from", FROM), dim),
            Span::styled(cell("first prompt or title", title_width), dim),
            Span::styled(cell("last used", LAST_USED), dim),
            Span::styled("length", dim),
        ])];
        for (i, row) in self.rows.iter().enumerate() {
            let selected = i == self.list.selected;
            let remind = matches!(*row, Row::Run(r) if self.runs[r].remind);
            let (from, from_style) = self.from(*row, names, theme);
            let (length, length_style) = self.length(*row, theme);
            let title_style = if selected {
                picked
            } else {
                theme.style(Role::Text)
            };
            lines.push(Line::from(vec![
                if selected {
                    Span::styled(format!("{} ", marks.selected), picked)
                } else {
                    Span::raw("  ")
                },
                if remind {
                    Span::styled(marks.needs_you, theme.style(Role::StateNeedsYou))
                } else {
                    Span::raw(" ")
                },
                Span::raw(" "),
                Span::styled(cell(&from, FROM), from_style),
                Span::styled(cell(&self.title(*row), title_width), title_style),
                Span::styled(
                    cell(&last_used(self.updated(*row), &self.clock), LAST_USED),
                    dim,
                ),
                // The last column, so a state longer than a length runs on.
                Span::styled(length, length_style),
            ]));
        }
        if self.rows.is_empty() {
            let text = if self.filter.trim().is_empty() {
                "    Nothing to resume here yet"
            } else {
                "    Nothing matches"
            };
            lines.push(Line::from(Span::styled(text, dim)));
        }
        lines
    }

    /// `Resume a conversation · in ~/work/shop`, then what you search for and which rows show
    /// when they don't all fit.
    fn title_line(&self, folder: &str, shown: usize, theme: &Theme) -> Line<'static> {
        let title = if self.mixed() || self.runs.is_empty() {
            "Resume a conversation"
        } else {
            "Resume a run"
        };
        let mut rest = match clean(folder) {
            _ if self.every_folder => " · in every folder".to_owned(),
            folder if folder.is_empty() => String::new(),
            folder => format!(" · in {folder}"),
        };
        if self.filtering || !self.filter.is_empty() {
            rest.push_str(&format!(" · search: {}", sanitize_line(&self.filter)));
        }
        if shown < self.rows.len() {
            let first = self.list.offset + 1;
            let last = self.list.offset + shown;
            rest.push_str(&format!(" · {first}-{last} of {}", self.rows.len()));
        }
        Line::from(vec![
            Span::styled(title, theme.style(Role::Text).add_modifier(Modifier::BOLD)),
            Span::styled(rest, theme.style(Role::Dim)),
        ])
    }

    /// The keys for the row selected, each with how long it stays as the row gets narrow.
    /// While you type a search, letters are the search's, so only the keys without them show.
    fn keys(&self) -> Vec<(String, u8)> {
        if let Some(run) = &self.confirm_stop {
            let again = format!("x again stops {}", sanitize_line(run.as_str()));
            return vec![(again, 9), ("esc keeps it".to_owned(), 9)];
        }
        if let Some(session) = &self.confirm_open {
            let cli = agent_label(&sanitize_line(&session.agent_id.0));
            return vec![
                (
                    format!("{cli} wrote to it just now, so it may still be open there"),
                    9,
                ),
                ("enter continues it here anyway".to_owned(), 8),
                ("b branches from it instead".to_owned(), 7),
                ("esc".to_owned(), 9),
            ];
        }
        let letters = !self.filtering;
        let mut keys: Vec<(&str, u8)> = vec![("↑↓ pick", 9)];
        match self.selected_row() {
            Some(Row::Agent(a)) => match &self.agents[a].resume {
                Resume::Ready => keys.push(("enter continues it", 8)),
                Resume::Cannot { .. } => keys.push(("enter shows its transcript", 8)),
                Resume::Working | Resume::RunOwns => {}
            },
            Some(Row::Run(r)) => {
                let run = &self.runs[r];
                let open = self.open.contains(&run.run);
                match (run.state == RunPhase::Merged, open) {
                    (true, true) => keys.push(("enter hides its agents", 8)),
                    (true, false) => keys.push(("enter shows its agents", 8)),
                    (false, true) => keys.extend([("enter resumes it", 8), ("← hides agents", 5)]),
                    (false, false) => keys.extend([("enter resumes it", 8), ("→ agents", 5)]),
                }
                if letters && !run.state.ended() {
                    keys.push(("x stops it", 4));
                }
                if letters {
                    keys.push(("d diff", 3));
                }
            }
            Some(Row::Conversation(_) | Row::Session(_)) => {
                keys.push(("enter continues it", 8));
                if letters {
                    keys.push(("b branches from it, the original untouched", 6));
                }
            }
            None => {}
        }
        let of_a_run = matches!(self.selected_row(), Some(Row::Run(_) | Row::Agent(_)));
        if letters && of_a_run {
            keys.push(("r raw", 2));
        }
        if letters {
            let folders = if self.every_folder {
                "a this folder"
            } else {
                "a every folder"
            };
            // On a run's row its own keys outlast the one that lists more sessions.
            let keep = if of_a_run { 1 } else { 5 };
            keys.extend([(folders, keep), ("/ search", 7), ("esc", 9)]);
        } else {
            keys.push(("esc clears the search", 9));
        }
        keys.into_iter()
            .map(|(key, keep)| (key.to_owned(), keep))
            .collect()
    }

    /// The keys on one row, the ones that matter least left out until the rest fit.
    fn key_line(&self, width: usize, theme: &Theme) -> Line<'static> {
        let mut keys = self.keys();
        let joined = |keys: &[(String, u8)]| {
            let words: Vec<&str> = keys.iter().map(|(key, _)| key.as_str()).collect();
            words.join(" · ")
        };
        while Span::raw(joined(&keys)).width() > width && keys.len() > 1 {
            let least = keys.iter().map(|(_, keep)| *keep).min().unwrap_or(0);
            let Some(at) = keys.iter().rposition(|(_, keep)| *keep == least) else {
                break;
            };
            keys.remove(at);
        }
        Line::from(Span::styled(joined(&keys), theme.style(Role::Dim)))
    }

    /// The list in its box, `width` cells wide: the title, the heading, `shown` rows from the
    /// first in view, and the keys.
    fn boxed(
        &self,
        width: usize,
        shown: usize,
        folder: &str,
        names: &DisplayNames,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let inner = width.saturating_sub(4);
        let faint = theme.style(Role::Faint);
        let table = self.lines(inner, names, theme);
        let mut body = vec![
            self.title_line(folder, shown, theme),
            Line::default(),
            table[0].clone(),
        ];
        body.extend(table.iter().skip(1 + self.list.offset).take(shown).cloned());
        body.push(Line::default());
        body.push(self.key_line(inner, theme));
        let rule = "─".repeat(width.saturating_sub(2));
        let mut lines = vec![Line::from(Span::styled(format!("╭{rule}╮"), faint))];
        for line in body {
            let line = clipped(line, inner);
            let fill = inner.saturating_sub(line.width());
            let mut spans = vec![Span::styled("│ ", faint)];
            spans.extend(line.spans);
            spans.push(Span::raw(" ".repeat(fill)));
            spans.push(Span::styled(" │", faint));
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(Span::styled(format!("╰{rule}╯"), faint)));
        lines
    }

    /// Enter: a conversation or a CLI session carries on, a run resumes, and a merged run, which
    /// can't, shows its agents. An agent's session carries on once its run ended, says why
    /// not while the run still owns it, and shows its transcript when its CLI can't resume it.
    fn enter(&mut self) -> ViewOutcome {
        let resume = |target| {
            ViewOutcome::Send(UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(target),
                },
            })
        };
        match self.selected_row() {
            Some(Row::Agent(a)) => Self::open_agent(&self.agents[a]),
            Some(Row::Conversation(i)) => resume(ResumeTarget::Conversation(
                self.conversations[i].conversation.clone(),
            )),
            Some(Row::Session(i)) => {
                let session = &self.sessions[i];
                let reference = session.reference();
                let lately = self.clock.now() - session.updated < MAYBE_OPEN;
                if lately && self.confirm_open.as_ref() != Some(&reference) {
                    self.confirm_open = Some(reference);
                    return ViewOutcome::Consumed;
                }
                self.confirm_open = None;
                resume(ResumeTarget::CliSession(reference))
            }
            Some(Row::Run(r)) if self.runs[r].state == RunPhase::Merged => {
                if self.open.contains(&self.runs[r].run) {
                    self.fold(r)
                } else {
                    self.unfold(r)
                }
            }
            // The run takes the screen, as a conversation carried on does, so its question isn't
            // left waiting behind the picker.
            Some(Row::Run(r)) => ViewOutcome::SendToConversation(UiCommand::Resume {
                run: Some(self.runs[r].run.clone()),
            }),
            None => ViewOutcome::Consumed,
        }
    }

    /// `b`: a new conversation that starts from the one picked, which stays as it was.
    fn branch(&mut self) -> ViewOutcome {
        self.confirm_open = None;
        let target = match self.selected_row() {
            Some(Row::Conversation(i)) => {
                ResumeTarget::BranchConversation(self.conversations[i].conversation.clone())
            }
            Some(Row::Session(i)) => ResumeTarget::BranchCliSession(self.sessions[i].reference()),
            Some(Row::Run(_) | Row::Agent(_)) => {
                return ViewOutcome::Notice(
                    "Only a conversation or a CLI's session branches. ⏎ carries this one on"
                        .to_owned(),
                );
            }
            None => return ViewOutcome::Consumed,
        };
        ViewOutcome::Send(UiCommand::Slash {
            command: SlashCommand::Resume {
                target: Some(target),
            },
        })
    }

    /// `a`: the list again with every folder's sessions, or with this folder's alone.
    fn other_folders(&mut self) -> ViewOutcome {
        self.every_folder = !self.every_folder;
        ViewOutcome::Request(if self.every_folder {
            ViewRequest::SessionsEverywhere
        } else {
            ViewRequest::Runs
        })
    }

    fn open_agent(agent: &AgentSession) -> ViewOutcome {
        let reference = clean(&agent.reference().to_string());
        match &agent.resume {
            Resume::Ready | Resume::Cannot { .. } => ViewOutcome::Send(UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(ResumeTarget::Agent(agent.reference())),
                },
            }),
            Resume::Working => ViewOutcome::Notice(format!(
                "{reference} is still working. t in its agent view takes it over"
            )),
            Resume::RunOwns => ViewOutcome::Notice(run_owns(&agent.run, &reference)),
        }
    }

    /// Shows the run's agents under it, asking the app for them the first time when the list
    /// came without them.
    fn unfold(&mut self, r: usize) -> ViewOutcome {
        let run = self.runs[r].run.clone();
        self.open.insert(run.clone());
        self.relist(Some(Picked::Run(run.clone())));
        if !self.read.contains(&run) {
            return ViewOutcome::Request(ViewRequest::AgentSessions { run });
        }
        if self.agents.iter().any(|agent| agent.run == run) {
            ViewOutcome::Consumed
        } else {
            ViewOutcome::Notice(format!(
                "{} has no agent sessions",
                sanitize_line(run.as_str())
            ))
        }
    }

    /// Folds the run's agents back under it, with the run selected.
    fn fold(&mut self, r: usize) -> ViewOutcome {
        let run = self.runs[r].run.clone();
        self.open.remove(&run);
        self.relist(Some(Picked::Run(run)));
        ViewOutcome::Consumed
    }

    /// The run of the row selected, or the run an agent row sits under.
    fn run_at_selection(&self) -> Option<usize> {
        match self.selected_row()? {
            Row::Run(r) => Some(r),
            Row::Agent(a) => {
                let run = &self.agents[a].run;
                self.runs.iter().position(|summary| &summary.run == run)
            }
            Row::Conversation(_) | Row::Session(_) => None,
        }
    }

    fn search(&mut self, change: impl FnOnce(&mut String)) -> ViewOutcome {
        change(&mut self.filter);
        self.relist(None);
        ViewOutcome::Consumed
    }

    /// A key while you type a search: letters, Backspace, and Esc are the search's. `None`
    /// for the keys that work the list as ever, such as Enter and the arrows.
    fn search_key(&mut self, key: &KeyEvent) -> Option<ViewOutcome> {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                self.filtering = false;
                Some(self.search(String::clear))
            }
            KeyCode::Backspace => Some(self.search(|filter| {
                filter.pop();
            })),
            KeyCode::Char(c) if plain => Some(self.search(|filter| filter.push(c))),
            _ => None,
        }
    }

    fn stop(&mut self, state: &AppState) -> ViewOutcome {
        let Some(summary) = self.selected() else {
            return self.not_a_run();
        };
        let run = summary.run.clone();
        if summary.state.ended() {
            return ViewOutcome::Notice(format!(
                "{} has ended, so it has nothing to stop",
                sanitize_line(run.as_str())
            ));
        }
        if self.confirm_stop.as_ref() != Some(&run) {
            self.confirm_stop = Some(run);
            return ViewOutcome::Consumed;
        }
        self.confirm_stop = None;
        // `Stop` acts on the run on screen, so it goes out only when that's the one picked.
        if state.attached.as_ref() == Some(&run) {
            ViewOutcome::Send(UiCommand::Stop)
        } else {
            ViewOutcome::Notice(format!(
                "Only the run on screen can be stopped. Resume {} first, then stop it",
                sanitize_line(run.as_str())
            ))
        }
    }

    /// `d` and `r` on a run; `r` on an agent shows its own session's raw log.
    fn run_view(&self, state: &AppState, diff: bool) -> ViewOutcome {
        if let Some(agent) = self.selected_agent().filter(|_| !diff) {
            return ViewOutcome::Request(ViewRequest::Raw {
                run: agent.run.clone(),
                session: agent.session.clone(),
            });
        }
        let Some(run) = self.selected().map(|run| run.run.clone()) else {
            return self.not_a_run();
        };
        let request = if diff {
            diff_request(state, &run)
        } else {
            raw_request(state, &run)
        };
        match request {
            Ok(request) => ViewOutcome::Request(request),
            Err(why) => ViewOutcome::Notice(why),
        }
    }

    /// The run keys on a conversation, a CLI session, or an agent's session.
    fn not_a_run(&self) -> ViewOutcome {
        if let Some(agent) = self.selected_agent() {
            return ViewOutcome::Notice(format!(
                "{} is an agent's session. ⏎ carries it on",
                clean(&agent.reference().to_string())
            ));
        }
        if let Some(conversation) = self.selected_conversation() {
            return ViewOutcome::Notice(format!(
                "{} is a conversation. ⏎ carries it on",
                sanitize_line(conversation.conversation.as_str())
            ));
        }
        match self.selected_session() {
            Some((session, id)) => ViewOutcome::Notice(format!(
                "{} is a {} session. ⏎ carries it on",
                sanitize_line(id),
                agent_label(&sanitize_line(&session.agent_id.0))
            )),
            None => ViewOutcome::Consumed,
        }
    }
}

/// The words you typed, each to be found in a row, case aside.
fn search_words(filter: &str) -> Vec<String> {
    filter
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Why a finished agent's session waits for its run, said as the app says it.
fn run_owns(run: &RunId, session: &str) -> String {
    format!(
        "{} hasn't ended, so it still owns {session}. t in its agent view takes it over, or it \
         carries on once the run ends",
        sanitize_line(run.as_str())
    )
}

/// `codex:019a5770`: each id cut to its first [`SHORT_ID`] characters, or more where two
/// listed ids start the same, so each still names one session for `/resume`.
fn short_ids(sessions: &[CliSession]) -> Vec<String> {
    sessions
        .iter()
        .map(|session| {
            let id = session.id.as_str();
            let shared = sessions
                .iter()
                .filter(|other| other.agent_id == session.agent_id && other.id != session.id)
                .map(|other| common_prefix(id, &other.id))
                .max()
                .unwrap_or(0);
            let cut = id
                .char_indices()
                .map(|(at, _)| at)
                .chain([id.len()])
                .find(|&at| at >= SHORT_ID.max(shared + 1))
                .unwrap_or(id.len());
            format!("{}:{}", session.agent_id, &id[..cut])
        })
        .collect()
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((at, _), _)| at)
}

/// `worker · build:money · 3 subagents · Fixed both functions.`, by the agent's name in the run
/// (its task, for a run from before names). The helpers its CLI started come back with the
/// session, so they're counted here rather than listed.
fn agent_title(agent: &AgentSession) -> String {
    let role = agent.role.as_str();
    let parts = [
        Some(role.to_owned()),
        // A step named like its role, as the run review is, says it once.
        agent
            .label
            .as_deref()
            .map(clean)
            .filter(|label| label != role),
        helpers_short(agent),
        agent.last_message.as_deref().map(said_before_a_block),
    ];
    join_parts(parts)
}

/// What an agent said before any fenced block, such as the report a build session ends with,
/// which reads as noise on one line.
fn said_before_a_block(text: &str) -> String {
    clean(text.split("```").next().unwrap_or(text))
}

/// `2 subagents`, or `3 helpers` when they're of more than one kind.
fn helpers_short(agent: &AgentSession) -> Option<String> {
    let first = agent.helpers.first()?;
    let count = agent.helpers.len();
    if agent.helpers.iter().all(|helper| helper.kind == first.kind) {
        Some(agent.helper_counts())
    } else {
        Some(panes::count(count, "helper"))
    }
}

fn join_parts<const N: usize>(parts: [Option<String>; N]) -> String {
    parts
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Whether it carries on: nothing when it does, and Enter on one that doesn't says why.
fn agent_state(agent: &AgentSession) -> &'static str {
    match &agent.resume {
        Resume::Ready => "",
        Resume::Working => "working",
        Resume::RunOwns => "run owns it",
        Resume::Cannot { .. } => "can't resume",
    }
}

/// The request for a run's diff: its newest task with changes, as far as this screen knows.
pub(crate) fn diff_request(state: &AppState, run: &RunId) -> Result<ViewRequest, String> {
    let known = state.runs.get(run);
    let task = known.and_then(|known| {
        known
            .tasks
            .values()
            .rev()
            .find(|task| task.diff.is_some_and(|diff| diff.files_changed > 0))
            .or_else(|| known.tasks.values().next_back())
            .map(|task| task.id.clone())
    });
    match task {
        Some(task) => Ok(ViewRequest::Diff {
            run: run.clone(),
            task,
            scope: DiffScope::Task,
        }),
        None => Err(format!(
            "No tasks of {} on this screen yet. Resume it to see its diff",
            sanitize_line(run.as_str())
        )),
    }
}

/// The request for a run's raw log: its newest session's agent lines.
pub(crate) fn raw_request(state: &AppState, run: &RunId) -> Result<ViewRequest, String> {
    let session = state.runs.get(run).and_then(|known| {
        known
            .sessions_in_order()
            .max_by_key(|session| session.started)
            .map(|session| session.key.clone())
    });
    match session {
        Some(session) => Ok(ViewRequest::Raw {
            run: run.clone(),
            session,
        }),
        None => Err(format!(
            "No sessions of {} on this screen yet. Resume it to see its raw log",
            sanitize_line(run.as_str())
        )),
    }
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

pub(crate) fn state_word(state: RunPhase) -> &'static str {
    match state {
        RunPhase::Running => "running",
        RunPhase::Pausing => "pausing",
        RunPhase::Waiting => "waiting",
        RunPhase::Crashed => "stopped",
        RunPhase::NeverStarted => "never started",
        RunPhase::FromAnotherBuild => "from an older Argonauts",
        RunPhase::Merged => "merged",
        RunPhase::PullRequest => "pull request",
        RunPhase::BranchLeft => "branch left",
        RunPhase::Stopped => "stopped",
        RunPhase::Failed => "failed",
        RunPhase::Abandoned => "abandoned",
    }
}

/// Which of the states every view draws a run's is. A run that waits in this list waits for
/// you to pick it up.
fn state_of(state: RunPhase) -> State {
    match state {
        RunPhase::Running | RunPhase::Pausing => State::Working,
        RunPhase::Waiting => State::NeedsYou,
        RunPhase::Failed => State::Failed,
        RunPhase::Merged | RunPhase::PullRequest => State::Done,
        RunPhase::BranchLeft
        | RunPhase::Stopped
        | RunPhase::Abandoned
        | RunPhase::Crashed
        | RunPhase::NeverStarted
        | RunPhase::FromAnotherBuild => State::Stopped,
    }
}

pub(crate) fn state_role(state: RunPhase) -> Role {
    state_of(state).role()
}

/// `text` cut to leave [`GAP`] clear cells in a slot of `width`, and padded to fill it.
fn cell(text: &str, width: usize) -> String {
    let room = width.saturating_sub(GAP);
    let mut cut = String::new();
    for ch in text.chars() {
        let wider = format!("{cut}{ch}");
        if Span::raw(wider.as_str()).width() > room {
            break;
        }
        cut = wider;
    }
    let fill = width.saturating_sub(Span::raw(cut.as_str()).width());
    format!("{cut}{}", " ".repeat(fill))
}

/// `line` cut to `width` cells, so the box's right side stays in place.
fn clipped(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    let mut room = width;
    let mut spans = Vec::new();
    for span in line.spans {
        let mut kept = String::new();
        for ch in span.content.chars() {
            let wide = Span::raw(ch.to_string()).width();
            if wide > room {
                room = 0;
                break;
            }
            room -= wide;
            kept.push(ch);
        }
        spans.push(Span::styled(kept, span.style));
    }
    Line::from(spans)
}

/// `42 turns`, `1 turn`, `1.2k turns`, and nothing for none.
fn counted(count: u32, noun: &str) -> String {
    match count {
        0 => String::new(),
        1 => format!("1 {noun}"),
        2..1_000 => format!("{count} {noun}s"),
        _ => {
            let tenths = count / 100;
            let (whole, tenth) = (tenths / 10, tenths % 10);
            if tenth == 0 {
                format!("{whole}k {noun}s")
            } else {
                format!("{whole}.{tenth}k {noun}s")
            }
        }
    }
}

/// `just now`, `12m ago`, `3h ago`, `yesterday`, then the day, such as `Oct 8`, with its year
/// when that isn't this one.
fn last_used(at: OffsetDateTime, clock: &WallClock) -> String {
    let now = clock.now();
    if now - at < time::Duration::DAY {
        return ago(at, now);
    }
    let day = clock.to_local(at).date();
    let today = clock.to_local(now).date();
    if today.previous_day() == Some(day) {
        return "yesterday".to_owned();
    }
    let month = &day.month().to_string()[..3];
    if day.year() == today.year() {
        format!("{month} {}", day.day())
    } else {
        format!("{month} {}, {}", day.day(), day.year())
    }
}

/// `just now`, `12m ago`, `3h ago`, `4d ago`.
pub(crate) fn ago(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let secs = (now - at).whole_seconds().max(0);
    match secs {
        0..60 => "just now".to_owned(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

impl View for ResumePicker {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        // The heading stays; the rows scroll under it.
        self.page = usize::from(area.height).saturating_sub(AROUND).max(1);
        self.list.keep_visible(self.rows.len(), self.page);
        // An empty list still has its one row, which says so.
        let shown = self.rows.len().clamp(1, self.page);
        let lines = self.boxed(
            usize::from(area.width),
            shown,
            ctx.state
                .welcome
                .as_ref()
                .map_or("", |welcome| &welcome.repo),
            &ctx.state.names,
            ctx.theme,
        );
        Clear.render(area, buf);
        panes::put_lines(buf, area, &lines);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        // A second `x` stops the run. Any other key keeps it, and Esc only does that.
        let stopping = !self.filtering && plain && key.code == KeyCode::Char('x');
        if !stopping && self.confirm_stop.take().is_some() && key.code == KeyCode::Esc {
            return ViewOutcome::Consumed;
        }
        // Enter or `b` answers the warning that a session may be open elsewhere. Any other key
        // drops it, and Esc only does that.
        let answering = key.code == KeyCode::Enter || (plain && key.code == KeyCode::Char('b'));
        if !answering && self.confirm_open.take().is_some() && key.code == KeyCode::Esc {
            return ViewOutcome::Consumed;
        }
        if self.filtering {
            if let Some(outcome) = self.search_key(&key) {
                return outcome;
            }
        } else if plain && key.code == KeyCode::Char('/') {
            self.filtering = true;
            return ViewOutcome::Consumed;
        }
        let key = if self.filtering {
            key
        } else {
            list::standard(key, list::vim(ctx.state), KeyCode::Esc)
        };
        // Outside a search, letters are keys.
        let letters = !self.filtering && plain;
        match key.code {
            KeyCode::Esc => return ViewOutcome::Close,
            KeyCode::Char('q') if letters => return ViewOutcome::Close,
            KeyCode::Char('x') if letters => return self.stop(ctx.state),
            KeyCode::Char('b') if letters => return self.branch(),
            KeyCode::Char('a') if letters => return self.other_folders(),
            KeyCode::Char(c @ ('d' | 'r')) if letters => {
                return self.run_view(ctx.state, c == 'd');
            }
            KeyCode::Enter => return self.enter(),
            KeyCode::Right => {
                return match self.selected_row() {
                    Some(Row::Run(r)) => self.unfold(r),
                    _ => ViewOutcome::Consumed,
                };
            }
            KeyCode::Left => {
                let open = self
                    .run_at_selection()
                    .filter(|&r| self.open.contains(&self.runs[r].run));
                return match open {
                    Some(r) => self.fold(r),
                    None => ViewOutcome::Ignored,
                };
            }
            _ => {}
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.list.apply(nav, self.rows.len(), self.page);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }

    /// A paste searches, as typing after `/` does.
    fn handle_paste(&mut self, text: &str, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        self.filtering = true;
        let pasted = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.search(|filter| filter.push_str(&pasted))
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        let keep = self.selected_row().map(|row| self.picked(row));
        match data {
            // A fresh list replaces this one in place, keeping the selection on the same row and
            // the agents of the older runs you opened since.
            ViewData::Runs {
                runs,
                conversations,
                sessions,
                agents,
                agents_read,
            } => {
                let kept: Vec<AgentSession> = self
                    .agents
                    .drain(..)
                    .filter(|agent| !agents_read.contains(&agent.run))
                    .collect();
                self.read.extend(agents_read.iter().cloned());
                self.runs = runs.clone();
                self.conversations = conversations.clone();
                self.sessions = sessions.clone();
                self.agents = agents.clone();
                self.agents.extend(kept);
                self.session_ids = short_ids(&self.sessions);
            }
            // The agents of a run you opened, read when you asked.
            ViewData::AgentSessions { run, agents } => {
                self.agents.retain(|agent| &agent.run != run);
                self.agents.extend(agents.iter().cloned());
                self.read.insert(run.clone());
            }
            _ => return false,
        }
        self.relist(keep);
        true
    }
}

#[cfg(test)]
#[path = "resume_agents_tests.rs"]
mod agents_tests;

#[cfg(test)]
#[path = "resume_picture_tests.rs"]
mod picture_tests;

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::api::{RunState as RunPhase, RunSummary};
    use argonauts_core::app::{ConversationSummary, UiCommand};
    use argonauts_core::cli_sessions::{CliSession, CliSessionRef};
    use argonauts_core::events::run::RunMode;
    use argonauts_core::input::{ResumeTarget, SlashCommand};
    use argonauts_core::names::{AgentId, ConversationId};
    use argonauts_core::{RunId, Spend};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use time::macros::datetime;
    use time::{Duration, OffsetDateTime};

    use super::ResumePicker;
    use crate::state::AppState;
    use crate::theme::Theme;
    use crate::views::{View, ViewCtx, ViewOutcome};

    const WALL: OffsetDateTime = datetime!(2026-09-30 12:00 UTC);

    fn run(id: &str, hours_ago: i64) -> RunSummary {
        RunSummary {
            run: RunId::new(id).unwrap(),
            title: format!("{id} title"),
            mode: RunMode::Crew,
            workflow: "council".into(),
            state: RunPhase::Waiting,
            stopped_at: "merge approval".into(),
            updated: WALL - Duration::hours(hours_ago),
            spent: Spend::default(),
            sessions_to_resume: 1,
            waiting: Vec::new(),
            live: false,
            remind: false,
            name: None,
        }
    }

    fn conversation(n: u32, title: &str, hours_ago: i64) -> ConversationSummary {
        ConversationSummary {
            conversation: ConversationId::numbered(n),
            title: title.to_owned(),
            updated: WALL - Duration::hours(hours_ago),
            worker: Some("claude-code".parse().unwrap()),
            turns: 2,
            runs: 0,
        }
    }

    fn picker() -> ResumePicker {
        ResumePicker::with_conversations(
            vec![run("run-02", 1), run("run-01", 5)],
            vec![
                conversation(2, "hi", 0),
                conversation(1, "what does export do?", 3),
            ],
        )
    }

    fn press(view: &mut ResumePicker, code: KeyCode) -> ViewOutcome {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
    }

    #[test]
    fn conversations_and_runs_list_together_newest_first() {
        let mut view = picker();
        view.clock = crate::format::WallClock::pinned(WALL);
        let order: Vec<String> = view.rows.iter().map(|row| view.named(*row)).collect();
        assert_eq!(
            order,
            [
                "conversation conversation-002",
                "run run-02",
                "conversation conversation-001",
                "run run-01"
            ]
        );
        let text = rows(&view, 100);
        assert!(
            text[0].contains("from") && text[0].contains("last used"),
            "{text:?}"
        );
        assert!(
            text[1].contains("Argonauts Code  ") && text[1].contains("2 turns"),
            "{text:?}"
        );
        assert!(text[2].contains("waiting"), "{text:?}");
    }

    #[test]
    fn a_conversation_with_only_runs_counts_its_runs() {
        let only_runs = ConversationSummary {
            turns: 0,
            runs: 1,
            worker: None,
            ..conversation(3, "fix the median", 1)
        };
        let two = ConversationSummary {
            runs: 2,
            ..only_runs.clone()
        };
        let chatted = ConversationSummary {
            runs: 2,
            ..conversation(2, "hi", 1)
        };
        let view = ResumePicker::with_conversations(Vec::new(), vec![only_runs, two, chatted]);
        let theme = Theme::dark();
        let lengths: Vec<String> = (0..3)
            .map(|i| view.length(super::Row::Conversation(i), &theme).0)
            .collect();
        assert_eq!(lengths, ["1 run", "2 runs", "2 turns"]);
    }

    #[test]
    fn enter_carries_on_a_conversation_or_resumes_a_run() {
        let mut view = picker();
        let sent = |outcome: ViewOutcome| match outcome {
            ViewOutcome::Send(command) => Some(command),
            _ => None,
        };
        assert_eq!(
            sent(press(&mut view, KeyCode::Enter)),
            Some(UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(ResumeTarget::Conversation(ConversationId::numbered(2))),
                },
            })
        );
        assert!(matches!(
            press(&mut view, KeyCode::Char('x')),
            ViewOutcome::Notice(text) if text.contains("conversation-002 is a conversation")
        ));
        press(&mut view, KeyCode::Down);
        // A run closes the picker on the way, so it and its question take the screen.
        assert!(matches!(
            press(&mut view, KeyCode::Enter),
            ViewOutcome::SendToConversation(UiCommand::Resume { run: Some(run) }) if run.as_str() == "run-02"
        ));
    }

    #[test]
    fn a_conversation_offers_no_run_keys() {
        let mut view = picker();
        let keys = |view: &ResumePicker| view.key_line(200, &Theme::dark()).to_string();
        assert_eq!(
            keys(&view),
            "↑↓ pick · enter continues it · b branches from it, the original untouched · a every \
             folder · / search · esc"
        );
        press(&mut view, KeyCode::Down);
        assert!(keys(&view).contains("x stops it · d diff · r raw"));
    }

    fn session(agent_id: &str, id: &str, title: &str, hours_ago: i64) -> CliSession {
        CliSession {
            agent_id: AgentId(agent_id.into()),
            id: id.into(),
            title: title.into(),
            prompt: None,
            folder: "/work/shop".into(),
            updated: WALL - Duration::hours(hours_ago),
            model: None,
            branch: Some("main".into()),
            last_message: None,
            turns: 3,
        }
    }

    fn with_sessions() -> ResumePicker {
        let mut view = ResumePicker::listing(
            vec![run("run-02", 1)],
            vec![conversation(2, "hi", 3)],
            vec![
                session("claude-code", "0a1b2c3d-1111-4aaa", "Input loop notes", 0),
                session("claude-code", "0a1b2c3d-2222-4aaa", "Fix the export", 2),
                session("claude-code", "9f8e7d6c-3333-4aaa", "Rename the writer", 4),
            ],
        );
        view.clock = crate::format::WallClock::pinned(WALL);
        view
    }

    fn rows(view: &ResumePicker, width: usize) -> Vec<String> {
        view.lines(width, &Default::default(), &Theme::dark())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    /// A run says its state where a conversation says its length, and says it once.
    #[test]
    fn a_finished_run_says_its_state_once() {
        let mut merged = run("run-02", 1);
        merged.state = RunPhase::Merged;
        merged.stopped_at = "merged".into();
        let view = ResumePicker::with_conversations(
            vec![merged, run("run-01", 5)],
            vec![conversation(1, "hi", 0)],
        );
        let text = rows(&view, 120);
        let row = |id: &str| {
            let at = view
                .rows
                .iter()
                .position(|row| view.named(*row).ends_with(id));
            text[1 + at.unwrap()].clone()
        };
        assert_eq!(row("run-02").matches("merged").count(), 1, "{text:#?}");
        assert!(row("run-01").contains("waiting"), "{text:#?}");
    }

    #[test]
    fn cli_sessions_list_with_the_rest() {
        let view = with_sessions();
        let order: Vec<String> = view.rows.iter().map(|row| view.named(*row)).collect();
        // Two ids that start alike show until they are different; another shows its first part.
        assert_eq!(
            order,
            [
                "session claude-code:0a1b2c3d-1",
                "run run-02",
                "session claude-code:0a1b2c3d-2",
                "conversation conversation-002",
                "session claude-code:9f8e7d6c"
            ]
        );
        let text = rows(&view, 118);
        let first = &text[1];
        for cell in ["Claude Code", "Input loop notes", "just now"] {
            assert!(first.contains(cell), "{cell}: {first}");
        }
    }

    #[test]
    fn enter_carries_a_cli_session_on() {
        let mut view = with_sessions();
        // Its CLI wrote to it just now, so the first Enter asks.
        assert!(matches!(
            press(&mut view, KeyCode::Enter),
            ViewOutcome::Consumed
        ));
        let ViewOutcome::Send(sent) = press(&mut view, KeyCode::Enter) else {
            panic!("Enter sends nothing");
        };
        assert_eq!(
            sent,
            UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(ResumeTarget::CliSession(CliSessionRef {
                        agent_id: AgentId("claude-code".into()),
                        id: "0a1b2c3d-1111-4aaa".into(),
                    })),
                },
            }
        );
        assert!(matches!(
            press(&mut view, KeyCode::Char('d')),
            ViewOutcome::Notice(text) if text == "claude-code:0a1b2c3d-1 is a Claude Code session. ⏎ carries it on"
        ));
    }

    #[test]
    fn the_title_says_where_and_which_rows_show() {
        let view = with_sessions();
        let theme = Theme::dark();
        assert_eq!(
            view.title_line("~/work/shop", 5, &theme).to_string(),
            "Resume a conversation · in ~/work/shop"
        );
        assert_eq!(
            view.title_line("~/work/shop", 2, &theme).to_string(),
            "Resume a conversation · in ~/work/shop · 1-2 of 5"
        );
    }

    #[test]
    fn a_fresh_list_keeps_the_session_selected() {
        let mut view = with_sessions();
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Down);
        let data = argonauts_core::app::ViewData::Runs {
            runs: Vec::new(),
            conversations: Vec::new(),
            sessions: view.sessions.clone(),
            agents: Vec::new(),
            agents_read: Vec::new(),
        };
        assert!(view.accept(&data));
        let (picked, _) = view.selected_session().unwrap();
        assert_eq!(picked.id, "0a1b2c3d-2222-4aaa");
    }

    #[test]
    fn runs_alone_are_titled_as_runs() {
        let view = ResumePicker::new(vec![run("run-01", 1)]);
        let title = view.title_line("~/work", 1, &Theme::dark()).to_string();
        assert_eq!(title, "Resume a run · in ~/work");
    }
}
