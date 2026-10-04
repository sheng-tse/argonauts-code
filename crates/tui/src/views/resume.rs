//! The resume picker: earlier conversations with the lead, parked or stopped runs, and the main
//! agent CLI's own sessions for this repo, newest first, with where each stopped, when, what it
//! spent, and what resuming restarts. Parked runs never expire.
//!
//! Each run's agents' sessions fold under it, and → shows them: a row per session, with the
//! helpers its CLI started inside it counted on that row. A session carries on through its CLI's
//! own resume once its run ended, one whose CLI can't resume opens its transcript, and a helper
//! only comes back with its session. `/` narrows the list as you type, since its letters are
//! keys: x stops a run, d shows its diff, and r its raw log.

use std::collections::HashSet;

use argonauts_core::agent_sessions::{AgentSession, AgentSessionRef, Resume};
use argonauts_core::api::{DiffScope, RunState as RunPhase, RunSummary};
use argonauts_core::app::{ChatSummary, UiCommand, ViewData, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::ChatId;
use argonauts_core::vendor_sessions::{VendorSession, VendorSessionRef};
use argonauts_core::{RunId, Spend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::format::{WallClock, vendor_label};
use crate::state::AppState;
use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

use super::list::{self, Column, ListState, fit_columns};
use super::panes::{self, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Cells between columns.
const GAP: usize = 2;

/// How much of a CLI session's id the list shows, unless two listed share that start.
const SHORT_ID: usize = 8;

/// The least of a title the list keeps while agents' sessions show, enough for a session's
/// role, card, and helpers, before the column of who ran it gives way.
const AGENT_TITLE_MIN: usize = 30;

#[derive(Debug)]
pub struct ResumePicker {
    pub runs: Vec<RunSummary>,
    pub chats: Vec<ChatSummary>,
    /// The main agent CLI's own sessions.
    pub sessions: Vec<VendorSession>,
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
    page: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Chat(usize),
    Run(usize),
    Session(usize),
    /// Under its run.
    Agent(usize),
}

/// A row by what it lists, which a fresh list or a new filter keeps selected.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Picked {
    Chat(ChatId),
    Run(RunId),
    Session(VendorSessionRef),
    Agent(AgentSessionRef),
}

impl ResumePicker {
    pub fn new(runs: Vec<RunSummary>) -> Self {
        Self::with_chats(runs, Vec::new())
    }

    pub fn with_chats(runs: Vec<RunSummary>, chats: Vec<ChatSummary>) -> Self {
        Self::listing(runs, chats, Vec::new())
    }

    pub fn listing(
        runs: Vec<RunSummary>,
        chats: Vec<ChatSummary>,
        sessions: Vec<VendorSession>,
    ) -> Self {
        let mut picker = Self {
            runs,
            chats,
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
        let mut rows = Vec::with_capacity(self.runs.len() + self.chats.len() + self.sessions.len());
        let (mut chat, mut run, mut session) = (0, 0, 0);
        loop {
            let heads = [
                self.chats.get(chat).map(|c| (c.updated, Row::Chat(chat))),
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
                Row::Chat(_) => chat += 1,
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
            Row::Chat(i) => {
                let chat = &self.chats[i];
                let worker = chat.worker.as_ref().map_or_else(String::new, |worker| {
                    format!("{worker} {}", vendor_label(&worker.worker))
                });
                format!("{} {} {worker}", chat.chat, chat.title)
            }
            Row::Run(i) => {
                let run = &self.runs[i];
                let state = state_word(run.state);
                format!("{} {} {state} {}", run.run, run.title, run.stopped_at)
            }
            Row::Session(i) => {
                let session = &self.sessions[i];
                let branch = session.branch.as_deref().unwrap_or_default();
                let vendor = vendor_label(&session.vendor.0);
                let id = &self.session_ids[i];
                format!("{id} {} {vendor} {branch}", session.title)
            }
            Row::Agent(a) => {
                let agent = &self.agents[a];
                [
                    agent.run.to_string(),
                    agent.session.to_string(),
                    agent.role.as_str().to_owned(),
                    agent.label.clone().unwrap_or_default(),
                    agent.worker.to_string(),
                    vendor_label(&agent.worker.worker),
                    agent.last_message.clone().unwrap_or_default(),
                    agent.helper_counts(),
                ]
                .join(" ")
            }
        }
    }

    fn picked(&self, row: Row) -> Picked {
        match row {
            Row::Chat(i) => Picked::Chat(self.chats[i].chat.clone()),
            Row::Run(i) => Picked::Run(self.runs[i].run.clone()),
            Row::Session(i) => Picked::Session(self.sessions[i].reference()),
            Row::Agent(a) => Picked::Agent(self.agents[a].reference()),
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

    fn selected_chat(&self) -> Option<&ChatSummary> {
        match self.selected_row()? {
            Row::Chat(i) => self.chats.get(i),
            _ => None,
        }
    }

    fn selected_session(&self) -> Option<(&VendorSession, &str)> {
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

    /// More than runs, so a Kind column tells them apart.
    fn mixed(&self) -> bool {
        !self.chats.is_empty() || !self.sessions.is_empty() || !self.agents.is_empty()
    }

    fn columns(&self, width: usize, names: &DisplayNames) -> Vec<Column> {
        let now = self.clock.now();
        let cells = |cell: &dyn Fn(Row) -> String| -> Vec<String> {
            self.rows.iter().map(|row| cell(*row)).collect()
        };
        let mixed = self.mixed();
        let agents_shown = self.rows.iter().any(|row| matches!(row, Row::Agent(_)));
        let mut columns = Vec::new();
        if mixed {
            columns.push(Column::new("Kind", cells(&|row| self.kind(row).to_owned())).keep(6));
        }
        columns.extend([
            Column::new(
                if mixed { "Id" } else { "Run" },
                cells(&|row| match row {
                    Row::Run(i) => sanitize_line(self.runs[i].run.as_str()).into_owned(),
                    Row::Chat(i) => sanitize_line(self.chats[i].chat.as_str()).into_owned(),
                    Row::Session(i) => sanitize_line(&self.session_ids[i]).into_owned(),
                    // Indented under its run.
                    Row::Agent(a) => format!("  {}", clean(self.agents[a].session.as_str())),
                }),
            ),
            {
                let title = Column::new(
                    "Title",
                    cells(&|row| match row {
                        Row::Run(i) => clean(&self.runs[i].title),
                        Row::Chat(i) => clean(&self.chats[i].title),
                        Row::Session(i) => clean(&self.sessions[i].title),
                        Row::Agent(a) => agent_title(&self.agents[a]),
                    }),
                );
                if agents_shown {
                    title.flex_min(AGENT_TITLE_MIN)
                } else {
                    title.flex()
                }
            },
            Column::new(
                "State",
                cells(&|row| match row {
                    Row::Run(i) => state_word(self.runs[i].state).to_owned(),
                    Row::Agent(a) => agent_state(&self.agents[a]).to_owned(),
                    _ => String::new(),
                }),
            )
            .keep(5),
            Column::new(
                if mixed { "Details" } else { "Stopped at" },
                cells(&|row| match row {
                    Row::Run(i) => run_details(&self.runs[i]),
                    Row::Chat(i) => chat_details(&self.chats[i], names),
                    Row::Session(i) => session_details(&self.sessions[i]),
                    Row::Agent(a) => sanitize_line(&names.worker(&self.agents[a].worker))
                        .trim()
                        .to_owned(),
                }),
            )
            .keep(1),
            Column::new(
                "When",
                cells(&|row| match row {
                    Row::Run(i) => ago(self.runs[i].updated, now),
                    Row::Chat(i) => ago(self.chats[i].updated, now),
                    Row::Session(i) => ago(self.sessions[i].updated, now),
                    Row::Agent(a) => ago(self.agents[a].updated, now),
                }),
            )
            .numeric()
            .keep(4),
            Column::new(
                "Spent",
                cells(&|row| match row {
                    Row::Run(i) => spent(&self.runs[i].spent),
                    _ => String::new(),
                }),
            )
            .numeric()
            .keep(3),
            Column::new(
                "Sessions",
                cells(&|row| match row {
                    Row::Run(i) => self.runs[i].sessions_to_resume.to_string(),
                    _ => String::new(),
                }),
            )
            .numeric()
            .keep(2),
        ]);
        if mixed {
            // Conversations and CLI sessions have no state, spend, or sessions; a column none
            // of the rows fills gives its room to the titles.
            columns.retain(|column| {
                column.heading == "Title" || column.cells.iter().any(|cell| !cell.is_empty())
            });
        }
        fit_columns(columns, width, GAP)
    }

    fn kind(&self, row: Row) -> &'static str {
        match row {
            Row::Run(_) => "run",
            Row::Chat(_) => "chat",
            Row::Session(_) => "session",
            Row::Agent(_) => "agent",
        }
    }

    fn lines(&self, width: usize, names: &DisplayNames, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        // The selection mark and the ◆ slot lead every row.
        let lead = 4;
        let columns = self.columns(width.saturating_sub(lead), names);
        let mut heading = vec![Span::raw(" ".repeat(lead))];
        for (i, column) in columns.iter().enumerate() {
            if i > 0 {
                heading.push(Span::raw(" ".repeat(GAP)));
            }
            heading.push(Span::styled(column.cell(column.heading), dim));
        }
        let mut lines = vec![Line::from(heading)];
        for (i, row) in self.rows.iter().enumerate() {
            let selected = i == self.list.selected;
            let run = match *row {
                Row::Run(r) => Some(&self.runs[r]),
                _ => None,
            };
            let mut spans = vec![
                if selected {
                    Span::styled(format!("{} ", marks.selected), theme.style(Role::Label))
                } else {
                    Span::raw("  ")
                },
                if run.is_some_and(|run| run.remind) {
                    Span::styled(marks.needs_you, theme.style(Role::Card))
                } else {
                    Span::raw(" ")
                },
                Span::raw(" "),
            ];
            for (c, column) in columns.iter().enumerate() {
                if c > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                let style = match (column.heading, run) {
                    ("Title", _) if selected => theme.style(Role::Label),
                    ("Title", _) => theme.style(Role::Text),
                    ("State", Some(run)) => theme.style(state_role(run.state)),
                    _ => dim,
                };
                spans.push(Span::styled(column.cell(&column.cells[i]), style));
            }
            lines.push(Line::from(spans));
        }
        if self.rows.is_empty() {
            let text = if self.filter.trim().is_empty() {
                "  No parked or stopped runs"
            } else {
                "  Nothing matches"
            };
            lines.push(Line::from(Span::styled(text, dim)));
        }
        lines
    }

    /// The keys for the row selected. While you type a search, letters are the search's, so
    /// only the keys without them show.
    fn footer(&self) -> Footer {
        if let Some(run) = &self.confirm_stop {
            let again = format!("x again to stop {}", sanitize_line(run.as_str()));
            return Footer::hints(&[(&again, 0), ("esc to keep", 0)]);
        }
        let letters = !self.filtering;
        let mut hints = vec![("↑↓ select", 0)];
        match self.selected_row() {
            Some(Row::Agent(a)) => match &self.agents[a].resume {
                Resume::Ready => hints.push(("⏎ continue", 6)),
                Resume::Cannot { .. } => hints.push(("⏎ transcript", 6)),
                Resume::Working | Resume::RunOwns => {}
            },
            Some(Row::Run(r)) => {
                let run = &self.runs[r];
                let open = self.open.contains(&run.run);
                if run.state == RunPhase::Merged {
                    hints.push((
                        if open {
                            "⏎ hide agents"
                        } else {
                            "⏎ agents"
                        },
                        6,
                    ));
                } else {
                    hints.push(("⏎ resume", 6));
                    hints.push((
                        if open {
                            "← hide agents"
                        } else {
                            "→ agents"
                        },
                        5,
                    ));
                }
                if letters && !run.state.ended() {
                    hints.push(("x stop", 4));
                }
                if letters {
                    hints.push(("d diff", 3));
                }
            }
            Some(Row::Chat(_) | Row::Session(_)) => hints.push(("⏎ resume", 6)),
            None => {}
        }
        let raw = matches!(self.selected_row(), Some(Row::Run(_) | Row::Agent(_)));
        if letters && raw {
            hints.push(("r raw", 2));
        }
        if letters {
            hints.extend([("/ search", 1), (panes::ESC_BACK, 0)]);
        } else {
            hints.push(("esc clear search", 0));
        }
        Footer::hints(&hints)
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
            Some(Row::Chat(i)) => resume(ResumeTarget::Conversation(self.chats[i].chat.clone())),
            Some(Row::Session(i)) => {
                resume(ResumeTarget::VendorSession(self.sessions[i].reference()))
            }
            Some(Row::Run(r)) if self.runs[r].state == RunPhase::Merged => {
                if self.open.contains(&self.runs[r].run) {
                    self.fold(r)
                } else {
                    self.unfold(r)
                }
            }
            // The run takes the screen, as a conversation carried on does, so its card isn't
            // left waiting behind the picker.
            Some(Row::Run(r)) => ViewOutcome::SendToChat(UiCommand::Resume {
                run: Some(self.runs[r].run.clone()),
            }),
            None => ViewOutcome::Consumed,
        }
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
            Row::Chat(_) | Row::Session(_) => None,
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
        if let Some(chat) = self.selected_chat() {
            return ViewOutcome::Notice(format!(
                "{} is a conversation. ⏎ carries it on",
                sanitize_line(chat.chat.as_str())
            ));
        }
        match self.selected_session() {
            Some((session, id)) => ViewOutcome::Notice(format!(
                "{} is a {} session. ⏎ carries it on",
                sanitize_line(id),
                vendor_label(&sanitize_line(&session.vendor.0))
            )),
            None => ViewOutcome::Consumed,
        }
    }

    /// `Resume a conversation or a run`, or a session too when the CLI has some, and how many
    /// of each, or how many match what you typed.
    fn header(&self, theme: &Theme) -> Header {
        if !self.mixed() {
            return Header {
                title: Line::from(Span::styled("Resume a run", theme.style(Role::Label))),
                description: "Parked runs never expire, so resume it or stop it".to_owned(),
                summary: self.matching(panes::count(self.runs.len(), "run")),
            };
        }
        let sessions = self.sessions.first().map_or_else(String::new, |first| {
            format!("{} session", vendor_label(&sanitize_line(&first.vendor.0)))
        });
        // Only the kinds listed, so a list of sessions alone doesn't read `0 runs`.
        let summary: Vec<String> = [
            (self.chats.len(), "conversation"),
            (self.runs.len(), "run"),
            (self.agents.len(), "agent session"),
            (self.sessions.len(), sessions.as_str()),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| panes::count(count, noun))
        .collect();
        let title = if self.sessions.is_empty() && self.agents.is_empty() {
            "Resume a conversation or a run"
        } else {
            "Resume a conversation, a run, or a session"
        };
        Header {
            title: Line::from(Span::styled(title, theme.style(Role::Label))),
            description: "Pick one to carry on. Parked runs never expire".to_owned(),
            summary: self.matching(summary.join(" · ")),
        }
    }

    /// The counts, or how many rows match what you typed.
    fn matching(&self, counts: String) -> String {
        if self.filter.trim().is_empty() {
            return counts;
        }
        let total = self.chats.len() + self.runs.len() + self.sessions.len() + self.agents.len();
        format!("{} of {total} match", self.rows.len())
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
fn short_ids(sessions: &[VendorSession]) -> Vec<String> {
    sessions
        .iter()
        .map(|session| {
            let id = session.id.as_str();
            let shared = sessions
                .iter()
                .filter(|other| other.vendor == session.vendor && other.id != session.id)
                .map(|other| common_prefix(id, &other.id))
                .max()
                .unwrap_or(0);
            let cut = id
                .char_indices()
                .map(|(at, _)| at)
                .chain([id.len()])
                .find(|&at| at >= SHORT_ID.max(shared + 1))
                .unwrap_or(id.len());
            format!("{}:{}", session.vendor, &id[..cut])
        })
        .collect()
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((at, _), _)| at)
}

/// `Claude Code on main`.
fn session_details(session: &VendorSession) -> String {
    let vendor = vendor_label(&sanitize_line(&session.vendor.0));
    match session.branch.as_deref().map(clean) {
        Some(branch) if !branch.is_empty() => format!("{vendor} on {branch}"),
        _ => vendor,
    }
}

/// `3 turns with claude-code`, or `1 run` for one that only started or opened runs.
fn chat_details(chat: &ChatSummary, names: &DisplayNames) -> String {
    let turns = match (chat.turns, chat.runs) {
        (0, 1) => "1 run".to_owned(),
        (0, runs) if runs > 1 => format!("{runs} runs"),
        (1, _) => "1 turn".to_owned(),
        (turns, _) => format!("{turns} turns"),
    };
    match &chat.worker {
        Some(worker) => format!("{turns} with {}", sanitize_line(&names.worker(worker))),
        None => turns,
    }
}

/// `owner · build:money · 3 subagents · Fixed both functions.`, by the agent's name in the run
/// (its card, for a run from before names). The helpers its CLI started come back with the
/// session, so they're counted here rather than listed.
fn agent_title(agent: &AgentSession) -> String {
    let role = agent.role.as_str();
    let parts = [
        Some(role.to_owned()),
        // A step named like its role, as the retro is, says it once.
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

/// The request for a run's diff: its newest card with changes, as far as this screen knows.
pub(crate) fn diff_request(state: &AppState, run: &RunId) -> Result<ViewRequest, String> {
    let known = state.runs.get(run);
    let card = known.and_then(|known| {
        known
            .cards
            .values()
            .rev()
            .find(|card| card.diff.is_some_and(|diff| diff.files_changed > 0))
            .or_else(|| known.cards.values().next_back())
            .map(|card| card.id.clone())
    });
    match card {
        Some(card) => Ok(ViewRequest::Diff {
            run: run.clone(),
            card,
            scope: DiffScope::Card,
        }),
        None => Err(format!(
            "No cards of {} on this screen yet. Resume it to see its diff",
            sanitize_line(run.as_str())
        )),
    }
}

/// The request for a run's raw log: its newest session's vendor lines.
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

/// Where the run stopped, unless that only repeats its state, as `merged` does for a merged
/// run.
fn run_details(run: &RunSummary) -> String {
    let details = clean(&run.stopped_at);
    if details.eq_ignore_ascii_case(state_word(run.state)) {
        String::new()
    } else {
        details
    }
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

pub(crate) fn state_word(state: RunPhase) -> &'static str {
    match state {
        RunPhase::Running => "running",
        RunPhase::Pausing => "pausing",
        RunPhase::Parked => "parked",
        RunPhase::Crashed => "crashed",
        RunPhase::Merged => "merged",
        RunPhase::PullRequest => "pull request",
        RunPhase::BranchLeft => "branch left",
        RunPhase::Stopped => "stopped",
        RunPhase::Failed => "failed",
        RunPhase::Abandoned => "abandoned",
    }
}

pub(crate) fn state_role(state: RunPhase) -> Role {
    match state {
        RunPhase::Running | RunPhase::Pausing => Role::Accent,
        RunPhase::Parked => Role::Warning,
        RunPhase::Crashed | RunPhase::Failed => Role::Error,
        RunPhase::Merged | RunPhase::PullRequest => Role::Success,
        RunPhase::BranchLeft | RunPhase::Stopped | RunPhase::Abandoned => Role::Dim,
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

/// Dollars once anything was priced, else tokens.
pub(crate) fn spent(spend: &Spend) -> String {
    panes::spend(spend).unwrap_or_else(|| panes::tokens(spend.total_tokens()))
}

impl View for ResumePicker {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let header = self.header(theme);
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        let search = if self.filter.is_empty() && !self.filtering {
            String::new()
        } else {
            format!("Search: {}", sanitize_line(&self.filter))
        };
        panes::draw_box(buf, &panes, ["", &search], Side::Right, theme);

        // The heading stays; the rows scroll under it.
        let height = usize::from(panes.right.height).saturating_sub(1);
        self.page = height.max(1);
        self.list.keep_visible(self.rows.len(), self.page);
        let lines = self.lines(usize::from(panes.right.width), &ctx.state.names, theme);
        let mut shown = vec![lines[0].clone()];
        shown.extend(lines.iter().skip(1 + self.list.offset).cloned());
        panes::put_lines(buf, panes.right, &shown);

        let marks = Marks::for_theme(theme);
        let mark =
            super::pager::position_mark(self.list.offset, height, self.rows.len(), marks.down);
        panes::draw_footer(buf, panes.footer, self.footer(), &mark, theme);
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
                chats,
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
                self.chats = chats.clone();
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
mod tests {
    use std::time::Instant;

    use argonauts_core::api::{RunState as RunPhase, RunSummary};
    use argonauts_core::app::{ChatSummary, UiCommand};
    use argonauts_core::events::run::RunMode;
    use argonauts_core::input::{ResumeTarget, SlashCommand};
    use argonauts_core::names::{ChatId, VendorId};
    use argonauts_core::vendor_sessions::{VendorSession, VendorSessionRef};
    use argonauts_core::{RunId, Spend};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use time::macros::datetime;
    use time::{Duration, OffsetDateTime};

    use super::{Footer, ResumePicker};
    use crate::state::AppState;
    use crate::theme::Theme;
    use crate::views::{View, ViewCtx, ViewOutcome};

    const WALL: OffsetDateTime = datetime!(2026-09-30 12:00 UTC);

    fn run(id: &str, hours_ago: i64) -> RunSummary {
        RunSummary {
            run: RunId::new(id).unwrap(),
            title: format!("{id} title"),
            mode: RunMode::Crew,
            preset: "peer-crew".into(),
            state: RunPhase::Parked,
            stopped_at: "merge checkpoint".into(),
            updated: WALL - Duration::hours(hours_ago),
            spent: Spend::default(),
            sessions_to_resume: 1,
            waiting: Vec::new(),
            live: false,
            remind: false,
            name: None,
        }
    }

    fn chat(n: u32, title: &str, hours_ago: i64) -> ChatSummary {
        ChatSummary {
            chat: ChatId::numbered(n),
            title: title.to_owned(),
            updated: WALL - Duration::hours(hours_ago),
            worker: Some("claude-code".parse().unwrap()),
            turns: 2,
            runs: 0,
        }
    }

    fn picker() -> ResumePicker {
        ResumePicker::with_chats(
            vec![run("run-02", 1), run("run-01", 5)],
            vec![chat(2, "hi", 0), chat(1, "what does export do?", 3)],
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
        let theme = Theme::dark();
        let text: Vec<String> = view
            .lines(100, &Default::default(), &theme)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(
            text[0].contains("Kind") && text[0].contains("Id"),
            "{text:?}"
        );
        let order: Vec<&str> = text[1..]
            .iter()
            .map(|row| {
                ["chat-002", "run-02", "chat-001", "run-01"]
                    .into_iter()
                    .find(|id| row.contains(id))
                    .unwrap_or("?")
            })
            .collect();
        assert_eq!(order, vec!["chat-002", "run-02", "chat-001", "run-01"]);
        assert!(text[1].contains("chat") && text[1].contains("2 turns with Claude Code"));
        assert!(text[2].contains("run ") && text[2].contains("parked"));
    }

    /// A conversation that only started or opened runs counts those, not "0 turns".
    #[test]
    fn a_conversation_with_only_runs_counts_its_runs() {
        use super::chat_details;
        let names = argonauts_core::display_names::DisplayNames::default();
        let only_runs = ChatSummary {
            turns: 0,
            runs: 1,
            worker: None,
            ..chat(3, "fix the median", 1)
        };
        assert_eq!(chat_details(&only_runs, &names), "1 run");
        let two = ChatSummary {
            runs: 2,
            ..only_runs.clone()
        };
        assert_eq!(chat_details(&two, &names), "2 runs");
        let chatted = ChatSummary {
            runs: 2,
            ..chat(2, "hi", 1)
        };
        assert_eq!(chat_details(&chatted, &names), "2 turns with Claude Code");
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
                    target: Some(ResumeTarget::Conversation(ChatId::numbered(2))),
                },
            })
        );
        assert!(matches!(
            press(&mut view, KeyCode::Char('x')),
            ViewOutcome::Notice(text) if text.contains("chat-002 is a conversation")
        ));
        press(&mut view, KeyCode::Down);
        // A run closes the picker on the way, so it and its card take the screen.
        assert!(matches!(
            press(&mut view, KeyCode::Enter),
            ViewOutcome::SendToChat(UiCommand::Resume { run: Some(run) }) if run.as_str() == "run-02"
        ));
    }

    #[test]
    fn a_conversation_offers_no_run_keys() {
        let mut view = picker();
        let hints = |view: &ResumePicker| match view.footer() {
            Footer::Hints(parts) => parts
                .into_iter()
                .map(|(part, _)| part)
                .collect::<Vec<_>>()
                .join(" · "),
            Footer::Line(_) => unreachable!(),
        };
        assert_eq!(hints(&view), "↑↓ select · ⏎ resume · / search · esc back");
        press(&mut view, KeyCode::Down);
        assert!(hints(&view).contains("x stop · d diff · r raw"));
    }

    fn session(vendor: &str, id: &str, title: &str, hours_ago: i64) -> VendorSession {
        VendorSession {
            vendor: VendorId(vendor.into()),
            id: id.into(),
            title: title.into(),
            prompt: None,
            folder: "/work/shop".into(),
            updated: WALL - Duration::hours(hours_ago),
            model: None,
            branch: Some("main".into()),
            last_message: None,
        }
    }

    fn with_sessions() -> ResumePicker {
        let mut view = ResumePicker::listing(
            vec![run("run-02", 1)],
            vec![chat(2, "hi", 3)],
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

    /// A merged run says so once, under State, and Details stays empty rather than saying it
    /// again; a parked one still says where it stopped.
    #[test]
    fn a_finished_run_says_its_state_once() {
        let mut merged = run("run-02", 1);
        merged.state = RunPhase::Merged;
        merged.stopped_at = "merged".into();
        let view = ResumePicker::with_chats(vec![merged, run("run-01", 5)], vec![chat(1, "hi", 0)]);
        let text = rows(&view, 120);
        let row = |id: &str| text.iter().find(|row| row.contains(id)).unwrap().clone();
        assert_eq!(row("run-02").matches("merged").count(), 1, "{text:#?}");
        assert!(row("run-01").contains("merge checkpoint"), "{text:#?}");
    }

    #[test]
    fn cli_sessions_list_with_the_rest_under_a_kind() {
        let view = with_sessions();
        let text = rows(&view, 120);
        let order: Vec<&str> = text[1..]
            .iter()
            .map(|row| {
                [
                    "claude-code:0a1b2c3d-1",
                    "run-02",
                    "claude-code:0a1b2c3d-2",
                    "chat-002",
                    "claude-code:9f8e7d6c",
                ]
                .into_iter()
                .find(|id| row.contains(id))
                .unwrap_or("?")
            })
            .collect();
        assert_eq!(
            order,
            vec![
                "claude-code:0a1b2c3d-1",
                "run-02",
                "claude-code:0a1b2c3d-2",
                "chat-002",
                "claude-code:9f8e7d6c"
            ]
        );
        let first = &text[1];
        for cell in [
            "session",
            "Input loop notes",
            "Claude Code on main",
            "just now",
        ] {
            assert!(first.contains(cell), "{cell}: {first}");
        }
        // Two ids that start alike show until they differ; another shows its first part.
        assert!(!text[5].contains("9f8e7d6c-"), "{}", text[5]);
    }

    #[test]
    fn enter_carries_a_cli_session_on() {
        let mut view = with_sessions();
        let ViewOutcome::Send(sent) = press(&mut view, KeyCode::Enter) else {
            panic!("Enter sends nothing");
        };
        assert_eq!(
            sent,
            UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(ResumeTarget::VendorSession(VendorSessionRef {
                        vendor: VendorId("claude-code".into()),
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
    fn the_header_counts_the_cli_sessions() {
        let view = with_sessions();
        let header = view.header(&Theme::dark());
        assert_eq!(
            header.summary,
            "1 conversation · 1 run · 3 Claude Code sessions"
        );
        assert_eq!(
            header.title.to_string(),
            "Resume a conversation, a run, or a session"
        );

        let alone = ResumePicker::listing(
            Vec::new(),
            Vec::new(),
            vec![session(
                "codex",
                "019a5770-e3f9",
                "Explain the input loop",
                1,
            )],
        );
        assert_eq!(alone.header(&Theme::dark()).summary, "1 Codex session");
    }

    #[test]
    fn a_fresh_list_keeps_the_session_selected() {
        let mut view = with_sessions();
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Down);
        let data = argonauts_core::app::ViewData::Runs {
            runs: Vec::new(),
            chats: Vec::new(),
            sessions: view.sessions.clone(),
            agents: Vec::new(),
            agents_read: Vec::new(),
        };
        assert!(view.accept(&data));
        let (picked, _) = view.selected_session().unwrap();
        assert_eq!(picked.id, "0a1b2c3d-2222-4aaa");
    }

    #[test]
    fn runs_alone_keep_the_runs_picker() {
        let view = ResumePicker::new(vec![run("run-01", 1)]);
        let theme = Theme::dark();
        let heading: String = view.lines(100, &Default::default(), &theme)[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            heading.contains("Run") && !heading.contains("Kind"),
            "{heading}"
        );
    }
}
