//! The crew panel, from Claude Code's agent list and Codex's agent command center: tabs by
//! state with counts, every agent, conversation, and CLI session grouped by repo and then run,
//! the highlighted row's details beside the list on a wide screen, and the quotas on one line.

mod draw;
mod rows;
#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::app::{AnswerTarget, UiCommand};
use argonauts_core::crew_panel::{CrewPanel, CrewRow, CrewState, CrewTab, CrewTarget};
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::SessionKey;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};

use crate::format::WallClock;
use crate::state::{AppState, SessionStatus};
use crate::text::marks;
use crate::theme::{Role, Theme};

use self::draw::BodyLine;
use super::list::{self, ListState};
use super::panes::{self, Footer};
use super::session::SessionView;
use super::{View, ViewCtx, ViewOutcome};

/// From this width the details sit beside the list, as in Codex.
pub const DETAILS_BESIDE: u16 = 120;

/// Rows the list keeps when the details show under it.
const MIN_LIST_ROWS: u16 = 3;

#[derive(Debug)]
pub struct CrewView {
    pub tab: CrewTab,
    /// The highlighted row among the tab's, in the order they show.
    pub list: ListState,
    /// For how long ago; tests pin it.
    pub clock: WallClock,
    /// The highlighted row's target, so it stays highlighted as rows move between groups.
    selected: Option<CrewTarget>,
    /// The details under the list, on a screen too narrow for them beside it (Space).
    pub details: bool,
    /// The keys in place of the list (`?`).
    pub keys: bool,
    confirm_stop: Option<(RunId, SessionKey)>,
    /// The first list line shown.
    scroll: usize,
    page: usize,
    /// The last frame had the details beside the list.
    wide: bool,
    /// Some agent works, so how long ago ticks.
    ticking: bool,
    /// Opened with ← from the input, so → goes back the way it came and Enter opens.
    right_closes: bool,
}

/// The rows a tab shows, in order, from the panel's groups.
fn ordered(panel: &CrewPanel, tab: CrewTab) -> Vec<&CrewRow> {
    panel
        .groups(tab)
        .into_iter()
        .flat_map(|repo| repo.runs.into_iter().flat_map(|run| run.rows))
        .collect()
}

/// An agent that waits on a usage reset shows a timer.
fn sleeping(state: &AppState, row: &CrewRow) -> bool {
    match &row.target {
        CrewTarget::Agent { run, session } => state
            .session(run, session)
            .is_some_and(|session| session.status == SessionStatus::Sleeping),
        _ => false,
    }
}

impl CrewView {
    pub fn new(_state: &AppState) -> Self {
        Self {
            tab: CrewTab::All,
            list: ListState::default(),
            clock: WallClock::local(),
            selected: None,
            details: false,
            keys: false,
            confirm_stop: None,
            scroll: 0,
            page: 1,
            wide: false,
            ticking: false,
            right_closes: false,
        }
    }

    /// The panel as ← from the input opens it.
    pub fn opened_left(state: &AppState) -> Self {
        Self {
            right_closes: true,
            ..Self::new(state)
        }
    }

    fn panel(&self, state: &AppState) -> CrewPanel {
        rows::panel(state, self.clock.now())
    }

    /// Finds the highlighted row again after the rows changed, or keeps the place.
    fn sync(&mut self, rows: &[&CrewRow]) {
        if let Some(at) = self
            .selected
            .as_ref()
            .and_then(|target| rows.iter().position(|row| row.target == *target))
        {
            self.list.selected = at;
        }
        self.list.keep_visible(rows.len(), self.page);
        self.selected = rows.get(self.list.selected).map(|row| row.target.clone());
    }

    fn switch_tab(&mut self, tab: CrewTab) -> ViewOutcome {
        self.tab = tab;
        self.list = ListState::default();
        self.selected = None;
        self.scroll = 0;
        ViewOutcome::Consumed
    }

