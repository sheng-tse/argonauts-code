//! The agent view, in the workflow view's frame: the phase's agents on the left, and on the
//! right the selected agent's Prompt, Activity, Diff, Subagents, and Outcome, each expandable.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::api::DiffScope;
use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use argonauts_core::names::SessionKey;
use argonauts_core::settings::ui::VendorInternals;
use argonauts_core::views::{
    AgentRow, AgentView as AgentData, PhaseRow, RowStatus, StatusLine, WorkflowRow,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::format;
use crate::markdown::{Indent, render_markdown};
use crate::render::{Clock, RenderCtx};
use crate::text::{KEEP, Part, fit_parts, sanitize, sanitize_line, str_width, wrap_text};
use crate::theme::{Role, Theme};

use super::dock::{self, Heading};
use super::follow::{Follower, Shown};
use super::layout::{cut, left};
use super::list;
use super::panes::{self, Footer, Marks};
use super::workflow::agents_in_phase;
use super::{View, ViewCtx, ViewOutcome};

/// Lines a section shows until it's expanded.
const PROMPT_PREVIEW: usize = 2;
const ACTIVITY_PREVIEW: usize = 3;
const SUBAGENT_PREVIEW: usize = 3;
const OUTCOME_PREVIEW: usize = 6;

/// The left pane is never wider than this, or than three tenths of the screen, so the sections
/// keep most of it.
const MAX_LEFT: usize = 64;

/// Longer subagent names are cut, so their states still line up.
const MAX_CHILD_LABEL: usize = 24;

/// Steering text keeps at least this many cells before its key hints give way.
const MIN_STEER: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Prompt,
    Activity,
    Diff,
    Subagents,
    Outcome,
}

#[derive(Debug)]
pub struct AgentView {
    pub data: AgentData,
    /// The phase's name and agent count for the left pane's title. `None` opened on its own,
    /// with no left pane.
    phase: Option<(String, usize)>,
    siblings: Vec<AgentRow>,
    selected: usize,
    scroll: usize,
    expanded: Vec<Section>,
    /// Which of the sections that expand Enter acts on; Tab moves it.
    cursor: usize,
    steer: Option<String>,
    confirm_stop: bool,
    /// When the data was first drawn, so a running agent's time keeps counting.
    seen_at: Option<Instant>,
    /// Where each section starts and how much shows, from the last frame, for the keys.
    drawn: Drawn,
    /// Set when the view was opened on its own; in a workflow's frame the workflow follows.
    follow: Option<Follower>,
}

#[derive(Debug, Default, Clone)]
struct Drawn {
    sections: Vec<(Section, usize)>,
    total: usize,
    page: usize,
    /// The right pane's width, so keys can lay the sections out again between frames.
    width: u16,
    /// Rows the Outcome renders to at that width, so it expands only when some are hidden.
    outcome_rows: usize,
}

impl AgentView {
    pub fn new(data: AgentData) -> Self {
        let siblings = vec![data.row.clone()];
        Self {
            data,
            phase: None,
            siblings,
            selected: 0,
            scroll: 0,
            expanded: Vec::new(),
            cursor: 0,
            steer: None,
            confirm_stop: false,
            seen_at: None,
            drawn: Drawn::default(),
            follow: None,
        }
    }

    /// Keeps the view current with its run's events while it's on top.
    pub fn following(mut self, follower: Follower) -> Self {
        self.follow = Some(follower);
        self
    }

