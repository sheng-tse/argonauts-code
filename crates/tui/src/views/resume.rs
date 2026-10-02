//! The resume picker: earlier conversations with the lead, parked or stopped runs, and the main
//! agent CLI's own sessions for this repo, newest first, with where each stopped, when, what it
//! spent, and what resuming restarts. Parked runs never expire.

use argonauts_core::api::{DiffScope, RunState as RunPhase, RunSummary};
use argonauts_core::app::{ChatSummary, UiCommand, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::vendor_sessions::VendorSession;
use argonauts_core::{RunId, Spend};
use crossterm::event::{KeyCode, KeyEvent};
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

#[derive(Debug)]
pub struct ResumePicker {
    pub runs: Vec<RunSummary>,
    pub chats: Vec<ChatSummary>,
    /// The main agent CLI's own sessions.
    pub sessions: Vec<VendorSession>,
    /// What the Id column shows for each session, such as `codex:019a5770`.
    session_ids: Vec<String>,
    /// Every kind by when each was last used, newest first.
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
            rows: Vec::new(),
            list: ListState::default(),
            clock: WallClock::local(),
            confirm_stop: None,
            page: 1,
        };
        picker.rows = picker.merged();
        picker
    }

    /// Each list is newest first already, so a merge keeps that. On a tie a conversation comes
    /// first, then a run.
    fn merged(&self) -> Vec<Row> {
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
            }
            rows.push(next);
        }
        rows
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.list.selected).copied()
    }

    fn selected(&self) -> Option<&RunSummary> {
        match self.selected_row()? {
            Row::Run(i) => self.runs.get(i),
            Row::Chat(_) | Row::Session(_) => None,
        }
    }

    fn selected_chat(&self) -> Option<&ChatSummary> {
        match self.selected_row()? {
            Row::Chat(i) => self.chats.get(i),
            Row::Run(_) | Row::Session(_) => None,
        }
    }

    fn selected_session(&self) -> Option<(&VendorSession, &str)> {
        match self.selected_row()? {
            Row::Session(i) => Some((self.sessions.get(i)?, self.session_ids.get(i)?)),
            Row::Run(_) | Row::Chat(_) => None,
        }
    }

    /// More than runs, so a Kind column tells them apart.
    fn mixed(&self) -> bool {
        !self.chats.is_empty() || !self.sessions.is_empty()
    }

    fn columns(&self, width: usize, names: &DisplayNames) -> Vec<Column> {
        let now = self.clock.now();
        let cells = |run: &dyn Fn(&RunSummary) -> String,
                     chat: &dyn Fn(&ChatSummary) -> String,
                     session: &dyn Fn(usize, &VendorSession) -> String| {
            self.rows
                .iter()
                .map(|row| match *row {
                    Row::Run(i) => run(&self.runs[i]),
                    Row::Chat(i) => chat(&self.chats[i]),
                    Row::Session(i) => session(i, &self.sessions[i]),
                })
                .collect()
        };
        let blank = |_: &ChatSummary| String::new();
        let none = |_: usize, _: &VendorSession| String::new();
        let mixed = self.mixed();
        let mut columns = Vec::new();
        if mixed {
            columns.push(
                Column::new(
                    "Kind",
                    cells(&|_| "run".to_owned(), &|_| "chat".to_owned(), &|_, _| {
                        "session".to_owned()
                    }),
                )
                .keep(6),
            );
        }
        columns.extend([
            Column::new(
                if mixed { "Id" } else { "Run" },
                cells(
                    &|run| sanitize_line(run.run.as_str()).into_owned(),
                    &|chat| sanitize_line(chat.chat.as_str()).into_owned(),
                    &|i, _| sanitize_line(&self.session_ids[i]).into_owned(),
                ),
            ),
            Column::new(
                "Title",
                cells(
                    &|run| clean(&run.title),
                    &|chat| clean(&chat.title),
                    &|_, session| clean(&session.title),
                ),
            )
            .flex(),
            Column::new(
                "State",
                cells(&|run| state_word(run.state).to_owned(), &blank, &none),
            )
            .keep(5),
            Column::new(
                if mixed { "Details" } else { "Stopped at" },
                cells(
                    &|run| clean(&run.stopped_at),
                    &|chat| chat_details(chat, names),
                    &|_, session| session_details(session),
                ),
            )
            .keep(1),
            Column::new(
                "When",
                cells(
                    &|run| ago(run.updated, now),
                    &|chat| ago(chat.updated, now),
                    &|_, session| ago(session.updated, now),
                ),
            )
            .numeric()
            .keep(4),
            Column::new("Spent", cells(&|run| spent(&run.spent), &blank, &none))
                .numeric()
                .keep(3),
            Column::new(
                "Sessions",
                cells(&|run| run.sessions_to_resume.to_string(), &blank, &none),
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
                Row::Chat(_) | Row::Session(_) => None,
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
            lines.push(Line::from(Span::styled("  No parked or stopped runs", dim)));
        }
        lines
    }

    fn footer(&self) -> Footer {
        match &self.confirm_stop {
            Some(run) => {
                let again = format!("x again to stop {}", sanitize_line(run.as_str()));
                Footer::hints(&[(&again, 0), ("esc to keep", 0)])
            }
            // `x`, `d`, and `r` act on runs only, so a conversation doesn't offer them.
            None if self.selected().is_none() => {
                Footer::hints(&[("↑↓ select", 0), ("⏎ resume", 5), (panes::ESC_BACK, 0)])
            }
            None => Footer::hints(&[
                ("↑↓ select", 0),
                ("⏎ resume", 5),
                ("x stop", 4),
                ("d diff", 3),
                ("r raw", 2),
                (panes::ESC_BACK, 0),
            ]),
        }
    }

    fn stop(&mut self, state: &AppState) -> ViewOutcome {
        let Some(run) = self.selected().map(|run| run.run.clone()) else {
            return self.not_a_run();
        };
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

    /// `x`, `d`, and `r` act on runs; a conversation or a CLI session has none of those.
    fn not_a_run(&self) -> ViewOutcome {
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
    /// of each.
    fn header(&self, theme: &Theme) -> Header {
        if !self.mixed() {
            return Header {
                title: Line::from(Span::styled("Resume a run", theme.style(Role::Label))),
                description: "Parked runs never expire, so resume it or stop it".to_owned(),
                summary: panes::count(self.runs.len(), "run"),
            };
        }
        let sessions = self.sessions.first().map_or_else(String::new, |first| {
            format!("{} session", vendor_label(&sanitize_line(&first.vendor.0)))
        });
        // Only the kinds listed, so a list of sessions alone doesn't read `0 runs`.
        let summary: Vec<String> = [
            (self.chats.len(), "conversation"),
            (self.runs.len(), "run"),
            (self.sessions.len(), sessions.as_str()),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| panes::count(count, noun))
        .collect();
        let title = if self.sessions.is_empty() {
            "Resume a conversation or a run"
        } else {
            "Resume a conversation, a run, or a session"
        };
        Header {
            title: Line::from(Span::styled(title, theme.style(Role::Label))),
            description: "Pick one to carry on. Parked runs never expire".to_owned(),
            summary: summary.join(" · "),
        }
    }
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

/// `3 turns with claude-code`.
fn chat_details(chat: &ChatSummary, names: &DisplayNames) -> String {
    let turns = if chat.turns == 1 {
        "1 turn".to_owned()
    } else {
        format!("{} turns", chat.turns)
    };
    match &chat.worker {
        Some(worker) => format!("{turns} with {}", sanitize_line(&names.worker(worker))),
        None => turns,
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
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

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
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        // Any other key keeps the run; Esc only does that.
        if key.code != KeyCode::Char('x')
            && self.confirm_stop.take().is_some()
            && key.code == KeyCode::Esc
        {
            return ViewOutcome::Consumed;
        }
        let run = self.selected().map(|run| run.run.clone());
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ViewOutcome::Close,
            KeyCode::Enter => {
                let target = match (self.selected_chat(), self.selected_session()) {
                    (Some(chat), _) => Some(ResumeTarget::Conversation(chat.chat.clone())),
                    (None, Some((session, _))) => {
                        Some(ResumeTarget::VendorSession(session.reference()))
                    }
                    (None, None) => None,
                };
                if let Some(target) = target {
                    return ViewOutcome::Send(UiCommand::Slash {
                        command: SlashCommand::Resume {
                            target: Some(target),
                        },
                    });
                }
                return match run {
                    Some(run) => ViewOutcome::Send(UiCommand::Resume { run: Some(run) }),
                    None => ViewOutcome::Consumed,
                };
            }
            KeyCode::Char('x') => return self.stop(ctx.state),
            KeyCode::Char(c @ ('d' | 'r')) => {
                let Some(run) = run else {
                    return self.not_a_run();
                };
                let request = if c == 'd' {
                    diff_request(ctx.state, &run)
                } else {
                    raw_request(ctx.state, &run)
                };
                return match request {
                    Ok(request) => ViewOutcome::Request(request),
                    Err(why) => ViewOutcome::Notice(why),
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

    fn accept(&mut self, data: &argonauts_core::app::ViewData) -> bool {
        // A fresh list replaces this one in place, keeping the selection on the same row.
        let argonauts_core::app::ViewData::Runs {
            runs,
            chats,
            sessions,
        } = data
        else {
            return false;
        };
        let run = self.selected().map(|run| run.run.clone());
        let chat = self.selected_chat().map(|chat| chat.chat.clone());
        let session = self
            .selected_session()
            .map(|(session, _)| session.reference());
        self.runs = runs.clone();
        self.chats = chats.clone();
        self.sessions = sessions.clone();
        self.session_ids = short_ids(&self.sessions);
        self.rows = self.merged();
        let index = self
            .rows
            .iter()
            .position(|row| match *row {
                Row::Run(i) => run.as_ref() == Some(&self.runs[i].run),
                Row::Chat(i) => chat.as_ref() == Some(&self.chats[i].chat),
                Row::Session(i) => session.as_ref() == Some(&self.sessions[i].reference()),
            })
            .unwrap_or(0);
        self.list.move_to(index, self.rows.len(), self.page);
        true
    }
}

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
        }
    }

    fn chat(n: u32, title: &str, hours_ago: i64) -> ChatSummary {
        ChatSummary {
            chat: ChatId::numbered(n),
            title: title.to_owned(),
            updated: WALL - Duration::hours(hours_ago),
            worker: Some("claude-code".parse().unwrap()),
            turns: 2,
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
        assert_eq!(
            sent(press(&mut view, KeyCode::Enter)),
            Some(UiCommand::Resume {
                run: Some(RunId::new("run-02").unwrap())
            })
        );
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
        assert_eq!(hints(&view), "↑↓ select · ⏎ resume · esc back");
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