    fn footer(&self, state: &AppState) -> Footer {
        if let Some((run, session)) = &self.confirm_stop {
            let name = state.session(run, session).map_or_else(
                || rows::clean(session.as_str()),
                |s| rows::clean(&state.names.worker(&s.worker)),
            );
            let again = format!("x again to stop {name}");
            return Footer::hints(&[(&again, 0), ("esc to keep", 0)]);
        }
        if self.keys {
            return Footer::hints(&[("? hide keys", 1), (panes::ESC_BACK, 0)]);
        }
        let open = if self.right_closes {
            "enter open"
        } else {
            "→ open"
        };
        let mut parts = vec![("↑↓ move", 0), (open, 5)];
        if !self.wide {
            parts.push(("Space details", 2));
        }
        parts.extend([
            ("Tab filter", 4),
            ("x stop", 3),
            ("? keys", 1),
            (panes::ESC_BACK, 0),
        ]);
        Footer::hints(&parts)
    }

    /// Enter or → on a row.
    fn open(&self, state: &AppState, panel: &CrewPanel, row: &CrewRow) -> ViewOutcome {
        let resume = |target: ResumeTarget| {
            ViewOutcome::SendToChat(UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(target),
                },
            })
        };
        match &row.target {
            CrewTarget::Agent { run, session } => {
                let asks = state
                    .runs
                    .get(run)
                    .and_then(|held| rows::asked_by(held, session));
                match asks {
                    Some(open) => ViewOutcome::AnswerCard(AnswerTarget::Checkpoint {
                        run: run.clone(),
                        id: open.opened.id.clone(),
                    }),
                    None if state.session(run, session).is_some() => ViewOutcome::Open(Box::new(
                        SessionView::from_crew(run.clone(), session.clone()),
                    )),
                    None => ViewOutcome::Notice(format!(
                        "{} isn't on this screen. /resume {run} brings it here",
                        rows::clean(session.as_str())
                    )),
                }
            }
            CrewTarget::Checkpoint { run, checkpoint } => {
                ViewOutcome::AnswerCard(AnswerTarget::Checkpoint {
                    run: run.clone(),
                    id: checkpoint.clone(),
                })
            }
            CrewTarget::Run { run } if state.attached.as_ref() == Some(run) => {
                ViewOutcome::CloseAll
            }
            CrewTarget::Run { run } if row.repo == panel.repo => {
                ViewOutcome::SendToChat(UiCommand::Resume {
                    run: Some(run.clone()),
                })
            }
            CrewTarget::Run { run } => ViewOutcome::Notice(format!(
                "{} runs in {}. Open argonauts there to work on it",
                rows::clean(run.as_str()),
                rows::clean(&row.repo)
            )),
            CrewTarget::Conversation { chat } => resume(ResumeTarget::Conversation(chat.clone())),
            CrewTarget::VendorSession { session } => {
                resume(ResumeTarget::VendorSession(session.clone()))
            }
        }
    }

    /// `x` asks first, and a second `x` stops that agent.
    fn stop(&mut self, state: &AppState, row: &CrewRow) -> ViewOutcome {
        let (run, session) = match &row.target {
            CrewTarget::Agent { run, session } => (run, session),
            CrewTarget::Checkpoint { .. } => {
                return ViewOutcome::Notice("This card has no agent to stop".into());
            }
            CrewTarget::Run { .. } => {
                return ViewOutcome::Notice("/stop stops the run on screen".into());
            }
            CrewTarget::Conversation { .. } => {
                return ViewOutcome::Notice("Esc in the chat stops a conversation's turn".into());
            }
            CrewTarget::VendorSession { .. } => {
                return ViewOutcome::Notice("A CLI session has nothing running to stop".into());
            }
        };
        let Some(agent) = state.session(run, session) else {
            return ViewOutcome::Consumed;
        };
        if !agent.is_running() {
            return ViewOutcome::Notice(format!(
                "{} already finished",
                rows::clean(&state.names.worker(&agent.worker))
            ));
        }
        let target = (run.clone(), session.clone());
        if self.confirm_stop.as_ref() != Some(&target) {
            self.confirm_stop = Some(target);
            return ViewOutcome::Consumed;
        }
        self.confirm_stop = None;
        let (run, session) = target;
        ViewOutcome::Send(UiCommand::StopAgent { run, session })
    }

    /// Keeps the highlighted row on screen, with the repo and run headings above it when it
    /// starts their group.
    fn follow_selection(&mut self, body: &[BodyLine], height: usize) {
        let height = height.max(1);
        let Some(at) = body
            .iter()
            .position(|line| line.row == Some(self.list.selected))
        else {
            self.scroll = self.scroll.min(body.len().saturating_sub(height));
            return;
        };
        let mut start = at;
        while start > 0
            && at - start < 2
            && body[start - 1].row.is_none()
            && !body[start - 1].line.spans.is_empty()
        {
            start -= 1;
        }
        if start < self.scroll {
            self.scroll = start;
        } else if at + 1 > self.scroll + height {
            self.scroll = at + 1 - height;
        }
        self.scroll = self.scroll.min(body.len().saturating_sub(height));
    }

    fn draw_list(
        &mut self,
        state: &AppState,
        rows: &[&CrewRow],
        panel: &CrewPanel,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
    ) {
        let width = usize::from(area.width);
        let dim = theme.style(Role::Dim);
        let mut body = if rows.is_empty() {
            vec![BodyLine {
                line: Line::from(Span::styled(empty_text(self.tab), dim)),
                row: None,
            }]
        } else {
            let sleeping: Vec<bool> = rows.iter().map(|row| sleeping(state, row)).collect();
            draw::list_lines(
                &panel.groups(self.tab),
                &sleeping,
                self.list.selected,
                width,
                self.clock.now(),
                &state.names,
                theme,
            )
        };
        if state.crew_panel.is_none() {
            body.push(BodyLine {
                line: Line::default(),
                row: None,
            });
            body.push(BodyLine {
                line: Line::from(Span::styled("Listing conversations and CLI sessions…", dim)),
                row: None,
            });
        }
        let height = usize::from(area.height);
        self.page = height.max(1);
        self.follow_selection(&body, height);
        let lines: Vec<Line<'static>> = body
            .into_iter()
            .skip(self.scroll)
            .map(|line| line.line)
            .collect();
        panes::put_lines(buf, area, &lines);
    }
}