    /// Opened from a workflow's phase: the phase's agents show on the left, ↑↓ switches them.
    /// A nested workflow's row opens its own view, so it isn't listed, but its agents count.
    pub fn in_phase(data: AgentData, phase: &PhaseRow) -> Self {
        let siblings: Vec<AgentRow> = phase
            .rows
            .iter()
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some(agent.clone()),
                WorkflowRow::Workflow(_) | WorkflowRow::Command(_) => None,
            })
            .collect();
        let mut view = Self::new(data);
        if let Some(index) = siblings
            .iter()
            .position(|row| row.session == view.data.row.session)
        {
            view.siblings = siblings;
            // The phase's row can be older than the agent just loaded.
            view.siblings[index] = view.data.row.clone();
            view.selected = index;
        }
        view.phase = Some((phase.name.clone(), agents_in_phase(phase)));
        view
    }

    /// The agent the left pane has selected, which may still be loading.
    pub fn session(&self) -> &SessionKey {
        &self.siblings[self.selected].session
    }

    /// Takes a fresh answer for one of its agents. False for any other agent.
    pub fn show(&mut self, data: AgentData) -> bool {
        let Some(index) = self
            .siblings
            .iter()
            .position(|row| row.session == data.row.session)
        else {
            return false;
        };
        if data.row.session != self.data.row.session {
            self.scroll = 0;
            self.cursor = 0;
            self.expanded.clear();
        }
        self.siblings[index] = data.row.clone();
        self.data = data;
        self.seen_at = None;
        true
    }

    fn loaded(&self) -> bool {
        self.data.row.session == *self.session()
    }

    /// Draws the view in the docked frame under `heading`, the workflow's own when it's shown
    /// in a workflow's frame: the phase's agents on the left, as Claude Code lists them, and
    /// this agent's detail on the right, with its position on the box's bottom edge.
    pub fn render_frame(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &ViewCtx<'_>,
        heading: &Heading,
        status: &[StatusLine],
    ) {
        let theme = ctx.theme;
        let seen_at = *self.seen_at.get_or_insert(ctx.now);
        let marks = Marks::for_theme(theme);
        let phase_title = match &self.phase {
            Some((phase, agents)) => format!(
                "{} · {}",
                sanitize_line(phase).trim(),
                panes::count(*agents, "agent")
            ),
            None => String::new(),
        };
        // A label two agents share reads with the agent's name after it.
        let names: Vec<(String, bool)> = sibling_names(&self.siblings, &ctx.state.names)
            .into_iter()
            .zip(&self.siblings)
            .map(|((label, repeated), row)| {
                if repeated {
                    let agent = sanitize_line(&ctx.state.names.worker(&row.worker)).into_owned();
                    (format!("{label}  {agent}"), true)
                } else {
                    (label, false)
                }
            })
            .collect();
        let left_width = if self.phase.is_some() {
            let most = MAX_LEFT.min(usize::from(area.width) * 3 / 10);
            let longest = names
                .iter()
                .map(|(label, _)| str_width(label))
                .max()
                .unwrap_or(0);
            (lead(marks.slot()) + longest)
                .max(str_width(&phase_title))
                .min(most) as u16
        } else {
            0
        };
        let frame = dock::split(area, left_width, status.len() as u16);
        dock::draw_rule(buf, &frame, theme);
        dock::draw_heading(buf, &frame, heading, theme);
        dock::draw_status(buf, &frame, status, theme);
        let panes = frame.panes;
        let label = sanitize_line(&self.siblings[self.selected].label)
            .trim()
            .to_owned();
        if panes.left.width > 0 {
            let rows = self.sibling_lines(&names, panes.left, theme);
            panes::put_lines(buf, panes.left, &rows);
        }

        let lines = self.detail_lines(panes.right.width, ctx, seen_at);
        let page = usize::from(panes.right.height);
        self.scroll = self.scroll.min(lines.len().saturating_sub(page));
        self.drawn.total = lines.len();
        self.drawn.page = page;
        self.drawn.width = panes.right.width;
        let shown = &lines[self.scroll.min(lines.len())..];
        let mark = dock::position(self.scroll, page, lines.len(), theme);
        dock::draw_box(buf, &frame, [&phase_title, &label], &mark, theme);
        panes::put_lines(buf, panes.right, shown);
        let footer = self.footer(usize::from(panes.footer.width), ctx);
        dock::draw_footer(buf, &frame, footer, theme);
    }

    /// The keys that act here and now, as Claude Code's: `↑↓ agent · j/k scroll · ⏎expand ·
    /// p pause · esc back · s save`. Ours join them only when they act: steering and stopping
    /// a running agent, and taking over a card. Lower numbers give way first.
    fn footer(&self, width: usize, ctx: &ViewCtx<'_>) -> Footer {
        if let Some(text) = &self.steer {
            return Footer::Line(steer_line(text, width, ctx.theme));
        }
        if self.confirm_stop {
            let label = sanitize_line(&self.data.row.label).trim().to_owned();
            let ask = format!("x again to stop {label}");
            return Footer::hints(&[(&ask, 0), ("esc to keep", 0)]);
        }
        let internals = vendor_internals(ctx);
        let enter = match self.current_section(internals) {
            Some(Section::Diff) => Some("⏎ open"),
            Some(section) if self.expanded.contains(&section) => Some("⏎ collapse"),
            Some(_) => Some("⏎ expand"),
            None => None,
        };
        let running = matches!(
            self.data.row.status,
            RowStatus::Running | RowStatus::NeedsYou | RowStatus::Waiting
        );
        let run_going = super::run_going(ctx.state, &self.data.run);
        let has_card = ctx
            .state
            .session(&self.data.run, &self.data.row.session)
            .is_some_and(|session| session.card.is_some());
        let mut hints = vec![("↑↓ agent", 0), ("j/k scroll", 6)];
        hints.extend(enter.map(|enter| (enter, 7)));
        if running {
            hints.push(("i steer", 5));
        }
        if run_going && has_card {
            hints.push(("t take over", 2));
        }
        if running {
            hints.push(("x stop", 4));
        }
        if run_going {
            hints.push(("p pause", 3));
        }
        hints.push((panes::ESC_BACK, 0));
        if self.phase.is_some() {
            hints.push(("s save", 1));
        }
        Footer::hints(&hints)
    }

    /// Each agent of the phase as Claude Code lists them beside an agent's detail: the cursor,
    /// the state glyph, and the label.
    fn sibling_lines(
        &self,
        names: &[(String, bool)],
        area: Rect,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let slot = marks.slot();
        let width = usize::from(area.width);
        let accent = theme.style(Role::Card);
        let height = usize::from(area.height);
        let skip = (self.selected + 1).saturating_sub(height);
        self.siblings
            .iter()
            .zip(names)
            .enumerate()
            .skip(skip)
            .map(|(i, (row, (label, _)))| {
                let (glyph, role) = row_glyph(&marks, row.status);
                let selected = i == self.selected;
                let label_style = if selected {
                    accent
                } else if matches!(
                    row.status,
                    RowStatus::Done | RowStatus::Failed | RowStatus::NeedsYou
                ) {
                    theme.style(Role::Text)
                } else {
                    theme.style(Role::Dim)
                };
                let spans = vec![
                    if selected {
                        Span::styled(marks.selected.to_owned(), accent)
                    } else {
                        Span::raw(" ")
                    },
                    Span::styled(panes::pad_glyph(glyph, slot), theme.style(role)),
                    Span::raw(" "),
                    Span::styled(cut(label, width.saturating_sub(lead(slot))), label_style),
                ];
                Line::from(spans)
            })
            .collect()
    }

    /// Everything in the right pane, top down, before scrolling.
    fn detail_lines(
        &mut self,
        width: u16,
        ctx: &ViewCtx<'_>,
        seen_at: Instant,
    ) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let row = &self.siblings[self.selected];
        let mut lines = vec![
            status_line(row, width, ctx, seen_at, self.loaded()),
            identity_line(row, &self.data.run, width, ctx),
        ];
        self.drawn.sections.clear();
        if !self.loaded() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("Loading…", theme.style(Role::Dim))));
            return lines;
        }
        let internals = vendor_internals(ctx);
        let mut outcome = self.outcome_rows(width, ctx);
        self.drawn.outcome_rows = outcome.as_ref().map_or(0, Vec::len);
        let current = self.current_section(internals);
        let blocks = [
            Section::Prompt,
            Section::Activity,
            Section::Diff,
            Section::Subagents,
            Section::Outcome,
        ];
        for section in blocks {
            let open = self.expanded.contains(&section);
            let body = match section {
                Section::Prompt => self.prompt(width, open, theme),
                Section::Activity => self.activity(width, open, theme),
                Section::Diff => self.diff(),
                Section::Subagents => self.subagents(width, open, internals, theme),
                Section::Outcome => self.outcome(outcome.take(), open, theme),
            };
            let Some((title, expandable, body)) = body else {
                continue;
            };
            let hint = match section {
                _ if !expandable => None,
                Section::Diff => Some("open"),
                _ if open => Some("collapse"),
                _ => Some("expand"),
            };
            lines.push(Line::default());
            self.drawn.sections.push((section, lines.len()));
            lines.push(section_title(title, hint, current == Some(section), theme));
            lines.extend(body);
        }
        lines
    }

    /// The sections that expand, in order.
    fn expandable(&self, internals: VendorInternals) -> Vec<Section> {
        let prompt = self.data.prompt.lines().count() > PROMPT_PREVIEW;
        let activity = self.data.activity.len() > ACTIVITY_PREVIEW;
        let subagents = internals == VendorInternals::Discovered
            && self
                .data
                .subagents
                .as_ref()
                .is_some_and(|section| section.children.len() > SUBAGENT_PREVIEW);
        let diff = self.data.diff.is_some();
        let outcome = self.drawn.outcome_rows > OUTCOME_PREVIEW;
        [
            (Section::Prompt, prompt),
            (Section::Activity, activity),
            (Section::Diff, diff),
            (Section::Subagents, subagents),
            (Section::Outcome, outcome),
        ]
        .into_iter()
        .filter_map(|(section, expands)| expands.then_some(section))
        .collect()
    }

    /// The section Enter expands.
    fn current_section(&self, internals: VendorInternals) -> Option<Section> {
        let sections = self.expandable(internals);
        sections
            .get(self.cursor.min(sections.len().saturating_sub(1)))
            .copied()
    }

    /// Lays the sections out again at the last frame's width, so keys between frames act on
    /// what the next frame shows.
    fn relayout(&mut self, ctx: &ViewCtx<'_>) {
        if self.drawn.width == 0 {
            return;
        }
        let seen_at = self.seen_at.unwrap_or(ctx.now);
        self.drawn.total = self.detail_lines(self.drawn.width, ctx, seen_at).len();
    }

    /// Scrolls so the section's title shows.
    fn reveal(&mut self, section: Section) {
        let Some(&(_, start)) = self.drawn.sections.iter().find(|(s, _)| *s == section) else {
            return;
        };
        let page = self.drawn.page.max(1);
        if start < self.scroll {
            self.scroll = start;
        } else if start >= self.scroll + page {
            self.scroll = start + 1 - page;
        }
    }

    fn prompt(&self, width: u16, open: bool, theme: &Theme) -> Option<Block> {
        let text = sanitize(&self.data.prompt);
        let all: Vec<&str> = text.lines().collect();
        let title = format!("Prompt · {}", panes::count(all.len(), "line"));
        let inner = usize::from(width).saturating_sub(2);
        let style = theme.style(Role::Text);
        let indent = [Span::raw("  ")];
        let mut body = Vec::new();
        if open {
            for line in &all {
                body.extend(wrap_text(line, style, usize::from(width), &indent, &indent));
            }
        } else {
            // The first lines with text: a blank line between parts says nothing in a preview.
            let shown: Vec<usize> = (0..all.len())
                .filter(|at| !all[*at].trim().is_empty())
                .take(PROMPT_PREVIEW)
                .collect();
            for at in &shown {
                body.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(cut(all[*at], inner), style),
                ]));
            }
            let after = shown.last().map_or(0, |last| last + 1);
            if all.len() > after {
                body.push(Line::from(Span::styled(
                    more_lines(all.len() - after),
                    theme.style(Role::Dim),
                )));
            }
        }
        Some((title, all.len() > PROMPT_PREVIEW, body))
    }

    fn activity(&self, width: u16, open: bool, theme: &Theme) -> Option<Block> {
        let activity = &self.data.activity;
        let calls = self.data.tool_calls.max(activity.len() as u32);
        if activity.is_empty() {
            let title = match calls {
                0 if matches!(self.data.row.status, RowStatus::Done | RowStatus::Failed) => {
                    "Activity · no tool calls".to_owned()
                }
                0 => "Activity · no tool calls yet".to_owned(),
                calls => format!("Activity · {}", panes::count(calls as usize, "tool call")),
            };
            return Some((title, false, Vec::new()));
        }
        let shown = if open {
            activity.len()
        } else {
            activity.len().min(ACTIVITY_PREVIEW)
        };
        let title = if shown as u32 >= calls {
            format!("Activity · {}", panes::count(shown, "tool call"))
        } else {
            format!(
                "Activity · last {shown} of {}",
                panes::count(calls as usize, "tool call")
            )
        };
        let marks = Marks::for_theme(theme);
        let body = activity[activity.len() - shown..]
            .iter()
            .map(|call| {
                let role = match call.ok {
                    Some(true) => Role::Success,
                    Some(false) => Role::Error,
                    None => Role::Dim,
                };
                let tool = sanitize_line(&call.tool).trim().to_owned();
                let summary = sanitize_line(&call.summary).trim().to_owned();
                // The call keeps its closing parenthesis when its summary is cut.
                let room = usize::from(width).saturating_sub(4 + str_width(&tool) + 2);
                let text = if summary.is_empty() {
                    tool
                } else {
                    format!("{tool}({})", cut(&summary, room))
                };
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(marks.running, theme.style(role)),
                    Span::raw(" "),
                    Span::styled(text, theme.style(Role::Text)),
                ])
            })
            .collect();
        Some((title, activity.len() > ACTIVITY_PREVIEW, body))
    }

    fn diff(&self) -> Option<Block> {
        let diff = self.data.diff?;
        let title = format!(
            "Diff · {} · +{} -{}",
            panes::count(diff.files_changed as usize, "file"),
            diff.insertions,
            diff.deletions
        );
        Some((title, true, Vec::new()))
    }

    /// `d`, or Enter on the Diff section: the card's diff in the pager.
    fn open_diff(&self, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let card = ctx
            .state
            .session(&self.data.run, &self.data.row.session)
            .and_then(|session| session.card.clone());
        match card {
            Some(card) => ViewOutcome::Request(ViewRequest::Diff {
                run: self.data.run.clone(),
                card,
                scope: DiffScope::Card,
            }),
            None => ViewOutcome::Notice("This agent has no card, so no diff".to_owned()),
        }
    }

    fn subagents(
        &self,
        width: u16,
        open: bool,
        internals: VendorInternals,
        theme: &Theme,
    ) -> Option<Block> {
        let section = self.data.subagents.as_ref()?;
        let children = &section.children;
        let name = sanitize_line(&section.title).trim().to_owned();
        let note = sanitize_line(&section.note).trim().to_owned();
        let mut title = format!("{name} · {}", children.len());
        if !note.is_empty() {
            title.push_str(&format!(" · {note}"));
        }
        match internals {
            VendorInternals::Hidden => None,
            VendorInternals::Summary => {
                let done = children
                    .iter()
                    .filter(|child| child.status == SubagentStatus::Done)
                    .count();
                let running = children
                    .iter()
                    .filter(|child| {
                        matches!(
                            child.status,
                            SubagentStatus::Started | SubagentStatus::Running
                        )
                    })
                    .count();
                let failed = children
                    .iter()
                    .filter(|child| child.status == SubagentStatus::Failed)
                    .count();
                let mut counts = Vec::new();
                for (n, word) in [(done, "done"), (running, "running"), (failed, "failed")] {
                    if n > 0 {
                        counts.push(format!("{n} {word}"));
                    }
                }
                if !counts.is_empty() {
                    title.push_str(&format!(" · {}", counts.join(", ")));
                }
                Some((title, false, Vec::new()))
            }
            VendorInternals::Discovered => {
                let shown = if open {
                    children.len()
                } else {
                    children.len().min(SUBAGENT_PREVIEW)
                };
                let labels: Vec<String> = children.iter().map(child_label).collect();
                let label_width = labels
                    .iter()
                    .map(|label| str_width(label))
                    .max()
                    .unwrap_or(0)
                    .min(MAX_CHILD_LABEL);
                let mut body: Vec<Line<'static>> = children[..shown]
                    .iter()
                    .zip(&labels)
                    .map(|(child, label)| subagent_line(child, label, label_width, width, theme))
                    .collect();
                if shown < children.len() {
                    let more = format!("  …{} more", children.len() - shown);
                    body.push(Line::from(Span::styled(more, theme.style(Role::Dim))));
                }
                Some((title, children.len() > SUBAGENT_PREVIEW, body))
            }
        }
    }

    fn outcome_rows(&self, width: u16, ctx: &ViewCtx<'_>) -> Option<Vec<Line<'static>>> {
        let source = self.data.outcome.as_deref()?;
        let render_ctx = RenderCtx::new(width, ctx.theme, Clock::still(ctx.now));
        Some(render_markdown(source, &render_ctx, &Indent::spaces(2)))
    }

    /// Expands only when the preview hides some of it.
    fn outcome(
        &self,
        rendered: Option<Vec<Line<'static>>>,
        open: bool,
        theme: &Theme,
    ) -> Option<Block> {
        let rendered = rendered?;
        // Counted as drawn, so a long paragraph isn't `1 line` over five rows.
        let title = format!("Outcome · {}", panes::count(rendered.len(), "line"));
        let expands = rendered.len() > OUTCOME_PREVIEW;
        if open || !expands {
            return Some((title, expands, rendered));
        }
        let hidden = rendered.len() - OUTCOME_PREVIEW;
        let mut body: Vec<Line<'static>> = rendered.into_iter().take(OUTCOME_PREVIEW).collect();
        let more = more_lines(hidden);
        body.push(Line::from(Span::styled(more, theme.style(Role::Dim))));
        Some((title, true, body))
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.drawn.total.saturating_sub(self.drawn.page);
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    fn steer_key(&mut self, key: KeyEvent) -> ViewOutcome {
        let Some(text) = self.steer.as_mut() else {
            return ViewOutcome::Ignored;
        };
        match key.code {
            KeyCode::Esc => self.steer = None,
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Enter => {
                let text = text.trim().to_owned();
                self.steer = None;
                if !text.is_empty() {
                    return ViewOutcome::Send(UiCommand::Steer {
                        run: self.data.run.clone(),
                        session: self.data.row.session.clone(),
                        text,
                    });
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control() =>
            {
                text.push(c);
            }
            _ => {}
        }
        ViewOutcome::Consumed
    }
}

/// A section's title, whether Enter acts on it, and its body.
type Block = (String, bool, Vec<Line<'static>>);

/// `hint` is what Enter does there, such as `expand`, and `None` when it does nothing.
/// `  …3 more lines` under a section's preview.
fn more_lines(count: usize) -> String {
    let noun = if count == 1 { "line" } else { "lines" };
    format!("  …{count} more {noun}")
}

fn section_title(title: String, hint: Option<&str>, current: bool, theme: &Theme) -> Line<'static> {
    let dim = theme.style(Role::Dim);
    let title_style = if current {
        theme.style(Role::Text).add_modifier(Modifier::BOLD)
    } else {
        dim
    };
    let mut spans = vec![Span::styled(title, title_style)];
    if let Some(word) = hint {
        let marks = Marks::for_theme(theme);
        spans.push(Span::styled(format!(" · {}{word}", marks.expand), dim));
    }
    Line::from(spans)
}

/// `steer > text▏  ⏎ send · esc cancel`, keeping the end of what you type in view. The hints
/// go first when the line is short.
fn steer_line(text: &str, width: usize, theme: &Theme) -> Line<'static> {
    let dim = theme.style(Role::Dim);
    let marks = Marks::for_theme(theme);
    let prompt = format!("{} ", theme.glyphs.prompt);
    let hints = panes::key_names("  ⏎ send · esc cancel", theme);
    let lead = str_width("steer ") + str_width(&prompt) + str_width(marks.cursor);
    let with_hints = width.saturating_sub(lead + str_width(&hints));
    let text = sanitize_line(text);
    let (room, hints) = if with_hints >= str_width(&text) || with_hints >= MIN_STEER {
        (with_hints, hints)
    } else {
        (width.saturating_sub(lead), String::new())
    };
    Line::from(vec![
        Span::styled("steer ", theme.style(Role::Label)),
        Span::styled(prompt, theme.style(Role::Card)),
        Span::raw(list::tail(&text, room)),
        Span::styled(marks.cursor, dim),
        Span::styled(hints, dim),
    ])
}

/// A child's name, or its kind when the vendor gave none.
fn child_label(child: &SubagentUpdate) -> String {
    let kind = match child.kind {
        SubagentKind::Subagent => "subagent",
        SubagentKind::Workflow => "workflow",
        SubagentKind::Thread => "thread",
    };
    child
        .label
        .as_deref()
        .map(|label| sanitize_line(label).trim().to_owned())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| format!("{kind} {}", sanitize_line(&child.id).trim()))
}

/// `✔ explorer  done · Found 14 call sites`, with labels padded so the words line up.
fn subagent_line(
    child: &SubagentUpdate,
    label: &str,
    label_width: usize,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let marks = Marks::for_theme(theme);
    let (glyph, word, role) = match child.status {
        SubagentStatus::Started | SubagentStatus::Running => (marks.running, "running", Role::Dim),
        SubagentStatus::Done => (marks.done, "done", Role::Success),
        SubagentStatus::Failed => (marks.failed, "failed", Role::Error),
        SubagentStatus::Unknown => (marks.waiting, "unknown", Role::Dim),
    };
    let dim = theme.style(Role::Dim);
    let mut parts = vec![Part::new(vec![Span::styled(word, theme.style(role))], KEEP)];
    if let Some(message) = &child.last_message {
        let message = sanitize_line(message).trim().to_owned();
        if !message.is_empty() {
            parts.push(Part::joined(
                Span::styled(" · ", dim),
                vec![Span::styled(message, dim)],
                0,
            ));
        }
    }
    let lead = vec![
        Span::raw("  "),
        Span::styled(panes::pad_glyph(glyph, marks.slot()), theme.style(role)),
        Span::raw(" "),
        Span::styled(left(label, label_width), theme.style(Role::Text)),
        Span::raw("  "),
    ];
    fit_parts(lead, &parts, usize::from(width))
}

/// Each agent's label, and its agent's name when others in the phase share the label, so six
/// drafters read apart. Any still alike number their label, which a narrow pane keeps when it
/// cuts the name.
/// Each agent's label, numbered when the label and the agent both repeat, and whether the
/// label alone repeats, since then only the agent's name tells the rows apart.
fn sibling_names(siblings: &[AgentRow], names: &DisplayNames) -> Vec<(String, bool)> {
    let labels: Vec<String> = siblings
        .iter()
        .map(|row| sanitize_line(&row.label).trim().to_owned())
        .collect();
    let named: Vec<(String, Option<String>)> = siblings
        .iter()
        .zip(&labels)
        .map(|(row, label)| {
            let shared = labels.iter().filter(|other| *other == label).count() > 1;
            let agent = shared.then(|| sanitize_line(&names.worker(&row.worker)).trim().to_owned());
            (label.clone(), agent)
        })
        .collect();
    named
        .iter()
        .enumerate()
        .map(|(i, name)| match &name.1 {
            Some(_) if named.iter().filter(|other| *other == name).count() > 1 => {
                let nth = named[..=i].iter().filter(|other| *other == name).count();
                (format!("{} {nth}", name.0), true)
            }
            _ => (name.0.clone(), name.1.is_some()),
        })
        .collect()
}

/// The cursor slot, the glyph, and the space after it.
fn lead(slot: usize) -> usize {
    1 + slot + 1
}

/// A row's glyph and its color, as the workflow view draws them.
fn row_glyph(marks: &Marks, status: RowStatus) -> (&'static str, Role) {
    match status {
        RowStatus::Running => (marks.running, Role::Faint),
        RowStatus::Done => (marks.finished, Role::Success),
        RowStatus::Failed => (marks.failed, Role::Error),
        RowStatus::NeedsYou => (marks.needs_you, Role::Card),
        RowStatus::Queued | RowStatus::Waiting | RowStatus::Skipped => (marks.waiting, Role::Dim),
    }
}

/// Under the status line, the ids and the numbers the row leaves out: `claude-code/opus ·
/// run-01 · 381k tok · $0.84`, then the context it has used. The id stays for commands such as
/// /reassign; the run and the context give way first.
fn identity_line(row: &AgentRow, run: &RunId, width: u16, ctx: &ViewCtx<'_>) -> Line<'static> {
    let dim = ctx.theme.style(Role::Dim);
    let mut parts = vec![Part::new(
        vec![Span::styled(
            sanitize_line(&row.worker.to_string()).into_owned(),
            dim,
        )],
        KEEP,
    )];
    let mut push = |text: String, rank: u16| {
        parts.push(Part::joined(
            Span::styled(" · ", dim),
            vec![Span::styled(text, dim)],
            rank,
        ));
    };
    push(sanitize_line(run.as_str()).into_owned(), 1);
    if row.tokens > 0 {
        push(panes::tokens(row.tokens), 3);
    }
    if let Some(spend) = panes::spend(&row.spent) {
        push(spend, 2);
    }
    if let Some(context) = row.context {
        push(
            format!(
                "ctx {}/{}",
                format::tokens(context.used),
                format::tokens(context.cap)
            ),
            0,
        );
    }
    fit_parts(Vec::new(), &parts, usize::from(width))
}

/// `✔ Completed · Opus 5.5 on Claude Code · 6m02s`, as Claude Code heads an agent's detail,
/// then the handoff and review markers. The state never gives way.
fn status_line(
    row: &AgentRow,
    width: u16,
    ctx: &ViewCtx<'_>,
    seen_at: Instant,
    loaded: bool,
) -> Line<'static> {
    let theme = ctx.theme;
    let marks = Marks::for_theme(theme);
    let dim = theme.style(Role::Dim);
    let (glyph, role) = row_glyph(&marks, row.status);
    let word = match row.status {
        RowStatus::Waiting => "Waiting",
        RowStatus::Queued => "Queued",
        RowStatus::Skipped => "Skipped",
        RowStatus::Running => "Running",
        RowStatus::NeedsYou => "Needs you",
        RowStatus::Done => "Completed",
        RowStatus::Failed => "Failed",
    };
    let mut parts = vec![Part::new(
        vec![
            Span::styled(glyph.to_owned(), theme.style(role)),
            Span::styled(format!(" {word}"), theme.style(Role::Text)),
        ],
        KEEP,
    )];
    let mut push = |text: String, style: Style, rank: u16| {
        parts.push(Part::joined(
            Span::styled(" · ", dim),
            vec![Span::styled(text, style)],
            rank,
        ));
    };
    let names = &ctx.state.names;
    push(
        sanitize_line(&names.worker(&row.worker)).into_owned(),
        dim,
        5,
    );
    let ticking = if loaded && row.status == RowStatus::Running {
        ctx.now.saturating_duration_since(seen_at)
    } else {
        Duration::ZERO
    };
    let elapsed = row.elapsed_ms + u64::try_from(ticking.as_millis()).unwrap_or(0);
    push(panes::elapsed(elapsed), dim, 3);
    if let Some(worker) = &row.standing_in_for {
        let original = sanitize_line(&names.worker(worker)).into_owned();
        push(format!("{} for {original}", marks.stands_in), dim, 4);
    }
    if needs_review(ctx, &row.session) {
        push(
            format!("{} needs review", marks.debt),
            theme.style(Role::Warning),
            4,
        );
    }
    fit_parts(Vec::new(), &parts, usize::from(width))
}

pub(super) fn vendor_internals(ctx: &ViewCtx<'_>) -> VendorInternals {
    ctx.state
        .ui
        .as_ref()
        .map_or(VendorInternals::Discovered, |ui| ui.panel.vendor_internals)
}

/// Whether the agent's card carries review debt, from the run's folded state.
pub(super) fn needs_review(ctx: &ViewCtx<'_>, session: &SessionKey) -> bool {
    ctx.state.runs.values().any(|run| {
        run.sessions
            .get(session)
            .and_then(|session| session.card.as_ref())
            .and_then(|card| run.cards.get(card))
            .is_some_and(|card| !card.debt.is_empty())
    })
}

impl View for AgentView {
    fn docked(&self) -> bool {
        true
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        if let Some(follow) = &self.follow {
            follow.show(Shown::Agent {
                run: self.data.run.clone(),
                session: self.session().clone(),
            });
        }
        let heading = Heading {
            names: vec![self.data.row.label.clone()],
            // The pane names the agent and its run, so the header says what the run is for.
            description: ctx
                .state
                .runs
                .get(&self.data.run)
                .map(|run| sanitize_line(&run.title).trim().to_owned())
                .unwrap_or_default(),
            totals: String::new(),
        };
        self.render_frame(area, buf, ctx, &heading, &[]);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if self.steer.is_some() {
            return self.steer_key(key);
        }
        // `j` `k` `g` `G` scroll the output with or without vim keys, and ↑↓ change agents.
        let key = match key.code {
            KeyCode::Char('h' | 'l') => list::standard(key, list::vim(ctx.state), KeyCode::Left),
            _ => key,
        };
        self.relayout(ctx);
        let internals = vendor_internals(ctx);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if self.confirm_stop {
            self.confirm_stop = false;
            match key.code {
                KeyCode::Char('x') if plain => {
                    return ViewOutcome::Send(UiCommand::StopAgent {
                        run: self.data.run.clone(),
                        session: self.data.row.session.clone(),
                    });
                }
                KeyCode::Esc => return ViewOutcome::Consumed,
                _ => {}
            }
        }
        let page = self.drawn.page.max(1) as isize;
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                let next = if key.code == KeyCode::Up {
                    self.selected.checked_sub(1)
                } else {
                    Some(self.selected + 1).filter(|&i| i < self.siblings.len())
                };
                let Some(next) = next else {
                    return ViewOutcome::Consumed;
                };
                self.selected = next;
                self.scroll = 0;
                self.confirm_stop = false;
                if self.loaded() {
                    return ViewOutcome::Consumed;
                }
                ViewOutcome::Request(ViewRequest::Agent {
                    run: self.data.run.clone(),
                    session: self.session().clone(),
                })
            }
            KeyCode::Char('j') if plain => {
                self.scroll_by(1);
                ViewOutcome::Consumed
            }
            KeyCode::Char('k') if plain => {
                self.scroll_by(-1);
                ViewOutcome::Consumed
            }
            KeyCode::PageDown => {
                self.scroll_by(page);
                ViewOutcome::Consumed
            }
            KeyCode::Char(' ') if plain => {
                self.scroll_by(page);
                ViewOutcome::Consumed
            }
            KeyCode::PageUp => {
                self.scroll_by(-page);
                ViewOutcome::Consumed
            }
            KeyCode::Home => {
                self.scroll = 0;
                ViewOutcome::Consumed
            }
            KeyCode::Char('g') if plain => {
                self.scroll = 0;
                ViewOutcome::Consumed
            }
            KeyCode::End => {
                self.scroll = self.drawn.total.saturating_sub(self.drawn.page);
                ViewOutcome::Consumed
            }
            KeyCode::Char('G') if plain => {
                self.scroll = self.drawn.total.saturating_sub(self.drawn.page);
                ViewOutcome::Consumed
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let count = self.expandable(internals).len();
                if count > 0 {
                    let at = self.cursor.min(count - 1);
                    self.cursor = if key.code == KeyCode::Tab {
                        (at + 1) % count
                    } else {
                        (at + count - 1) % count
                    };
                    self.relayout(ctx);
                    if let Some(section) = self.current_section(internals) {
                        self.reveal(section);
                    }
                }
                ViewOutcome::Consumed
            }
            KeyCode::Enter if self.current_section(internals) == Some(Section::Diff) => {
                self.open_diff(ctx)
            }
            KeyCode::Enter => {
                if let Some(section) = self.current_section(internals) {
                    if let Some(at) = self.expanded.iter().position(|&s| s == section) {
                        self.expanded.remove(at);
                    } else {
                        self.expanded.push(section);
                    }
                    self.relayout(ctx);
                    self.reveal(section);
                }
                ViewOutcome::Consumed
            }
            KeyCode::Char('i') if plain && self.loaded() => {
                self.steer = Some(String::new());
                ViewOutcome::Consumed
            }
            KeyCode::Char('t') if plain && self.loaded() => {
                let card = ctx
                    .state
                    .session(&self.data.run, &self.data.row.session)
                    .and_then(|session| session.card.clone());
                match card {
                    Some(card) => ViewOutcome::ConfirmTakeOver {
                        run: self.data.run.clone(),
                        card,
                    },
                    None => ViewOutcome::Notice("This agent has no card to take over".to_owned()),
                }
            }
            KeyCode::Char('d') if plain && self.loaded() => self.open_diff(ctx),
            KeyCode::Char('x') if plain && self.loaded() => {
                self.confirm_stop = true;
                ViewOutcome::Consumed
            }
            KeyCode::Char('p') if plain => super::pause(&self.data.run, ctx),
            // ← goes up, as Esc does.
            KeyCode::Esc | KeyCode::Left => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &argonauts_core::app::ViewData) -> bool {
        match data {
            argonauts_core::app::ViewData::Agent { view } if view.run == self.data.run => {
                self.show((**view).clone())
            }
            _ => false,
        }
    }

    fn next_change(&self, now: Instant) -> Option<Instant> {
        let seen_at = self.seen_at?;
        if !self.loaded() {
            return None;
        }
        // The agent's own time counts, or else a running agent's in the left pane.
        let row = std::iter::once(&self.data.row)
            .chain(&self.siblings)
            .find(|row| row.status == RowStatus::Running)?;
        let shown = row.elapsed_ms + millis(now.saturating_duration_since(seen_at));
        Some(now + Duration::from_millis(1000 - shown % 1000))
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use argonauts_core::views::ActivityLine;
    use argonauts_core::{DiffStat, RunId, Spend};

    use super::*;
    use crate::stability::{assert_frames, buffer_text, check_with};
    use crate::state::AppState;
    use crate::theme::Glyphs;

    fn theme(glyphs: Glyphs) -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = glyphs;
        theme
    }

    fn data(outcome: &str) -> AgentData {
        AgentData {
            run: RunId::new("run-01").unwrap(),
            row: AgentRow {
                session: SessionKey::parse("s-002").unwrap(),
                label: "build:b".into(),
                worker: "claude-code/opus".parse().unwrap(),
                status: RowStatus::Running,
                tokens: 381_000,
                spent: Spend::default(),
                elapsed_ms: 362_000,
                subagents: 2,
                context: None,
                standing_in_for: None,
            },
            prompt: "Build the history cells.".into(),
            activity: Vec::new(),
            tool_calls: 0,
            diff: None,
            subagents: None,
            outcome: Some(outcome.into()),
        }
    }

    fn render(view: &mut AgentView, width: u16, theme: &Theme, state: &AppState) -> Buffer {
        let ctx = ViewCtx {
            theme,
            state,
            now: Instant::now(),
        };
        let area = Rect::new(0, 0, width, 24);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        buf
    }

    fn draw(view: &mut AgentView, width: u16) -> String {
        let buf = render(view, width, &theme(Glyphs::UNICODE), &AppState::default());
        assert_frames(&buf);
        buffer_text(&buf)
    }

    fn key(view: &mut AgentView, code: KeyCode, modifiers: KeyModifiers) -> ViewOutcome {
        key_in(view, code, modifiers, &AppState::default())
    }

    fn key_in(
        view: &mut AgentView,
        code: KeyCode,
        modifiers: KeyModifiers,
        state: &AppState,
    ) -> ViewOutcome {
        let theme = theme(Glyphs::UNICODE);
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: Instant::now(),
        };
        view.handle_key(KeyEvent::new(code, modifiers), &ctx)
    }

    fn phase(rows: Vec<WorkflowRow>) -> PhaseRow {
        PhaseRow {
            name: "Build".into(),
            standard: argonauts_core::workflow::StandardPhase::Do,
            status: RowStatus::Running,
            detail: String::new(),
            attempt: None,
            count: Some(argonauts_core::views::PhaseCount {
                done: 0,
                total: rows.len() as u32,
            }),
            rows,
        }
    }

    fn in_phase(outcome: &str) -> AgentView {
        let data = data(outcome);
        let row = data.row.clone();
        AgentView::in_phase(data, &phase(vec![WorkflowRow::Agent(row)]))
    }

    /// The footer's last row, which a cut would end with `…`.
    fn footer(text: &str) -> &str {
        text.lines().last().unwrap_or_default().trim_start()
    }

    /// A run that's going, with the agent's session working on a card.
    fn live_state() -> AppState {
        let mut state = AppState::default();
        let run_id = RunId::new("run-01").unwrap();
        let mut run = crate::state::RunState::new(run_id.clone());
        run.started = Some(time::OffsetDateTime::now_utc());
        let mut session = crate::state::SessionState::new(
            SessionKey::parse("s-002").unwrap(),
            "claude-code/opus".parse().unwrap(),
            "claude-code",
            argonauts_core::names::RoleName::Owner,
            time::OffsetDateTime::now_utc(),
        );
        session.card = Some(argonauts_core::CardId::new("card-002").unwrap());
        run.sessions.insert(session.key.clone(), session);
        state.runs.insert(run_id, run);
        state
    }

    fn draw_in(view: &mut AgentView, width: u16, state: &AppState) -> String {
        let buf = render(view, width, &theme(Glyphs::UNICODE), state);
        assert_frames(&buf);
        buffer_text(&buf)
    }

    #[test]
    fn the_footer_keeps_its_ends_at_pty_widths() {
        let state = live_state();
        for width in [60, 80, 100] {
            let text = draw_in(&mut in_phase("Done."), width, &state);
            let footer = footer(&text);
            assert!(footer.starts_with("↑↓ agent"), "{footer}");
            let end = footer.trim_end();
            assert!(
                end.ends_with("esc back") || end.ends_with("s save"),
                "{footer}"
            );
            assert!(!footer.contains('…'), "{footer}");
        }
        let text = draw_in(&mut in_phase("Done."), 120, &state);
        let footer = footer(&text);
        for hint in ["i steer", "t take over", "x stop", "p pause", "s save"] {
            assert!(footer.contains(hint), "{hint}: {footer}");
        }
    }

    /// Steering and stopping need the agent running, and taking over and pausing need its run
    /// going. Enter's hint says what it does to the section it's on.
    #[test]
    fn the_footer_offers_only_the_keys_that_act() {
        let mut done = data(&"A line of the outcome.\n".repeat(12));
        done.row.status = RowStatus::Done;
        done.diff = Some(DiffStat::default());
        let row = done.row.clone();
        let mut view = AgentView::in_phase(done, &phase(vec![WorkflowRow::Agent(row)]));
        let mut finished = live_state();
        for run in finished.runs.values_mut() {
            run.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        }
        let text = draw_in(&mut view, 120, &finished);
        let shown = footer(&text);
        for gone in [
            "i steer",
            "t take over",
            "x stop",
            "p pause",
            "d diff",
            "tab",
        ] {
            assert!(!shown.contains(gone), "{gone}: {shown}");
        }
        assert!(shown.contains("⏎ open"), "{shown}");

        key_in(&mut view, KeyCode::Tab, KeyModifiers::NONE, &finished);
        let text = draw_in(&mut view, 120, &finished);
        assert!(footer(&text).contains("⏎ expand"), "{text}");
        key_in(&mut view, KeyCode::Enter, KeyModifiers::NONE, &finished);
        let text = draw_in(&mut view, 120, &finished);
        assert!(footer(&text).contains("⏎ collapse"), "{text}");
        assert!(footer(&text).contains("j/k scroll"), "{text}");
    }

    /// `j` and `k` scroll with vim keys or without, so the footer names them as Claude Code's
    /// does either way.
    #[test]
    fn the_footer_names_j_and_k_as_claude_code_does() {
        let text = draw(&mut in_phase("Done."), 120);
        assert!(footer(&text).contains("j/k scroll"), "{text}");
        let vim = crate::views::list::keys_state(true);
        let buf = render(&mut in_phase("Done."), 120, &theme(Glyphs::UNICODE), &vim);
        let text = buffer_text(&buf);
        assert!(footer(&text).contains("j/k scroll"), "{text}");
    }

    #[test]
    fn the_steer_line_keeps_the_end_of_what_you_type() {
        let mut view = in_phase("Done.");
        key(&mut view, KeyCode::Char('i'), KeyModifiers::NONE);
        for c in "use serde_json for the writer and keep the csv path as it is".chars() {
            key(&mut view, KeyCode::Char(c), KeyModifiers::NONE);
        }
        let text = draw(&mut view, 50);
        let footer = footer(&text);
        assert!(footer.contains("…"), "{footer}");
        assert!(footer.contains("as it is"), "{footer}");
    }

    #[test]
    fn a_short_outcome_takes_no_tab_stop() {
        let mut view = in_phase("Done.");
        let text = draw(&mut view, 80);
        assert!(text.contains("Outcome · 1 line"), "{text}");
        assert!(!text.contains("↵expand"), "{text}");

        let long = (1..=12)
            .map(|n| format!("Line {n}."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut view = in_phase(&long);
        let text = draw(&mut view, 80);
        assert!(text.contains("↵expand"), "{text}");
    }

    #[test]
    fn diff_opens_from_enter_and_d() {
        let mut data = data("Done.");
        data.diff = Some(DiffStat {
            files_changed: 2,
            insertions: 10,
            deletions: 3,
        });
        let mut view = AgentView::new(data);
        let text = draw(&mut view, 80);
        assert!(text.contains("↵open"), "{text}");

        // Diff is the one section Enter acts on here, and with no card to diff both say so.
        for code in [KeyCode::Char('d'), KeyCode::Enter] {
            let outcome = key(&mut view, code, KeyModifiers::NONE);
            assert!(
                matches!(&outcome, ViewOutcome::Notice(text) if text.contains("no diff")),
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn left_goes_back_and_ctrl_letters_pass_through() {
        let mut view = in_phase("Done.");
        draw(&mut view, 80);
        assert!(matches!(
            key(&mut view, KeyCode::Left, KeyModifiers::NONE),
            ViewOutcome::Close
        ));
        for c in ['d', 'g', 'G', ' ', 'p', 'x', 's', 't'] {
            assert!(
                matches!(
                    key(&mut view, KeyCode::Char(c), KeyModifiers::CONTROL),
                    ViewOutcome::Ignored
                ),
                "Ctrl+{c}"
            );
        }
    }

    #[test]
    fn with_vim_keys_h_goes_back_and_l_acts_as_enter() {
        let mut data = data("Done.");
        data.diff = Some(DiffStat {
            files_changed: 2,
            insertions: 10,
            deletions: 3,
        });
        let mut view = AgentView::new(data);
        draw(&mut view, 80);
        let plain = KeyModifiers::NONE;
        for vim in [false, true] {
            let state = list::keys_state(vim);
            let outcome = key_in(&mut view, KeyCode::Char('l'), plain, &state);
            let opened = matches!(&outcome, ViewOutcome::Notice(text) if text.contains("no diff"));
            assert_eq!(opened, vim, "{outcome:?}");
            let outcome = key_in(&mut view, KeyCode::Char('h'), plain, &state);
            assert_eq!(matches!(outcome, ViewOutcome::Close), vim, "{outcome:?}");
        }
    }

    #[test]
    fn pause_acts_only_on_the_run_on_screen() {
        let mut view = in_phase("Done.");
        let outcome = key(&mut view, KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(
            matches!(&outcome, ViewOutcome::Notice(text) if text.starts_with("Only the run on screen")),
            "{outcome:?}"
        );
        let mut state = AppState::default();
        state.attached = Some(view.data.run.clone());
        assert!(matches!(
            key_in(&mut view, KeyCode::Char('p'), KeyModifiers::NONE, &state),
            ViewOutcome::Send(UiCommand::Pause { now: false })
        ));
    }

    /// Through the terminal's diff, as a pty shows it: the code span stays before the long word.
    #[test]
    fn the_outcome_keeps_a_code_span_before_a_long_word() {
        let source = format!("- `code` span {}", "a".repeat(150));
        let theme = theme(Glyphs::UNICODE);
        for width in [80, 100, 120] {
            let buf = render(&mut in_phase(&source), width, &theme, &AppState::default());
            let mut previous = Buffer::empty(buf.area);
            for cell in &mut previous.content {
                cell.set_symbol("#");
            }
            let mut out = Vec::new();
            crate::terminal::flush_diff(&mut out, &previous, &buf).unwrap();
            let mut parser = vt100::Parser::new(24, width, 0);
            parser.process(&out);
            let screen = parser.screen().contents();
            assert!(screen.contains("- code span"), "{screen}");
        }
    }

    fn row(session: &str, status: RowStatus) -> AgentRow {
        AgentRow {
            session: SessionKey::parse(session).unwrap(),
            label: "owner · card-002".into(),
            worker: "claude-code/opus".parse().unwrap(),
            status,
            tokens: 0,
            spent: Spend::default(),
            elapsed_ms: 0,
            subagents: 0,
            context: None,
            standing_in_for: None,
        }
    }

    #[test]
    fn a_phase_shows_the_agent_it_opened_with() {
        let fresh = row("s-002", RowStatus::Running);
        let data = AgentData {
            run: RunId::new("run-01").unwrap(),
            row: fresh.clone(),
            prompt: String::new(),
            activity: Vec::new(),
            tool_calls: 0,
            diff: None,
            subagents: None,
            outcome: None,
        };
        let siblings = vec![
            WorkflowRow::Agent(row("s-001", RowStatus::Done)),
            WorkflowRow::Agent(row("s-002", RowStatus::Done)),
        ];
        let view = AgentView::in_phase(data, &phase(siblings));
        assert_eq!(view.selected, 1);
        assert_eq!(view.siblings[1], fresh);
    }

    #[test]
    fn the_agent_is_named_with_its_id_and_the_run_under_the_title() {
        let rows = vec![WorkflowRow::Agent(data("done").row)];
        let mut view = AgentView::in_phase(data("done"), &phase(rows));
        let text = draw(&mut view, 100);
        let lines: Vec<&str> = text.lines().collect();
        // Drawn alone here, the view has its own header too; in a workflow's frame that's
        // the workflow's.
        let named = lines
            .iter()
            .position(|line| line.contains('│') && line.contains("claude-code/opus · run-01"))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(
            lines[named - 1].contains("● Running · Opus on Claude Code"),
            "{text}"
        );

        // On its own, the header already says it.
        let text = draw(&mut AgentView::new(data("done")), 100);
        assert_eq!(
            text.matches("claude-code/opus · run-01").count(),
            1,
            "{text}"
        );
    }

    #[test]
    fn repeated_labels_are_told_apart_by_the_agent() {
        let labelled = |session: &str, label: &str, worker: &str| AgentRow {
            label: label.into(),
            worker: worker.parse().unwrap(),
            ..row(session, RowStatus::Done)
        };
        let codex = labelled("s-002", "drafter", "codex");
        let data = AgentData {
            row: codex.clone(),
            ..data("done")
        };
        let rows = vec![
            WorkflowRow::Agent(labelled("s-001", "drafter", "claude-code")),
            WorkflowRow::Agent(codex),
            WorkflowRow::Agent(labelled("s-003", "judge", "codex")),
        ];
        let mut view = AgentView::in_phase(data, &phase(rows));
        let text = draw(&mut view, 100);
        let left = |needle: &str| {
            text.lines()
                .find(|line| line.contains(needle))
                .map(|line| line.split('│').nth(1).unwrap_or_default().trim().to_owned())
                .unwrap_or_else(|| panic!("{needle}: {text}"))
        };
        // The agents' names line up in a column of their own, whole while a label repeats.
        assert_eq!(left("✔ drafter  Claude Code"), "✔ drafter  Claude Code");
        assert!(left("❯✔ drafter").ends_with("drafter  Codex"), "{text}");
        assert_eq!(left("✔ judge"), "✔ judge");
    }

    /// Two judges on one agent still read alike by name, so their labels take a number.
    #[test]
    fn alike_agents_number_their_labels() {
        let labelled = |session: &str, label: &str, worker: &str| AgentRow {
            label: label.into(),
            worker: worker.parse().unwrap(),
            ..row(session, RowStatus::Done)
        };
        let rows = [
            labelled("s-001", "judge", "codex"),
            labelled("s-002", "judge", "codex"),
            labelled("s-003", "judge", "claude-code"),
            labelled("s-004", "owner", "codex"),
        ];
        let shown: Vec<String> = sibling_names(&rows, &DisplayNames::default())
            .into_iter()
            .map(|(label, repeated)| format!("{label} {repeated}"))
            .collect();
        assert_eq!(
            shown,
            ["judge 1 true", "judge 2 true", "judge true", "owner false"]
        );
    }

    /// The preview skips the blank line between the prompt's parts.
    #[test]
    fn the_prompt_preview_shows_lines_with_text() {
        let mut data = data("Done.");
        data.prompt =
            "You make this change on your own.\n\n- Work only in the worktree.\n- Never commit.\n"
                .into();
        let text = draw(&mut AgentView::new(data), 100);
        assert!(text.contains("Prompt · 4 lines"), "{text}");
        assert!(
            text.contains("  You make this change on your own."),
            "{text}"
        );
        assert!(text.contains("  - Work only in the worktree."), "{text}");
        assert!(text.contains("…1 more line "), "{text}");
    }

    #[test]
    fn a_finished_agent_had_no_tool_calls_rather_than_none_yet() {
        let mut done = data("done");
        done.row.status = RowStatus::Done;
        let text = draw(&mut AgentView::new(done.clone()), 100);
        assert!(text.contains("Activity · no tool calls "), "{text}");
        assert!(!text.contains("no tool calls yet"), "{text}");
        let text = draw(&mut AgentView::new(data("done")), 100);
        assert!(text.contains("no tool calls yet"), "{text}");

        done.activity = vec![ActivityLine {
            tool: "Bash".into(),
            summary: "npm test".into(),
            ok: Some(true),
        }];
        done.tool_calls = 1;
        let text = draw(&mut AgentView::new(done.clone()), 100);
        assert!(text.contains("Activity · 1 tool call "), "{text}");
        // Only some show: the latest of how many.
        done.tool_calls = 9;
        let text = draw(&mut AgentView::new(done), 100);
        assert!(
            text.contains("Activity · last 1 of 9 tool calls "),
            "{text}"
        );
    }

    #[test]
    fn a_phase_counts_the_agents_of_its_nested_workflows() {
        let data = data("Done.");
        let nested = argonauts_core::views::NestedWorkflowRow {
            id: argonauts_core::names::WorkflowId::numbered(2),
            name: "build:c-demo-tests".into(),
            status: RowStatus::Running,
            agents_done: 1,
            agents_total: 4,
            workers: Vec::new(),
            elapsed_ms: 0,
        };
        let rows = vec![
            WorkflowRow::Agent(data.row.clone()),
            WorkflowRow::Workflow(nested),
        ];
        let mut view = AgentView::in_phase(data, &phase(rows));
        // The nested workflow isn't a row to switch to, but its four agents count.
        assert_eq!(view.siblings.len(), 1);
        let text = draw(&mut view, 100);
        assert!(text.contains("Build · 5 agents"), "{text}");
    }

    #[test]
    fn ascii_frames_are_ascii() {
        let theme = theme(Glyphs::ASCII);
        for width in [40, 80, 100] {
            let buf = render(&mut in_phase("Done."), width, &theme, &AppState::default());
            let text = buffer_text(&buf);
            // A cut shows `...` only where the terminal's ambiguous width is known to be 2, a
            // process setting, so the narrow frame with cut titles is left to the pty tests.
            if width >= 80 {
                assert!(check_with(&buf, 2).is_empty(), "{text}");
            }
            let box_drawing = |c: char| ('\u{2500}'..='\u{257F}').contains(&c);
            assert!(!text.chars().any(box_drawing), "{text}");
            assert!(!footer(&text).contains(['↑', '↓', '⏎']), "{text}");
        }
    }
}
