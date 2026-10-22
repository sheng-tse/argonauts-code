//! The resume picker: one list of earlier conversations and the CLIs' own sessions for this
//! folder, newest first, with where each is from, when it was last used, and how long it is.
//! Enter carries one on, `b` branches from it, and `a` lists every folder's sessions. A run is
//! never listed on its own: it comes back with the conversation that started it, whose row is
//! marked while one of its runs waits on you. `/` narrows the list as you type, since its
//! letters are keys.

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
    pub conversations: Vec<ConversationSummary>,
    /// The main agent CLI's own sessions.
    pub sessions: Vec<CliSession>,
    /// What the Id column shows for each session, such as `codex:019a5770`.
    session_ids: Vec<String>,
    /// What you typed to narrow the list.
    filter: String,
    /// Typing goes to the filter, from `/` until Esc.
    filtering: bool,
    /// What shows, by when each was last used, newest first.
    rows: Vec<Row>,
    pub list: ListState,
    /// Shows times like `2h ago`; tests pin it.
    pub clock: WallClock,
    /// The session a first Enter warned may still be open in its CLI.
    confirm_open: Option<CliSessionRef>,
    /// The CLIs' sessions listed are every folder's.
    every_folder: bool,
    page: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Conversation(usize),
    Session(usize),
}

/// A row by what it lists, which a fresh list or a new filter keeps selected.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Picked {
    Conversation(ConversationId),
    Session(CliSessionRef),
}

impl ResumePicker {
    pub fn listing(conversations: Vec<ConversationSummary>, sessions: Vec<CliSession>) -> Self {
        let mut picker = Self {
            conversations,
            session_ids: short_ids(&sessions),
            sessions,
            filter: String::new(),
            filtering: false,
            rows: Vec::new(),
            list: ListState::default(),
            clock: WallClock::local(),
            confirm_open: None,
            every_folder: false,
            page: 1,
        };
        picker.rows = picker.merged();
        picker
    }

    /// Each list is newest first already, so a merge keeps that. On a tie a conversation comes
    /// first, and a row that doesn't match what you typed is left out.
    fn merged(&self) -> Vec<Row> {
        let words = search_words(&self.filter);
        let mut rows = Vec::with_capacity(self.conversations.len() + self.sessions.len());
        let (mut conversation, mut session) = (0, 0);
        loop {
            let next = match (
                self.conversations.get(conversation),
                self.sessions.get(session),
            ) {
                (Some(c), Some(s)) if s.updated > c.updated => Row::Session(session),
                (Some(_), _) => Row::Conversation(conversation),
                (None, Some(_)) => Row::Session(session),
                (None, None) => break,
            };
            match next {
                Row::Conversation(_) => conversation += 1,
                Row::Session(_) => session += 1,
            }
            if self.matches(next, &words) {
                rows.push(next);
            }
        }
        rows
    }

    /// Whether the row's words hold every word you typed.
    fn matches(&self, row: Row, words: &[String]) -> bool {
        if words.is_empty() {
            return true;
        }
        let text = self.search_text(row).to_lowercase();
        words.iter().all(|word| text.contains(word.as_str()))
    }