fn empty_text(tab: CrewTab) -> &'static str {
    match tab {
        CrewTab::NeedsYou => "Nothing needs you",
        CrewTab::Working => "No agent is working",
        CrewTab::Ready => "Nothing waits for you to carry it on",
        CrewTab::Done => "Nothing has finished yet",
        CrewTab::All => "No agents, conversations, or CLI sessions yet",
    }
}

/// A divider down `area`'s one column, `|` in ASCII.
fn divider(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let glyph = if theme.glyphs.prompt.is_ascii() {
        "|"
    } else {
        "│"
    };
    for y in area.top()..area.bottom() {
        if let Some(cell) = buf.cell_mut((area.x, y)) {
            cell.set_symbol(glyph);
            cell.set_style(marks::decoration(theme.style(Role::Rule)));
        }
    }
}

/// The details pane's width beside the list.
fn details_width(width: u16) -> u16 {
    (width * 3 / 10).clamp(34, 56)
}

impl View for CrewView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let state = ctx.state;
        let panel = self.panel(state);
        let rows = ordered(&panel, self.tab);
        self.sync(&rows);
        self.ticking = panel.rows.iter().any(|row| row.state == CrewState::Working);
        self.wide = area.width >= DETAILS_BESIDE;
        let width = usize::from(area.width);

        let [title, tabs, _gap, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        let line = draw::title_line(state, width, theme);
        panes::put(buf, title.x, title.y, title.width, &line);
        let line = draw::tabs_line(&panel, self.tab, width, theme);
        panes::put(buf, tabs.x, tabs.y, tabs.width, &line);

        if self.keys {
            let lines =
                draw::keys_lines(self.wide, self.right_closes, list::vim(state), width, theme);
            panes::put_lines(buf, body, &lines);
        } else {
            let row = rows.get(self.list.selected).copied();
            let details = |width: u16| {
                row.map(|row| {
                    let sleeping = sleeping(state, row);
                    draw::details_lines(row, sleeping, usize::from(width), &state.names, theme)
                })
            };
            if self.wide {
                let [list_area, _, rule, _, details_area] = Layout::horizontal([
                    Constraint::Min(0),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(details_width(area.width)),
                ])
                .areas(body);
                self.draw_list(state, &rows, &panel, list_area, buf, theme);
                divider(buf, rule, theme);
                if let Some(lines) = details(details_area.width) {
                    panes::put_lines(buf, details_area, &lines);
                }
            } else {
                let lines = if self.details {
                    details(body.width).unwrap_or_default()
                } else {
                    Vec::new()
                };
                let room = body.height.saturating_sub(MIN_LIST_ROWS + 1);
                let shown = (lines.len() as u16).min((body.height * 3 / 5).min(room));
                if shown == 0 {
                    self.draw_list(state, &rows, &panel, body, buf, theme);
                } else {
                    let [list_area, rule, details_area] = Layout::vertical([
                        Constraint::Min(0),
                        Constraint::Length(1),
                        Constraint::Length(shown),
                    ])
                    .areas(body);
                    self.draw_list(state, &rows, &panel, list_area, buf, theme);
                    let rule_line = Line::from(marks::decorative(
                        theme.glyphs.rule.repeat(usize::from(rule.width)),
                        theme.style(Role::Rule),
                    ));
                    panes::put(buf, rule.x, rule.y, rule.width, &rule_line);
                    panes::put_lines(buf, details_area, &lines);
                }
            }
        }
        panes::draw_footer(buf, footer, self.footer(state), "", theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let state = ctx.state;
        // Ctrl and Alt letters are global keys, such as Ctrl+X.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char(_))
        {
            return ViewOutcome::Ignored;
        }
        let key = match key.code {
            // Nothing here takes typing, so j and k move with vim keys off too.
            KeyCode::Char('j') => KeyEvent::from(KeyCode::Down),
            KeyCode::Char('k') => KeyEvent::from(KeyCode::Up),
            _ => list::standard(key, list::vim(state), KeyCode::Esc),
        };
        // Any other key keeps the agent; Esc only does that.
        if key.code != KeyCode::Char('x')
            && self.confirm_stop.take().is_some()
            && key.code == KeyCode::Esc
        {
            return ViewOutcome::Consumed;
        }
        let back = matches!(key.code, KeyCode::Esc | KeyCode::Left)
            || (self.right_closes && key.code == KeyCode::Right);
        if self.keys && (back || key.code == KeyCode::Char('?')) {
            self.keys = false;
            return ViewOutcome::Consumed;
        }
        self.keys = false;
        let panel = self.panel(state);
        let rows = ordered(&panel, self.tab);
        self.sync(&rows);
        let row = rows.get(self.list.selected).copied();
        match key.code {
            _ if back && self.details && !self.wide => {
                self.details = false;
                ViewOutcome::Consumed
            }
            _ if back => ViewOutcome::Close,
            KeyCode::Char('?') => {
                self.keys = true;
                ViewOutcome::Consumed
            }
            KeyCode::Tab => self.switch_tab(self.tab.next()),
            KeyCode::BackTab => self.switch_tab(self.tab.previous()),
            // A wide screen shows the details beside the list anyway.
            KeyCode::Char(' ') => {
                self.details = !self.details;
                ViewOutcome::Consumed
            }
            KeyCode::Enter | KeyCode::Right => match row {
                Some(row) => self.open(state, &panel, row),
                None => ViewOutcome::Consumed,
            },
            KeyCode::Char('x') => match row {
                Some(row) => self.stop(state, row),
                None => ViewOutcome::Consumed,
            },
            _ => match list::nav_key(&key, self.page) {
                Some(nav) => {
                    self.list.apply(nav, rows.len(), self.page);
                    self.selected = rows.get(self.list.selected).map(|row| row.target.clone());
                    ViewOutcome::Consumed
                }
                None => ViewOutcome::Ignored,
            },
        }
    }

    fn next_change(&self, now: Instant) -> Option<Instant> {
        self.ticking.then(|| now + Duration::from_secs(1))
    }
}