    /// What a row is found by: its id and title, and what it ran on.
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
            Row::Session(i) => {
                let session = &self.sessions[i];
                let branch = session.branch.as_deref().unwrap_or_default();
                let cli = agent_label(&session.agent_id.0);
                let id = &self.session_ids[i];
                format!("{id} {} {cli} {branch}", session.title)
            }
        }
    }

    fn picked(&self, row: Row) -> Picked {
        match row {
            Row::Conversation(i) => {
                Picked::Conversation(self.conversations[i].conversation.clone())
            }
            Row::Session(i) => Picked::Session(self.sessions[i].reference()),
        }
    }

    /// A row's kind and id, such as `session codex:019a5770`, which tests tell rows apart by.
    #[cfg(test)]
    fn named(&self, row: Row) -> String {
        match row {
            Row::Conversation(i) => format!("conversation {}", self.conversations[i].conversation),
            Row::Session(i) => format!("session {}", self.session_ids[i]),
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

    #[cfg(test)]
    fn selected_session(&self) -> Option<(&CliSession, &str)> {
        match self.selected_row()? {
            Row::Session(i) => Some((self.sessions.get(i)?, self.session_ids.get(i)?)),
            Row::Conversation(_) => None,
        }
    }

    /// Where a row is from: Argonauts Code's own conversations, or a CLI by its name and color.
    fn from(&self, row: Row, names: &DisplayNames, theme: &Theme) -> (String, Style) {
        match row {
            Row::Conversation(_) => (
                "Argonauts Code".to_owned(),
                theme.style(Role::Text).add_modifier(Modifier::BOLD),
            ),
            Row::Session(i) => {
                let id = self.sessions[i].agent_id.0.as_str();
                (
                    names.cli(&sanitize_line(id)),
                    theme.style(Role::for_cli(id)),
                )
            }
        }
    }

    fn title(&self, row: Row) -> String {
        match row {
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
        }
    }

    fn updated(&self, row: Row) -> OffsetDateTime {
        match row {
            Row::Conversation(i) => self.conversations[i].updated,
            Row::Session(i) => self.sessions[i].updated,
        }
    }

    /// How long a conversation or a session is.
    fn length(&self, row: Row) -> String {
        match row {
            Row::Conversation(i) => {
                let conversation = &self.conversations[i];
                match (conversation.turns, conversation.runs) {
                    (0, runs) => counted(runs, "run"),
                    (turns, _) => counted(turns, "turn"),
                }
            }
            Row::Session(i) => counted(self.sessions[i].turns, "turn"),
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
            let needs_you = matches!(*row, Row::Conversation(c) if self.conversations[c].needs_you);
            let (from, from_style) = self.from(*row, names, theme);
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
                if needs_you {
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
                Span::styled(self.length(*row), dim),
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
            Span::styled(
                "Resume a conversation",
                theme.style(Role::Text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(rest, theme.style(Role::Dim)),
        ])
    }

    /// The keys for the row selected, each with how long it stays as the row gets narrow.
    /// While you type a search, letters are the search's, so only the keys without them show.
    fn keys(&self) -> Vec<(String, u8)> {
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
        let mut keys: Vec<(&str, u8)> = vec![("↑↓  pick", 9)];
        if self.selected_row().is_some() {
            keys.push(("enter continues it", 8));
            if letters {
                keys.push(("b branches from it, the original untouched", 6));
            }
        }
        if letters {
            let folders = if self.every_folder {
                "a this folder"
            } else {
                "a every folder"
            };
            keys.extend([(folders, 5), ("/ search", 7), ("esc", 9)]);
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

    /// Enter: a conversation or a CLI session carries on. A conversation brings back the runs
    /// it started, each drawn after the turn that started it.
    fn enter(&mut self) -> ViewOutcome {
        let resume = |target| {
            ViewOutcome::Send(UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(target),
                },
            })
        };
        match self.selected_row() {
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
            ViewRequest::ResumeList
        })
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
}

/// The words you typed, each to be found in a row, case aside.
fn search_words(filter: &str) -> Vec<String> {
    filter
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
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

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
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
            KeyCode::Char('b') if letters => return self.branch(),
            KeyCode::Char('a') if letters => return self.other_folders(),
            KeyCode::Enter => return self.enter(),
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

    /// A fresh list replaces this one in place, keeping the selection on the same row.
    fn accept(&mut self, data: &ViewData) -> bool {
        let ViewData::ResumeList {
            conversations,
            sessions,
        } = data
        else {
            return false;
        };
        let keep = self.selected_row().map(|row| self.picked(row));
        self.conversations = conversations.clone();
        self.sessions = sessions.clone();
        self.session_ids = short_ids(&self.sessions);
        self.relist(keep);
        true
    }
}

#[cfg(test)]
#[path = "resume_picture_tests.rs"]
mod picture_tests;

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::app::{ConversationSummary, UiCommand, ViewData};
    use argonauts_core::cli_sessions::{CliSession, CliSessionRef};
    use argonauts_core::input::{ResumeTarget, SlashCommand};
    use argonauts_core::names::{AgentId, ConversationId};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use time::macros::datetime;
    use time::{Duration, OffsetDateTime};

    use super::{ResumePicker, Row};
    use crate::state::AppState;
    use crate::theme::Theme;
    use crate::views::{View, ViewCtx, ViewOutcome};

    const WALL: OffsetDateTime = datetime!(2026-09-30 12:00 UTC);

    fn conversation(n: u32, title: &str, hours_ago: i64) -> ConversationSummary {
        ConversationSummary {
            conversation: ConversationId::numbered(n),
            title: title.to_owned(),
            updated: WALL - Duration::hours(hours_ago),
            worker: Some("claude-code".parse().unwrap()),
            turns: 2,
            runs: 0,
            needs_you: false,
        }
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
        let view = ResumePicker::listing(vec![only_runs, two, chatted], Vec::new());
        let lengths: Vec<String> = (0..3).map(|i| view.length(Row::Conversation(i))).collect();
        assert_eq!(lengths, ["1 run", "2 runs", "2 turns"]);
    }

    /// A run is found through the conversation that started it, whose row is marked while the
    /// run waits on you.
    #[test]
    fn a_conversation_whose_run_needs_you_is_marked() {
        let waiting = ConversationSummary {
            needs_you: true,
            ..conversation(1, "fix the median", 1)
        };
        let view = ResumePicker::listing(vec![conversation(2, "hi", 0), waiting], Vec::new());
        let text = rows(&view, 100);
        assert!(!text[1].contains('◆'), "{text:#?}");
        assert!(text[2].starts_with("  ◆ Argonauts Code"), "{text:#?}");
    }

    #[test]
    fn enter_carries_on_a_conversation() {
        let mut view = ResumePicker::listing(
            vec![
                conversation(2, "hi", 0),
                conversation(1, "what does export do?", 3),
            ],
            Vec::new(),
        );
        let ViewOutcome::Send(sent) = press(&mut view, KeyCode::Enter) else {
            panic!("Enter sends nothing");
        };
        assert_eq!(
            sent,
            UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(ResumeTarget::Conversation(ConversationId::numbered(2))),
                },
            }
        );
        let keys = view.key_line(200, &Theme::dark()).to_string();
        assert_eq!(
            keys,
            "↑↓  pick · enter continues it · b branches from it, the original untouched · a every \
             folder · / search · esc"
        );
    }

    #[test]
    fn cli_sessions_list_with_the_conversations() {
        let view = with_sessions();
        let order: Vec<String> = view.rows.iter().map(|row| view.named(*row)).collect();
        // Two ids that start alike show until they are different; another shows its first part.
        assert_eq!(
            order,
            [
                "session claude-code:0a1b2c3d-1",
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
    }

    #[test]
    fn the_title_says_where_and_which_rows_show() {
        let view = with_sessions();
        let theme = Theme::dark();
        assert_eq!(
            view.title_line("~/work/shop", 4, &theme).to_string(),
            "Resume a conversation · in ~/work/shop"
        );
        assert_eq!(
            view.title_line("~/work/shop", 2, &theme).to_string(),
            "Resume a conversation · in ~/work/shop · 1-2 of 4"
        );
    }

    #[test]
    fn a_fresh_list_keeps_the_session_selected() {
        let mut view = with_sessions();
        press(&mut view, KeyCode::Down);
        let data = ViewData::ResumeList {
            conversations: Vec::new(),
            sessions: view.sessions.clone(),
        };
        assert!(view.accept(&data));
        let (picked, _) = view.selected_session().unwrap();
        assert_eq!(picked.id, "0a1b2c3d-2222-4aaa");
    }
}
