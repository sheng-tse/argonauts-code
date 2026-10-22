//! The agent view, in the workflow view's frame, as Claude Code opens an agent of a workflow:
//! the phase's agents on the left, and on the right the selected agent's state and counts,
//! then its Prompt, Activity, and Outcome. Argonauts Code adds the findings a review sent it
//! back with, a reviewer's own findings as its outcome, and its Diff and Subagents.

use std::time::{Duration, Instant};

use argonauts_core::api::DiffScope;
use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::settings::ui::CliInternals;
use argonauts_core::steps::plan::Severity;
use argonauts_core::views::{
    AgentRow, AgentView as AgentData, AskedFor, Finding, Moved, PhaseRow, RowStatus, StartedFrom,
    StatusLine, Tone, WorkflowRow,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::format;
use crate::kit::State;
use crate::markdown::{Indent, render_markdown};
use crate::render::{Clock, RenderCtx};
use crate::text::{
    KEEP, Part, fit_parts, sanitize, sanitize_line, str_width, truncate_spans, wrap_text,
};
use crate::theme::{Role, Theme};

use super::dock::{self, Heading};
use super::follow::{Follower, Shown};
use super::goal_panel;
use super::layout::{cut, left};
use super::list;
use super::panes::{self, Footer, Marks};
use super::session::{SessionView, Tab};
use super::states;
use super::structured;
use super::workflow_outline::{self, AgentOutline, Place};
use super::workflow_rows::{self, Models, tone_role};
use super::{View, ViewCtx, ViewOutcome};

/// Lines a section shows until it's expanded.
const PROMPT_PREVIEW: usize = 1;
const ACTIVITY_PREVIEW: usize = 3;
const SUBAGENT_PREVIEW: usize = 3;
const OUTCOME_PREVIEW: usize = 6;

/// A section folds only when that hides at least this many lines, as Claude Code's do.
const WORTH_FOLDING: usize = 2;

/// The list beside an agent is as wide as its widest row, between these, and never so wide
/// that the agent keeps fewer than [`LEAST_RIGHT`] cells: Claude Code's own bounds.
const LEAST_LEFT: usize = 14;
const MOST_LEFT: usize = 30;
const LEAST_RIGHT: usize = 30;

/// Where two agents share a label, each row names its agent too, and the list may take up to
/// this, or three tenths of the screen.
const MOST_LEFT_NAMED: usize = 64;

/// What the box takes beside its two panes: its borders and the blank cell inside each.
const BOX_EDGES: usize = 7;
/// Below this many columns the list beside an open agent keeps to a quarter of the box.
const NARROW: u16 = 120;

/// Longer subagent names are cut, so their states still line up.
const MAX_CHILD_LABEL: usize = 24;

/// Steering text keeps at least this many cells before its key hints give way.
const MIN_STEER: usize = 12;

#[derive(Debug)]
pub struct AgentView {
    pub data: AgentData,
    /// The phase's name and what it holds, such as `2 agents`, for the left pane's title.
    /// `None` opened on its own, with no left pane.
    phase: Option<(String, String)>,
    siblings: Vec<AgentRow>,
    /// The phase's outline as its workflow view had it open, when it was opened from one.
    outline: Option<AgentOutline>,
    /// How its run stands, as the workflow view it was opened from has it, which knows a run
    /// read from its journal that this session never watched.
    pub run_state: Option<argonauts_core::views::WorkflowState>,
    selected: usize,
    scroll: usize,
    /// Enter shows every section whole, and folds them again.
    expanded: bool,
    steer: Option<String>,
    confirm_stop: bool,
    /// A restart `r` asked for, which a second `r` sends.
    restart: Option<super::Restart>,
    /// When the data was first drawn, so a running agent's time keeps counting.
    seen_at: Option<Instant>,
    /// Where each section starts and how much shows, from the last frame, for the keys.
    drawn: Drawn,
    /// Set when the view was opened on its own; in a workflow's frame the workflow follows.
    follow: Option<Follower>,
    /// In a workflow's frame, the workflow's end, for a view this one opens over itself.
    lent: Option<Follower>,
}

#[derive(Debug, Default, Clone)]
struct Drawn {
    total: usize,
    page: usize,
    /// The right pane's width, so keys can lay the sections out again between frames.
    width: u16,
    /// Rows the Outcome renders to at that width, so it expands only when some are hidden.
    outcome_rows: usize,
    /// Rows of the Outcome that show before it's expanded.
    outcome_preview: usize,
}

impl AgentView {
    pub fn new(data: AgentData) -> Self {
        let siblings = vec![data.row.clone()];
        Self {
            data,
            phase: None,
            siblings,
            outline: None,
            run_state: None,
            selected: 0,
            scroll: 0,
            expanded: false,
            steer: None,
            confirm_stop: false,
            restart: None,
            seen_at: None,
            drawn: Drawn::default(),
            follow: None,
            lent: None,
        }
    }

    /// Keeps the view current with its run's events while it's on top.
    pub fn following(mut self, follower: Follower) -> Self {
        self.follow = Some(follower);
        self
    }

    /// The end a view opened over this one keeps current with, when this view has none of
    /// its own.
    pub(super) fn lend(mut self, follower: Option<Follower>) -> Self {
        self.lent = follower;
        self
    }

    /// `g`: the agent in full, on its Goal tab.
    fn open_goal(&self, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let (run, session) = (&self.data.run, &self.data.row.session);
        if self.data.goal.is_none() {
            return ViewOutcome::Notice("This agent works without a goal".to_owned());
        }
        if ctx.state.session(run, session).is_none() {
            return ViewOutcome::Notice(
                "This agent isn't on screen, so its goal can't open".into(),
            );
        }
        let mut view = SessionView::new(run.clone(), session.clone()).on(Tab::Goal);
        if let Some(follow) = self.follow.as_ref().or(self.lent.as_ref()) {
            view = view.following(follow.another());
        }
        ViewOutcome::Open(Box::new(view))
    }

    /// Opened from a workflow's phase: the phase's outline shows on the left, each workflow
    /// inside open while it runs, and ↑↓ switches between its agents.
    pub fn in_phase(data: AgentData, phase: &PhaseRow) -> Self {
        let rows: Vec<&WorkflowRow> = phase.rows.iter().collect();
        let items = workflow_outline::items(&rows, None, &workflow_outline::at_work);
        let outline = AgentOutline::of(&items, &workflow_outline::at_work);
        Self::in_outline(data, phase, outline)
    }

    /// Opened from a workflow's phase, its outline as the workflow view has it open.
    pub(super) fn in_outline(data: AgentData, phase: &PhaseRow, outline: AgentOutline) -> Self {
        let mut view = Self::new(data);
        if let Some(index) = outline.agents.iter().position(|row| row.is(&view.data.row)) {
            view.siblings = outline.agents.clone();
            // The phase's row can be older than the agent just loaded.
            view.siblings[index] = view.data.row.clone();
            view.selected = index;
            view.outline = Some(outline);
        }
        view.phase = Some((phase.name.clone(), workflow_rows::phase_size(phase)));
        view
    }

    /// Where the selected agent sits in its phase's outline.
    fn place(&self) -> Place {
        self.outline
            .as_ref()
            .and_then(|outline| outline.places.get(self.selected).cloned())
            .unwrap_or_default()
    }

    /// The agent the left pane has selected, which may still be loading.
    pub(super) fn selected_row(&self) -> &AgentRow {
        &self.siblings[self.selected]
    }

    /// Takes a fresh answer for one of its agents. False for any other agent.
    pub fn show(&mut self, data: AgentData) -> bool {
        let Some(index) = self.siblings.iter().position(|row| row.is(&data.row)) else {
            return false;
        };
        if !data.row.is(&self.data.row) {
            self.scroll = 0;
            self.expanded = false;
        }
        self.siblings[index] = data.row.clone();
        self.data = data;
        self.seen_at = None;
        true
    }

    fn loaded(&self) -> bool {
        self.data.row.is(self.selected_row())
    }

    /// Draws the view in the docked frame under `heading`, the workflow's own when it's shown
    /// in a workflow's frame: the phase's agents on the left, as Claude Code lists them, and
    /// this agent on the right, with its position on the box's bottom edge. `models` are the
    /// workflow's, so the agent's model keeps the color its row has.
    pub(super) fn render_frame(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &ViewCtx<'_>,
        heading: &Heading,
        status: &[StatusLine],
        models: &Models,
    ) {
        let theme = ctx.theme;
        let seen_at = *self.seen_at.get_or_insert(ctx.now);
        let phase_title = match &self.phase {
            Some((phase, size)) => format!("{} · {size}", sanitize_line(phase).trim()),
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
        let mut frame = dock::split(area, 0, status.len() as u16);
        if self.phase.is_some() {
            let labels: Vec<String> = names.iter().map(|(label, _)| label.clone()).collect();
            // An outline's lines take their level's cells too.
            let longest = match &self.outline {
                Some(outline) => outline.width(0, &labels),
                None => labels
                    .iter()
                    .map(|label| str_width(label))
                    .max()
                    .unwrap_or(0),
            };
            let panes = usize::from(frame.panes.body.width).saturating_sub(BOX_EDGES);
            // Labels that carry their agent's name need more than Claude Code's own do.
            let named = names.iter().any(|(_, repeated)| *repeated);
            let most = if named {
                MOST_LEFT.max(MOST_LEFT_NAMED.min(usize::from(area.width) * 3 / 10))
            } else {
                MOST_LEFT
            };
            // On a narrow screen the list keeps to a quarter of the box, as Claude Code's does,
            // and its title is cut to fit. Labels that need their agent's name to be told apart
            // keep their room.
            let most = if area.width < NARROW && !named {
                most.min(panes / 4).max(LEAST_LEFT)
            } else {
                most
            };
            let left_width = (LIST_MARGIN + longest)
                .max(str_width(&phase_title))
                .clamp(LEAST_LEFT, most)
                .min(panes.saturating_sub(LEAST_RIGHT));
            frame = dock::split(area, left_width as u16, status.len() as u16);
        }
        dock::draw_rule(buf, &frame, theme);
        dock::draw_heading(buf, &frame, heading, theme);
        dock::draw_status(buf, &frame, status, &[], theme);
        let panes = frame.panes;
        let label = sanitize_line(&self.siblings[self.selected].label)
            .trim()
            .to_owned();
        if panes.left.width > 0 {
            let rows = self.sibling_lines(&names, panes.left, theme);
            panes::put_lines(buf, panes.left, &rows);
        }

        let lines = self.detail_lines(panes.right.width, ctx, seen_at, models);
        let page = usize::from(panes.right.height);
        self.scroll = self.scroll.min(lines.len().saturating_sub(page));
        self.drawn.total = lines.len();
        self.drawn.page = page;
        self.drawn.width = panes.right.width;
        let last = (self.scroll + page).min(lines.len());
        let mark = workflow_rows::scroll_mark(self.scroll, last, lines.len(), theme);
        dock::draw_box(buf, &frame, [&phase_title, &label], &mark, theme);
        panes::put_lines(buf, panes.right, &lines[self.scroll..last]);
        let footer = self.footer(usize::from(panes.footer.width), ctx);
        dock::draw_footer(buf, &frame, footer, theme);
    }

    fn running(&self) -> bool {
        matches!(
            self.data.row.status,
            RowStatus::Running | RowStatus::NeedsYou | RowStatus::Waiting
        )
    }

    /// Claude Code's keys for an open agent, and messaging it, the same whoever is selected:
    /// `↑↓ agent · j/k scroll · ⏎ expand · m message · x stop · r restart · p pause · esc back ·
    /// s save`. A key that can't act now says why when pressed. Lower numbers give way first.
    fn footer(&self, width: usize, ctx: &ViewCtx<'_>) -> Footer {
        if let Some(text) = &self.steer {
            return Footer::Line(steer_line(text, width, ctx.theme));
        }
        if self.confirm_stop {
            let label = sanitize_line(&self.data.row.label).trim().to_owned();
            let ask = format!("x again to stop {label}");
            return Footer::hints(&[(&ask, 0), ("esc to keep", 0)]);
        }
        if let Some(restart) = &self.restart {
            return Footer::hints(&[(&restart.ask(), 0), ("esc to keep", 0)]);
        }
        let enter = if self.expanded {
            "⏎ collapse"
        } else {
            "⏎ expand"
        };
        // An agent of a CLI's own workflow is that CLI's to message and stop, from the
        // worker whose CLI runs it.
        if let Some(host) = self.place().host.filter(|_| self.data.row.inner.is_some()) {
            let elsewhere = format!("to message or stop it, select {host}");
            return Footer::hints(&[
                ("↑↓  agent", 0),
                ("j/k scroll", 3),
                (enter, 2),
                (panes::ESC_BACK, 0),
                (&elsewhere, 1),
            ]);
        }
        let mut hints = vec![("↑↓  agent", 0), ("j/k scroll", 6), (enter, 7)];
        // With vim keys on, `g` is the top of the list.
        if self.loaded() && self.data.goal.is_some() && !list::vim(ctx.state) {
            hints.push(("g its goal", 5));
        }
        hints.push(("m message", 5));
        // As in Claude Code, an agent stops or starts over only while it runs.
        if self.running() {
            hints.extend([("x stop", 4), ("r restart", 4)]);
        }
        let ended = super::run_ended(ctx.state, &self.data.run)
            || self.run_state.is_some_and(|state| state.has_ended());
        if !self.running() && ended {
            hints.push(("c continue", 3));
        }
        // A run that ended has nothing left to pause; a paused one says p resumes it.
        if !ended {
            hints.push((super::pause_hint(ctx.state, &self.data.run), 3));
        }
        hints.push((panes::ESC_BACK, 0));
        if self.phase.is_some() {
            hints.push(("s save", 1));
        }
        Footer::hints(&hints)
    }

    /// Each agent of the phase as Claude Code lists them beside an open agent: the cursor, the
    /// state glyph, and the label.
    fn sibling_lines(
        &self,
        names: &[(String, bool)],
        area: Rect,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let slot = marks.slot();
        let width = usize::from(area.width);
        let accent = theme.style(Role::Question);
        let line = |i: usize, indent: usize| {
            let row = &self.siblings[i];
            let (glyph, role) = workflow_rows::glyph(row.status, row.fallback_for.is_some(), theme);
            let selected = i == self.selected;
            // A row at work is dim, with its grey glyph.
            let label_style = match (selected, row.status, role) {
                (true, _, _) => accent,
                (false, RowStatus::Running, Role::Faint) => theme.style(Role::Dim),
                (false, _, _) => theme.style(Role::Text),
            };
            Line::from(vec![
                if selected {
                    Span::styled(format!("{} ", marks.selected), accent)
                } else {
                    Span::raw("  ")
                },
                Span::raw(" ".repeat(indent)),
                Span::styled(
                    format!("{} ", panes::pad_glyph(glyph, slot)),
                    theme.style(role),
                ),
                Span::styled(
                    cut(&names[i].0, width.saturating_sub(LIST_MARGIN + indent)),
                    label_style,
                ),
            ])
        };
        if let Some(outline) = &self.outline {
            return outline.lines(self.selected, usize::from(area.height), theme, &line);
        }
        let (first, last) =
            workflow_rows::window(self.selected, self.siblings.len(), usize::from(area.height));
        (first..last).map(|i| line(i, 0)).collect()
    }

    /// Everything in the right pane, top down, before scrolling: the state and who runs it,
    /// its counts, a blank row, and the sections one under another.
    fn detail_lines(
        &mut self,
        width: u16,
        ctx: &ViewCtx<'_>,
        seen_at: Instant,
        models: &Models,
    ) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let row = &self.siblings[self.selected];
        let loaded = self.loaded();
        let facts: &[String] = if loaded { &self.data.facts } else { &[] };
        let ticking = if loaded && row.status == RowStatus::Running {
            millis(ctx.now.saturating_duration_since(seen_at))
        } else {
            0
        };
        let tool_calls = if loaded { self.data.tool_calls } else { 0 };
        // Where it sits, for an agent of a workflow inside: first among the facts, and two
        // levels in, its own line under the state.
        let (said, levels) = self.place().words();
        let facts: Vec<String> = said.into_iter().chain(facts.iter().cloned()).collect();
        // An agent view that says whose place it takes below leaves it off its first line.
        let said_below = matches!(self.data.moved, Some(Moved::FallbackFor { .. })) && loaded;
        let mut lines = vec![status_line(row, &facts, said_below, models, width, ctx)];
        if let Some(levels) = levels {
            lines.push(Line::from(Span::styled(
                cut(&levels, usize::from(width)),
                theme.style(Role::Dim),
            )));
        }
        lines.extend(counts_line(row, tool_calls, ticking, width, theme));
        // Where its goal stands, as its row in the workflow says it, and who keeps it.
        if let Some(goal) = self.data.goal.as_deref().filter(|_| loaded) {
            let mut spans = goal_panel::squares(goal, theme);
            spans.push(Span::styled(
                format!(" {} · ", goal_panel::row_words(goal)),
                theme.style(Role::Text),
            ));
            spans.extend(goal_panel::kept_by(goal, theme));
            lines.push(Line::from(truncate_spans(&spans, usize::from(width))));
        }
        lines.push(Line::default());
        if !loaded {
            lines.push(Line::from(Span::styled("Loading…", theme.style(Role::Dim))));
            return lines;
        }
        // Claude Code leaves a blank line between an open agent's sections.
        let mut first = true;
        let mut section = |lines: &mut Vec<Line<'static>>| {
            if !std::mem::take(&mut first) {
                lines.push(Line::default());
            }
        };
        if let Some(from) = &self.data.started_from {
            section(&mut lines);
            lines.extend(started_from_lines(from, width, ctx));
        }
        if let Some(moved) = &self.data.moved {
            section(&mut lines);
            lines.extend(moved_lines(moved, models, width, ctx));
        }
        if let Some(given) = self.data.given.as_deref() {
            section(&mut lines);
            lines.push(Line::from(Span::styled(
                cut(sanitize_line(given).trim(), usize::from(width)),
                theme.style(Role::Text),
            )));
        }
        if let Some(asked) = &self.data.asked_for {
            section(&mut lines);
            lines.extend(asked_for_lines(asked, &self.data.row, models, width, ctx));
        }
        // Nothing ran for a job left out, so it has no prompt, calls or outcome to show.
        if self.data.row.left_out {
            self.drawn.outcome_rows = 0;
            return lines;
        }
        let internals = cli_internals(ctx);
        let (outcome, plan_rows) = self.outcome_rows(width, ctx);
        self.drawn.outcome_rows = outcome.as_ref().map_or(0, Vec::len);
        // A plan's table shows whole; the preview is of the words under it.
        self.drawn.outcome_preview = plan_rows + OUTCOME_PREVIEW;
        let open = self.expanded;
        let inner_cli = self.inner_cli(&ctx.state.names);
        let blocks = [
            self.prompt(width, open, inner_cli.as_deref(), theme),
            self.to_fix(width, theme),
            self.earlier(width, &ctx.state.names, theme),
            self.activity(width, open, inner_cli.as_deref(), theme),
            self.diff(),
            self.subagents(width, open, internals, theme),
            self.its_workflow(theme),
            Some(self.outcome(outcome, open, width, theme)),
        ];
        // A reviewer's outcome reads its verdict in the verdict's colour, as its row does.
        let verdict = self.data.review.as_ref().map(|review| {
            let words = sanitize_line(&review.verdict).trim().to_owned();
            (format!("Outcome · {words}"), words, tone_role(review.tone))
        });
        for (title, hint, body) in blocks.into_iter().flatten() {
            let line = match &verdict {
                Some((outcome, words, role)) if title.starts_with(outcome.as_str()) => {
                    let rest = format!("{}{}", &title[outcome.len()..], hint_words(hint, theme));
                    let text = theme.style(Role::Text);
                    Line::from(vec![
                        Span::styled("Outcome · ", text),
                        Span::styled(words.clone(), theme.style(*role)),
                        Span::styled(rest, text),
                    ])
                }
                _ => section_title(&title, hint.as_deref(), theme),
            };
            section(&mut lines);
            lines.push(line);
            lines.extend(body);
        }
        lines
    }

    /// Whether Enter has anything to unfold: a long prompt, earlier tool calls, more subagents,
    /// or a long outcome.
    fn expandable(&self, internals: CliInternals) -> bool {
        let prompt = folds(self.data.prompt.lines().count(), PROMPT_PREVIEW);
        let activity = self.data.activity.len() > ACTIVITY_PREVIEW;
        let subagents = internals == CliInternals::Discovered
            && self
                .data
                .subagents
                .as_ref()
                .is_some_and(|section| section.children.len() > SUBAGENT_PREVIEW);
        let outcome = self.drawn.outcome_rows > self.drawn.outcome_preview;
        prompt || activity || subagents || outcome
    }

    /// Lays the sections out again at the last frame's width, so keys between frames act on
    /// what the next frame shows.
    fn relayout(&mut self, ctx: &ViewCtx<'_>) {
        if self.drawn.width == 0 {
            return;
        }
        let seen_at = self.seen_at.unwrap_or(ctx.now);
        let models = Models::default();
        self.drawn.total = self
            .detail_lines(self.drawn.width, ctx, seen_at, &models)
            .len();
    }

    /// The prompt's first line with text, and all of it once expanded.
    /// `cli` is the CLI that runs the agent's workflow itself, for an agent of one: it reports
    /// the prompt shortened.
    fn prompt(&self, width: u16, open: bool, cli: Option<&str>, theme: &Theme) -> Option<Block> {
        let text = sanitize(&self.data.prompt);
        let all: Vec<&str> = text.lines().collect();
        let dim = theme.style(Role::Dim);
        let inner = usize::from(width).saturating_sub(2);
        let indent = [Span::raw("  ")];
        let foldable = folds(all.len(), PROMPT_PREVIEW);
        let cli = cli.filter(|_| !text.trim().is_empty());
        // A session a handover started was given the main agent's brief as its prompt.
        let brief = if self.data.started_from.is_some() {
            "the brief, "
        } else {
            ""
        };
        let title = match cli {
            Some(cli) => format!("Prompt · shortened by {cli}"),
            None if foldable => format!("Prompt · {brief}{}", panes::count(all.len(), "line")),
            None if !brief.is_empty() => "Prompt · the brief".to_owned(),
            None => "Prompt".to_owned(),
        };
        let mut body = Vec::new();
        if open || !foldable {
            for line in &all {
                body.extend(wrap_text(line, dim, usize::from(width), &indent, &indent));
            }
        } else {
            // The first lines with text of the prompt's own words, past the frame every agent
            // of its role gets: a blank line between parts says nothing in a preview.
            let about = self.data.prompt_about.as_ref();
            let start = about.map_or(0, |about| about.first.min(all.len()));
            let shown: Vec<usize> = (start..all.len())
                .filter(|at| !all[*at].trim().is_empty())
                .take(PROMPT_PREVIEW)
                .collect();
            for at in &shown {
                body.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(cut(all[*at], inner), dim),
                ]));
            }
            let after = shown.last().map_or(start, |last| last + 1);
            // Every line the preview leaves out counts, the frame above it too.
            let hidden = all.len() - (after - start);
            if hidden > 0 {
                let mut more = more_lines(hidden);
                let parts = match about.filter(|about| !about.holds.is_empty()) {
                    Some(about) => about
                        .holds
                        .iter()
                        .map(|holds| sanitize_line(holds).trim().to_owned())
                        .collect(),
                    None => prompt_parts(&all[after..]),
                };
                if !parts.is_empty() {
                    more.push_str(&format!(": {}", argonauts_core::text::listed(&parts)));
                }
                body.push(Line::from(Span::styled(cut(&more, inner), dim)));
            }
        }
        let hint = (foldable || cli.is_some()).then(|| self.enter_word());
        Some((title, hint, body))
    }

    /// The workflow the agent's own CLI runs: its count and tokens, and its phases on one line.
    fn its_workflow(&self, theme: &Theme) -> Option<Block> {
        let nested = self.outline.as_ref()?.hosted_by(&self.data.row)?;
        let (title, body) = workflow_outline::its_workflow(nested, theme);
        Some((title, None, body))
    }

    /// The CLI that runs the agent's workflow itself, for an agent of one, by its name.
    fn inner_cli(&self, ctx_names: &DisplayNames) -> Option<String> {
        self.data
            .row
            .inner
            .as_ref()
            .map(|_| sanitize_line(&ctx_names.cli(&self.data.row.worker.agent)).into_owned())
    }

    /// What Enter does to the sections now.
    fn enter_word(&self) -> String {
        let word = if self.expanded { "collapse" } else { "expand" };
        format!("⏎ {word}")
    }

    /// The findings a review sent this agent back with, which it works on now.
    fn to_fix(&self, width: u16, theme: &Theme) -> Option<Block> {
        let findings = self.data.to_fix.as_ref()?;
        let title = format!(
            "Findings to fix · from {}, round {}",
            sanitize_line(&findings.from).trim(),
            findings.round
        );
        Some((
            title,
            None,
            finding_lines(&findings.items, &[], width, theme),
        ))
    }

    /// For a reviewer, the earlier round's findings it was given, each with its answer, and for
    /// a judge the latest review, which may have found nothing.
    fn earlier(&self, width: u16, names: &DisplayNames, theme: &Theme) -> Option<Block> {
        let earlier = self.data.earlier.as_ref()?;
        let mut title = format!(
            "Earlier findings · {}, round {}",
            sanitize_line(&earlier.from).trim(),
            earlier.round
        );
        if let Some(by) = earlier.answered_by.as_deref().map(str::trim) {
            title.push_str(&format!(" · answered by {}", sanitize_line(by)));
        }
        let text = theme.style(Role::Text);
        let dim = theme.style(Role::Dim);
        // What the review said of the work, `approved by GPT 6 Luna on Codex`, after `none`
        // when it found nothing.
        let mut said = Vec::new();
        if earlier.items.is_empty() {
            said.push(Span::styled("  none", text));
        }
        if let Some(verdict) = &earlier.verdict {
            said.push(Span::styled(
                if said.is_empty() { "  " } else { " · " },
                text,
            ));
            said.push(Span::styled(
                sanitize_line(&verdict.said).trim().to_owned(),
                theme.style(tone_role(verdict.tone)),
            ));
            let cli = sanitize_line(&names.cli(&verdict.by.agent)).into_owned();
            let by = match names.model(&verdict.by.agent, verdict.by.model.as_deref()) {
                Some(model) => format!(" by {} on {cli}", sanitize_line(&model)),
                None => format!(" by {cli}"),
            };
            said.push(Span::styled(by, dim));
        }
        let mut body = Vec::new();
        if !said.is_empty() {
            body.push(Line::from(truncate_spans(&said, usize::from(width))));
        }
        let summary = earlier.verdict.as_ref().and_then(|v| v.summary.as_deref());
        if let Some(summary) = summary.map(|summary| sanitize_line(summary).trim().to_owned()) {
            body.push(Line::from(Span::styled(
                cut(&format!("  \"{summary}\""), usize::from(width)),
                dim,
            )));
        }
        body.extend(finding_lines(
            &earlier.items,
            &earlier.answers,
            width,
            theme,
        ));
        Some((title, None, body))
    }

    fn activity(&self, width: u16, open: bool, cli: Option<&str>, theme: &Theme) -> Option<Block> {
        let activity = &self.data.activity;
        let calls = self.data.tool_calls.max(activity.len() as u32);
        let dim = theme.style(Role::Dim);
        if activity.is_empty() {
            let (title, said) = match calls {
                0 if matches!(
                    self.data.row.status,
                    RowStatus::Done | RowStatus::Failed | RowStatus::Skipped
                ) =>
                {
                    ("Activity".to_owned(), Some("No tool calls."))
                }
                0 => ("Activity".to_owned(), Some("No tool calls yet.")),
                calls => (
                    format!("Activity · {}", panes::count(calls as usize, "tool call")),
                    None,
                ),
            };
            let body = said
                .map(|said| Line::from(Span::styled(format!("  {said}"), dim)))
                .into_iter()
                .collect();
            return Some((title, None, body));
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
        // An agent its CLI runs itself reports its count of calls and its last call alone,
        // and not how that went, so the title says so and Enter has no more of them to show.
        if let Some(cli) = cli {
            let title = if calls > 1 {
                format!(
                    "Activity · {} · {cli} names only the last",
                    panes::count(calls as usize, "tool call")
                )
            } else {
                title
            };
            let running = self.data.row.status == RowStatus::Running;
            let (mark, _) = workflow_rows::glyph(RowStatus::Running, false, theme);
            let body = activity[activity.len() - shown..]
                .iter()
                .map(|call| {
                    let tool = sanitize_line(&call.tool).trim().to_owned();
                    let summary = sanitize_line(&call.summary).trim().to_owned();
                    let room = usize::from(width).saturating_sub(4 + str_width(&tool) + 2 + 10);
                    let text = if summary.is_empty() {
                        tool
                    } else {
                        format!("{tool}({})", cut(&summary, room))
                    };
                    let mut spans = vec![
                        Span::styled(format!("  {mark} "), theme.style(Role::Faint)),
                        Span::styled(text, theme.style(Role::Text)),
                    ];
                    if running {
                        spans.push(Span::styled(" · running", dim));
                    }
                    Line::from(spans)
                })
                .collect();
            return Some((title, None, body));
        }
        let running = self.data.row.status == RowStatus::Running;
        let ascii = theme.glyphs.is_ascii();
        let body = activity[activity.len() - shown..]
            .iter()
            .map(|call| {
                // Claude Code's marks for a call: green once it answered, red with `error`,
                // grey while it runs, and hollow when no answer ever came.
                let (glyph, role, said) = match (call.ok, running) {
                    (Some(true), _) => (if ascii { "*" } else { "⏺" }, Role::Success, None),
                    (Some(false), _) => {
                        (if ascii { "x" } else { "⏺" }, Role::Danger, Some("error"))
                    }
                    (None, true) => (if ascii { ">" } else { "⏺" }, Role::Faint, Some("running")),
                    (None, false) => (
                        if ascii { "o" } else { "◌" },
                        Role::Faint,
                        Some("no result"),
                    ),
                };
                let said_style = match role {
                    Role::Danger => theme.style(Role::Danger),
                    _ => dim,
                };
                let note = call.note.as_deref().map(sanitize_line);
                let said = said
                    .into_iter()
                    .map(str::to_owned)
                    .chain(note.map(|note| note.trim().to_owned()))
                    .map(|said| format!(" · {said}"))
                    .collect::<String>();
                let tool = sanitize_line(&call.tool).trim().to_owned();
                let summary = sanitize_line(&call.summary).trim().to_owned();
                // The call keeps its closing parenthesis when its summary is cut.
                let taken = 4 + str_width(&tool) + 2 + str_width(&said);
                let room = usize::from(width).saturating_sub(taken);
                let text = if summary.is_empty() {
                    tool
                } else {
                    format!("{tool}({})", cut(&summary, room))
                };
                Line::from(vec![
                    Span::styled(format!("  {glyph} "), theme.style(role)),
                    Span::styled(text, theme.style(Role::Text)),
                    Span::styled(said, said_style),
                ])
            })
            .collect();
        let more = activity.len() > ACTIVITY_PREVIEW || (shown as u32) < calls;
        Some((title, more.then(|| self.enter_word()), body))
    }

    fn diff(&self) -> Option<Block> {
        let diff = self.data.diff?;
        let title = format!(
            "Diff · {} · +{} -{}",
            panes::count(diff.files_changed as usize, "file"),
            diff.insertions,
            diff.deletions
        );
        Some((title, Some("d open".to_owned()), Vec::new()))
    }

    /// `d`: the task's diff in the pager.
    fn open_diff(&self, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let task = ctx
            .state
            .session(&self.data.run, &self.data.row.session)
            .and_then(|session| session.task.clone());
        match task {
            Some(task) => ViewOutcome::Request(ViewRequest::Diff {
                run: self.data.run.clone(),
                task,
                scope: DiffScope::Task,
            }),
            None => ViewOutcome::Notice("This agent has no task, so no diff".to_owned()),
        }
    }

    fn subagents(
        &self,
        width: u16,
        open: bool,
        internals: CliInternals,
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
            CliInternals::Hidden => None,
            CliInternals::Summary => {
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
                Some((title, None, Vec::new()))
            }
            CliInternals::Discovered => {
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
                    let more = format!("  … {} more", children.len() - shown);
                    body.push(Line::from(Span::styled(more, theme.style(Role::Dim))));
                }
                let more = children.len() > SUBAGENT_PREVIEW;
                Some((title, more.then(|| self.enter_word()), body))
            }
        }
    }

    /// What the agent ended with: its plan as a table when it made one, then its report or
    /// final words. Also how many of the rows are the plan's, with the blank row after it.
    fn outcome_rows(&self, width: u16, ctx: &ViewCtx<'_>) -> (Option<Vec<Line<'static>>>, usize) {
        let mut rows = self.plan_rows(width, ctx).unwrap_or_default();
        let mut plan_rows = rows.len();
        if let Some(source) = self.data.outcome.as_deref() {
            if !rows.is_empty() {
                rows.push(Line::default());
                plan_rows += 1;
            }
            let render_ctx = RenderCtx::new(width, ctx.theme, Clock::still(ctx.now));
            rows.extend(render_markdown(source, &render_ctx, &Indent::spaces(2)));
        }
        ((!rows.is_empty()).then_some(rows), plan_rows)
    }

    /// The plan's table, two cells in as the rest of the outcome is. What each task asks for
    /// is in the opened agent, where the right arrow expands it.
    fn plan_rows(&self, width: u16, ctx: &ViewCtx<'_>) -> Option<Vec<Line<'static>>> {
        let draw = structured::Draw {
            names: &ctx.state.names,
            theme: ctx.theme,
            width: usize::from(width).saturating_sub(2),
            expanded: false,
            hints: false,
        };
        let (mut rows, _) = structured::lines(self.data.plan.as_ref()?, &draw)?;
        for row in &mut rows {
            row.spans.insert(0, Span::raw("  "));
        }
        Some(rows)
    }

    /// A reviewer's verdict and findings are its outcome. Any other agent's is what it ended
    /// with, folded when it is long, and until it ends a line on where it stands.
    fn outcome(
        &self,
        rendered: Option<Vec<Line<'static>>>,
        open: bool,
        width: u16,
        theme: &Theme,
    ) -> Block {
        let dim = theme.style(Role::Dim);
        if let Some(review) = &self.data.review {
            let mut title = format!("Outcome · {}", sanitize_line(&review.verdict).trim());
            if !review.findings.is_empty() {
                title.push_str(&format!(" · {}", finding_counts(&review.findings)));
            }
            let mut body = finding_lines(&review.findings, &[], width, theme);
            // With no findings, what it gave is its own lines, a challenger's points, each one
            // row cut at the pane as findings are.
            if body.is_empty() {
                let text = theme.style(Role::Text);
                body = self
                    .data
                    .outcome
                    .as_deref()
                    .unwrap_or_default()
                    .lines()
                    .map(|line| sanitize_line(line).trim().to_owned())
                    .filter(|line| !line.is_empty())
                    .map(|line| {
                        let span = Span::styled(format!("  {line}"), text);
                        Line::from(truncate_spans(&[span], usize::from(width)))
                    })
                    .collect();
            }
            if body.is_empty() {
                body = rendered.unwrap_or_default();
            }
            return (title, None, body);
        }
        // A failed agent's outcome is why it failed, in the failure's colour, as Claude Code
        // draws every failed agent's.
        let failed = theme.style(Role::Danger);
        let why = self
            .data
            .row
            .note
            .as_ref()
            .filter(|note| note.tone == Tone::Bad && self.data.row.status == RowStatus::Failed)
            .map(|note| {
                argonauts_core::text::sentence_case(sanitize_line(note.text.trim()).trim())
            });
        if let Some(why) = why.filter(|why| !why.is_empty()) {
            let indent = [Span::raw("  ")];
            let body = wrap_text(&why, failed, usize::from(width), &indent, &indent);
            return ("Outcome".to_owned(), None, body);
        }
        let Some(rendered) = rendered else {
            let said = match self.data.row.status {
                RowStatus::Running | RowStatus::NeedsYou => "Still running…",
                RowStatus::Waiting | RowStatus::Queued => "Waiting for an agent slot.",
                RowStatus::Paused => {
                    &format!("{}, with its work so far kept.", held(&self.data.row))
                }
                RowStatus::Skipped => "Skipped.",
                RowStatus::Done | RowStatus::Failed => "It ended without a report.",
            };
            let line = Line::from(Span::styled(format!("  {said}"), dim));
            return ("Outcome".to_owned(), None, vec![line]);
        };
        let mut rendered = rendered;
        // What a failed agent ended with reads in the failure's colour, as in Claude Code's
        // own view.
        if self.data.row.status == RowStatus::Failed {
            for span in rendered.iter_mut().flat_map(|line| &mut line.spans) {
                span.style = failed;
            }
        }
        let preview = self.drawn.outcome_preview;
        if rendered.len() <= preview {
            return ("Outcome".to_owned(), None, rendered);
        }
        // Counted as drawn, so a long paragraph isn't `1 line` over five rows.
        let title = format!("Outcome · {}", panes::count(rendered.len(), "line"));
        if open {
            return (title, Some(self.enter_word()), rendered);
        }
        let hidden = rendered.len() - preview;
        let mut body: Vec<Line<'static>> = rendered.into_iter().take(preview).collect();
        body.push(Line::from(Span::styled(more_lines(hidden), dim)));
        (title, Some(self.enter_word()), body)
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
                        // Ctrl+Enter sends it now, as Claude Code's own key does.
                        now: key.modifiers.contains(KeyModifiers::CONTROL),
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

/// A section's title, the key that acts on it when one does, and its body.
type Block = (String, Option<String>, Vec<Line<'static>>);

/// Whether a section of `lines` lines folds to `preview` of them.
fn folds(lines: usize, preview: usize) -> bool {
    lines >= preview + WORTH_FOLDING
}

/// The parts a prompt's hidden lines hold, from their headings: `## Your request` reads
/// `your request`.
fn prompt_parts(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| line.trim_start().strip_prefix('#'))
        .map(|heading| heading.trim_start_matches('#').trim())
        .filter(|heading| !heading.is_empty())
        .map(argonauts_core::text::clause_case)
        .collect()
}

/// `  … 3 more lines` under a section's preview.
fn more_lines(count: usize) -> String {
    format!(
        "  … {count} more {}",
        if count == 1 { "line" } else { "lines" }
    )
}

/// `Prompt · 9 lines · ⏎ expand`, the key last when one acts on the section.
fn section_title(title: &str, hint: Option<&str>, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("{title}{}", hint_words(hint.map(str::to_owned), theme)),
        theme.style(Role::Text),
    ))
}

/// ` · ⏎ expand` after a section's title, when a key acts on it.
fn hint_words(hint: Option<String>, theme: &Theme) -> String {
    hint.map(|hint| format!(" · {}", panes::key_names(&hint, theme)))
        .unwrap_or_default()
}

/// What a finding is called: blocking, which the change can't merge with, or a note.
fn severity_word(severity: Severity) -> (&'static str, Role) {
    match severity {
        Severity::High => (BLOCKING, Role::Danger),
        Severity::Medium | Severity::Low => ("note", Role::Dim),
    }
}

const BLOCKING: &str = "blocking";

/// `1 blocking · 2 notes`, or nothing for no findings.
fn finding_counts(findings: &[Finding]) -> String {
    let blocking = findings
        .iter()
        .filter(|finding| finding.severity == Severity::High)
        .count();
    let notes = findings.len() - blocking;
    let mut said = Vec::new();
    if blocking > 0 {
        said.push(format!("{blocking} {BLOCKING}"));
    }
    if notes > 0 {
        said.push(panes::count(notes, "note"));
    }
    said.join(" · ")
}

/// A finding a row: blocking or a note, in its color, the file and line it is about, and what
/// it says, each column as wide as its widest cell. The first column is always as wide as
/// `blocking`, so every section's findings line up. An answer, when a finding had one, goes
/// dim under it.
fn finding_lines(
    findings: &[Finding],
    answers: &[String],
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let dim = theme.style(Role::Dim);
    let place = |finding: &Finding| sanitize_line(&finding.place).trim().to_owned();
    let severity_width = str_width(BLOCKING);
    let place_width = findings
        .iter()
        .map(|finding| str_width(&place(finding)))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (at, finding) in findings.iter().enumerate() {
        let (severity, role) = severity_word(finding.severity);
        let mut spans = vec![Span::styled(
            format!("  {} ", left(severity, severity_width)),
            theme.style(role),
        )];
        if place_width > 0 {
            spans.push(Span::styled(
                format!(" {}  ", left(&place(finding), place_width)),
                dim,
            ));
        }
        spans.push(Span::styled(
            sanitize_line(&finding.text).trim().to_owned(),
            theme.style(Role::Text),
        ));
        lines.push(Line::from(truncate_spans(&spans, usize::from(width))));
        let answer = answers
            .get(at)
            .map(|answer| sanitize_line(answer).trim().to_owned());
        if let Some(answer) = answer.filter(|answer| !answer.is_empty()) {
            let indent = " ".repeat(2 + severity_width + 2);
            let line = Span::styled(format!("{indent}answer: {answer}"), dim);
            lines.push(Line::from(truncate_spans(&[line], usize::from(width))));
        }
    }
    lines
}

/// `message > text▏  ⏎ send · esc cancel`, keeping the end of what you type in view. The hints
/// go first when the line is short.
fn steer_line(text: &str, width: usize, theme: &Theme) -> Line<'static> {
    let dim = theme.style(Role::Dim);
    let marks = Marks::for_theme(theme);
    let prompt = format!("{} ", theme.glyphs.prompt);
    let hints = panes::key_names("  ⏎ send · esc cancel", theme);
    let margin = str_width("message ") + str_width(&prompt) + str_width(marks.cursor);
    let with_hints = width.saturating_sub(margin + str_width(&hints));
    let text = sanitize_line(text);
    let (room, hints) = if with_hints >= str_width(&text) || with_hints >= MIN_STEER {
        (with_hints, hints)
    } else {
        (width.saturating_sub(margin), String::new())
    };
    Line::from(vec![
        Span::styled("message ", theme.style(Role::Label)),
        Span::styled(prompt, theme.style(Role::Question)),
        Span::raw(list::tail(&text, room)),
        Span::styled(marks.cursor, dim),
        Span::styled(hints, dim),
    ])
}

/// A child's name, or its kind when the agent gave none.
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
    // A subagent its CLI has started counts as running here, as the CLI's own list has it.
    let (state, word) = match child.status {
        SubagentStatus::Started | SubagentStatus::Running => (State::Working, "running"),
        SubagentStatus::Done => (State::Done, "done"),
        SubagentStatus::Failed => (State::Failed, "failed"),
        SubagentStatus::Unknown => (State::Waiting, "unknown"),
    };
    let (glyph, role) = states::mark(state, theme);
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
    let opening = vec![
        Span::raw("  "),
        Span::styled(panes::pad_glyph(glyph, marks.slot()), theme.style(role)),
        Span::raw(" "),
        Span::styled(left(label, label_width), theme.style(Role::Text)),
        Span::raw("  "),
    ];
    fit_parts(opening, &parts, usize::from(width))
}

/// Each agent's label, and its agent's name when others in the phase share the label, so six
/// drafting agents read apart. Any still alike number their label, which a narrow pane keeps when
/// it cuts the name. Each agent's label, numbered when the label and the agent both repeat, and
/// whether the label alone repeats, since then only the agent's name tells the rows apart.
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

/// The cursor, the glyph, and the space after each, before a label in the list of agents.
const LIST_MARGIN: usize = 4;

/// Under the state, the counts as Claude Code gives them, `57.8k tok · 23 tool calls · 2m 40s`,
/// then how full the agent's context is. Nothing for an agent that has none yet.
fn counts_line(
    row: &AgentRow,
    tool_calls: u32,
    ticking: u64,
    width: u16,
    theme: &Theme,
) -> Option<Line<'static>> {
    let mut counts = Vec::new();
    if row.tokens > 0 {
        counts.push(panes::tokens(row.tokens));
    }
    if tool_calls > 0 {
        counts.push(panes::count(tool_calls as usize, "tool call"));
    }
    if !matches!(row.status, RowStatus::Waiting | RowStatus::Queued) && !row.left_out {
        let elapsed = Duration::from_millis(row.elapsed_ms + ticking);
        counts.push(format::duration(elapsed));
    }
    if let Some(context) = row.context {
        counts.push(match (context.of_window(), context.window) {
            (Some(share), Some(window)) => format!(
                "context {} of {}",
                format::percent(share),
                format::window(window)
            ),
            _ => format!("context {}", format::tokens(context.used)),
        });
    }
    let text = cut(&counts.join(" · "), usize::from(width));
    (!text.is_empty()).then(|| Line::from(Span::styled(text, theme.style(Role::Dim))))
}

/// What a held agent waits on, as its row says it: `Paused` for a pause you asked for, else
/// what the run waits for, such as `Stopped with the run` or `Waits for Claude Code`.
fn held(row: &AgentRow) -> String {
    row.note
        .as_ref()
        .and_then(|note| note.text.split(" · ").next())
        .map(str::trim)
        .filter(|held| !held.is_empty())
        .map(argonauts_core::text::sentence_case)
        .unwrap_or_else(|| "Paused".to_owned())
}

/// `⏺ Running · Sonnet 5.5 on Claude Code · worker · worktree · round 2 (sent back)`, as
/// Claude Code heads an open agent: the state, then the model in its own color, its CLI, and
/// what Argonauts Code knows of the agent. A fallback says whose place it takes.
fn status_line(
    row: &AgentRow,
    facts: &[String],
    said_below: bool,
    models: &Models,
    width: u16,
    ctx: &ViewCtx<'_>,
) -> Line<'static> {
    let theme = ctx.theme;
    let dim = theme.style(Role::Dim);
    let names = &ctx.state.names;
    let (glyph, role) = match row.left_out {
        true => (
            workflow_rows::glyph(RowStatus::Failed, false, theme).0,
            Role::Faint,
        ),
        false => workflow_rows::glyph(row.status, row.fallback_for.is_some(), theme),
    };
    let word = match row.status {
        _ if row.left_out => "Left out".to_owned(),
        RowStatus::Waiting => "Waiting".to_owned(),
        RowStatus::Queued => "Queued".to_owned(),
        RowStatus::Skipped => "Skipped".to_owned(),
        RowStatus::Running => "Running".to_owned(),
        RowStatus::Paused => held(row),
        RowStatus::NeedsYou => "Needs you".to_owned(),
        RowStatus::Done => "Done".to_owned(),
        RowStatus::Failed => "Failed".to_owned(),
    };
    // As Claude Code draws it, a failure's word is in the failure's colour.
    let word_style = match row.status {
        RowStatus::Failed if !row.left_out => theme.style(role),
        _ => theme.style(Role::Text),
    };
    let mut spans = vec![
        Span::styled(format!("{glyph} "), theme.style(role)),
        Span::styled(word, word_style),
        Span::styled(" · ", theme.style(Role::Text)),
    ];
    let cli = sanitize_line(&names.cli(&row.worker.agent)).into_owned();
    // The model a job left out was meant for never ran, so it's dim.
    let model_style = if row.left_out {
        dim
    } else {
        models.style(&row.worker, ctx)
    };
    match names.model(&row.worker.agent, row.worker.model.as_deref()) {
        Some(model) => {
            spans.push(Span::styled(
                sanitize_line(&model).into_owned(),
                model_style,
            ));
            spans.push(Span::styled(format!(" on {cli}"), dim));
        }
        None => spans.push(Span::styled(cli, model_style)),
    }
    for fact in facts {
        let fact = sanitize_line(fact);
        if !fact.trim().is_empty() {
            spans.push(Span::styled(format!(" · {}", fact.trim()), dim));
        }
    }
    if let Some(first) = row.fallback_for.as_ref().filter(|_| !said_below) {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            format!("in place of {}", sanitize_line(&names.short(first))),
            theme.style(Role::Warn),
        ));
    }
    if let Some(by) = review_needed_by(ctx, &row.session) {
        spans.push(Span::styled(" · ", dim));
        spans.push(review_needed_words(&by, names, theme));
    }
    Line::from(truncate_spans(&spans, usize::from(width)))
}

/// What an orchestrator began from, as a line and a dim one under it: `Forked from your main
/// session · 48.2k tokens of conversation`, or `Started from the brief · written by Opus 5.5
/// from this conversation`.
fn started_from_lines(from: &StartedFrom, width: u16, ctx: &ViewCtx<'_>) -> Vec<Line<'static>> {
    const BESIDE: &str = "your conversation goes on beside the run";
    let theme = ctx.theme;
    let (said, under) = match from {
        StartedFrom::Fork { tokens } => {
            let mut said = "Forked from your main session".to_owned();
            if let Some(tokens) = tokens {
                said.push_str(&format!(
                    " · {} tokens of conversation",
                    crate::format::context_tokens(*tokens)
                ));
            }
            (
                said,
                format!("the brief came as its first message · {BESIDE}"),
            )
        }
        StartedFrom::Brief(brief) => {
            let by = ctx.state.names.short(&brief.written_by);
            let said = format!(
                "Started from the brief · written by {} from this conversation",
                sanitize_line(&by)
            );
            let under = match brief.why.as_deref() {
                Some(why) => format!("{} · {BESIDE}", sanitize_line(why).trim()),
                None => BESIDE.to_owned(),
            };
            (said, under)
        }
    };
    let width = usize::from(width);
    vec![
        Line::from(Span::styled(cut(&said, width), theme.style(Role::Text))),
        Line::from(Span::styled(
            cut(&format!("  {under}"), width),
            theme.style(Role::Dim),
        )),
    ]
}

/// Why the job came to this model: `In place of Qwen3 Coder 30B, whose 64k window can't hold
/// this change`, the model it stands in for in that model's colour, or `Left out, because agy
/// is signed out`, each with what more is known under it.
fn moved_lines(
    moved: &Moved,
    models: &Models,
    width: u16,
    ctx: &ViewCtx<'_>,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let names = &ctx.state.names;
    let text = theme.style(Role::Text);
    let (first, more) = match moved {
        Moved::FallbackFor {
            asked,
            which,
            lines,
        } => {
            let model = names
                .model(&asked.agent, asked.model.as_deref())
                .unwrap_or_else(|| names.cli(&asked.agent));
            // Why reads on from the model's name: "whose window can't hold…", "which crashed".
            let which = sanitize_line(which);
            let which = which.trim();
            let clause = if which.starts_with("whose ") || which.starts_with("which ") {
                which.to_owned()
            } else {
                format!("which {which}")
            };
            let spans = vec![
                Span::styled("In place of ", text),
                Span::styled(sanitize_line(&model).into_owned(), models.style(asked, ctx)),
                Span::styled(format!(", {clause}"), text),
            ];
            (spans, lines)
        }
        Moved::LeftOut { because, lines } => (
            vec![Span::styled(
                format!("Left out, because {}", sanitize_line(because).trim()),
                text,
            )],
            lines,
        ),
    };
    let mut out = vec![Line::from(truncate_spans(&first, usize::from(width)))];
    for line in more {
        out.push(Line::from(Span::styled(
            cut(
                &format!("  {}", sanitize_line(line).trim()),
                usize::from(width),
            ),
            theme.style(Role::Dim),
        )));
    }
    out
}

/// `Asked for by Claude Code · its agent review:final, on Haiku 5.5`, and under it what that
/// agent did with its own tokens and time: a hand-over by design, told once, here.
fn asked_for_lines(
    asked: &AskedFor,
    row: &AgentRow,
    models: &Models,
    width: u16,
    ctx: &ViewCtx<'_>,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let names = &ctx.state.names;
    let text = theme.style(Role::Text);
    let by = Line::from(truncate_spans(
        &[
            Span::styled(
                format!(
                    "Asked for by {} · its agent {}, on ",
                    sanitize_line(&names.cli(&asked.cli)),
                    sanitize_line(asked.label.trim()),
                ),
                text,
            ),
            Span::styled(
                sanitize_line(&names.short(&asked.worker)).into_owned(),
                models.style(&asked.worker, ctx),
            ),
        ],
        usize::from(width),
    ));
    // While the one asked still reads, the agent that asked waits for its verdict.
    let did = if matches!(row.status, RowStatus::Running | RowStatus::Queued) {
        "the change and waits for the verdict"
    } else {
        "the change, waited, and passed the verdict back"
    };
    let mut what = format!(
        "  it gave {} {did}",
        sanitize_line(&names.cli(&row.worker.agent)),
    );
    // Claude Code reports an agent's tokens only once it ends.
    if asked.tokens > 0 {
        what.push_str(&format!(" · {}", panes::tokens(asked.tokens)));
    }
    what.push_str(&format!(" · {}", panes::elapsed(asked.elapsed_ms)));
    let did = Line::from(truncate_spans(
        &[Span::styled(what, theme.style(Role::Dim))],
        usize::from(width),
    ));
    vec![by, did]
}

pub(super) fn cli_internals(ctx: &ViewCtx<'_>) -> CliInternals {
    ctx.state
        .ui
        .as_ref()
        .map_or(CliInternals::Discovered, |ui| ui.panel.subagents)
}

/// Who must still review the work a weaker fallback did on the agent's task: the strongest
/// model before it. Said once, on the task's newest worker, from the run's folded state.
pub(super) fn review_needed_by(ctx: &ViewCtx<'_>, session: &SessionKey) -> Option<ModelRef> {
    ctx.state.runs.values().find_map(|run| {
        let task = run.sessions.get(session)?.task.as_ref()?;
        let debt = run.tasks.get(task)?.debt.last()?;
        let newest = run
            .sessions
            .values()
            .filter(|other| other.task.as_ref() == Some(task) && other.role == RoleName::Worker)
            .max_by_key(|other| (other.started, other.key.clone()))?;
        (newest.key == *session).then(|| debt.needs.clone())
    })
}

/// `needs a review by Opus 5.5`, in the warning colour: a word, with no mark of its own.
pub(super) fn review_needed_words(
    by: &ModelRef,
    names: &DisplayNames,
    theme: &Theme,
) -> Span<'static> {
    Span::styled(
        format!("needs a review by {}", sanitize_line(&names.short(by))),
        theme.style(Role::Warn),
    )
}

impl View for AgentView {
    fn docked(&self) -> bool {
        true
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        if let Some(follow) = &self.follow {
            follow.show(Shown::agent(self.data.run.clone(), self.selected_row()));
        }
        let heading = Heading {
            name: self.data.row.label.clone(),
            // The pane names the agent and its run, so the header says what the run is for.
            description: ctx
                .state
                .runs
                .get(&self.data.run)
                .map(|run| sanitize_line(&run.title).trim().to_owned())
                .unwrap_or_default(),
            ..Heading::default()
        };
        self.render_frame(area, buf, ctx, &heading, &[], &Models::default());
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
        let internals = cli_internals(ctx);
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
        if let Some(restart) = self.restart.take() {
            match key.code {
                KeyCode::Char('r') if plain => return ViewOutcome::Send(restart.command),
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
                ViewOutcome::Request(ViewRequest::agent(&self.data.run, self.selected_row()))
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
            KeyCode::Enter => {
                if self.expanded || self.expandable(internals) {
                    self.expanded = !self.expanded;
                    self.relayout(ctx);
                    self.scroll_by(0);
                }
                ViewOutcome::Consumed
            }
            KeyCode::Char('m' | 't' | 'c' | 'x' | 'r')
                if plain && self.loaded() && self.data.row.inner.is_some() =>
            {
                ViewOutcome::Notice(super::inner_why(ctx.state, &self.data.row))
            }
            KeyCode::Char('g') if plain && self.loaded() => self.open_goal(ctx),
            KeyCode::Char('m') if plain && self.loaded() => {
                match super::steer_refusal(ctx.state, &self.data.run, &self.data.row.session) {
                    Some(why) => ViewOutcome::Notice(why),
                    None => {
                        self.steer = Some(String::new());
                        ViewOutcome::Consumed
                    }
                }
            }
            KeyCode::Char('t') if plain && self.loaded() => {
                super::take_over(ctx.state, &self.data.run, &self.data.row.session)
            }
            KeyCode::Char('c') if plain && self.loaded() => super::carry_on(
                ctx.state,
                &self.data.run,
                &self.data.row.session,
                self.running(),
            ),
            KeyCode::Char('d') if plain && self.loaded() => self.open_diff(ctx),
            KeyCode::Char('x') if plain && self.loaded() => {
                if !self.running() {
                    let label = sanitize_line(&self.data.row.label).trim().to_owned();
                    return ViewOutcome::Notice(format!("{label} already finished"));
                }
                self.confirm_stop = true;
                ViewOutcome::Consumed
            }
            KeyCode::Char('r') if plain && self.loaded() => {
                match super::restart(ctx.state, &self.data.run, &self.data.row) {
                    Ok(restart) => {
                        self.restart = Some(restart);
                        ViewOutcome::Consumed
                    }
                    Err(why) => ViewOutcome::Notice(why),
                }
            }
            KeyCode::Char('p') if plain => super::pause_key(&self.data.run, ctx),
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
                fallback_for: None,
                answered: false,
                tries: Vec::new(),
                note: None,
                inner: None,
                left_out: false,
                fallback_why: None,
            },
            prompt: "Build the history cells.".into(),
            activity: Vec::new(),
            tool_calls: 0,
            diff: None,
            subagents: None,
            outcome: Some(outcome.into()),
            goal: None,
            plan: None,
            facts: Vec::new(),
            to_fix: None,
            asked_for: None,
            earlier: None,
            moved: None,
            given: None,
            prompt_about: None,
            review: None,
            started_from: None,
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
            try_number: None,
            count: Some(argonauts_core::views::PhaseCount {
                done: 0,
                total: rows.len() as u32,
            }),
            rows,
            note: None,
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

    /// A run that's going, with the agent's session working on a task.
    fn live_state() -> AppState {
        let mut state = AppState::default();
        let run_id = RunId::new("run-01").unwrap();
        let mut run = crate::state::RunState::new(run_id.clone());
        run.started = Some(time::OffsetDateTime::now_utc());
        let mut session = crate::state::SessionState::new(
            SessionKey::parse("s-002").unwrap(),
            "claude-code/opus".parse().unwrap(),
            "claude-code",
            argonauts_core::names::RoleName::Worker,
            time::OffsetDateTime::now_utc(),
        );
        session.task = Some(argonauts_core::TaskId::new("task-002").unwrap());
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
            assert!(footer.starts_with("↑↓  agent"), "{footer}");
            let end = footer.trim_end();
            assert!(
                end.ends_with("esc back") || end.ends_with("s save"),
                "{footer}"
            );
            assert!(!footer.contains('…'), "{footer}");
        }
        let text = draw_in(&mut in_phase("Done."), 120, &state);
        let footer = footer(&text);
        for hint in ["m message", "x stop", "r restart", "p pause", "s save"] {
            assert!(footer.contains(hint), "{hint}: {footer}");
        }
    }

    /// An agent of a run read from its journal, which this session never watched, ended with
    /// its run as the workflow view says: it can be carried on, and nothing is left to pause.
    #[test]
    fn an_agent_of_a_run_read_from_its_journal_offers_to_carry_on() {
        let mut done = data("Done.");
        done.row.status = RowStatus::Done;
        let row = done.row.clone();
        let mut view = AgentView::in_phase(done, &phase(vec![WorkflowRow::Agent(row)]));
        view.run_state = Some(argonauts_core::views::WorkflowState::Done);
        let text = draw_in(&mut view, 124, &AppState::default());
        let keys = footer(&text);
        assert!(keys.contains("c continue"), "{keys}");
        assert!(!keys.contains("p pause"), "{keys}");
    }

    /// The keys are Claude Code's for an open agent: stop and restart only while it runs, and
    /// nothing to pause once its run ended. One that can't act says why when pressed anyway.
    /// Enter's word says what it does now.
    #[test]
    fn the_keys_hold_still_and_say_why_when_they_cant_act() {
        let mut done = data(&"A line of the outcome.\n".repeat(12));
        done.row.status = RowStatus::Done;
        done.diff = Some(DiffStat::default());
        let row = done.row.clone();
        let mut view = AgentView::in_phase(done, &phase(vec![WorkflowRow::Agent(row)]));
        let mut finished = live_state();
        for run in finished.runs.values_mut() {
            run.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        }
        let text = draw_in(&mut view, 124, &finished);
        assert_eq!(
            footer(&text).trim(),
            "↑↓  agent · j/k scroll · ⏎ expand · m message · c continue · esc back · s save"
        );
        // Pressed anyway, they say what happened and what to do instead.
        for c in ['t', 'm', 'r'] {
            let ViewOutcome::Notice(why) =
                key_in(&mut view, KeyCode::Char(c), KeyModifiers::NONE, &finished)
            else {
                panic!("{c} says why");
            };
            assert!(why.starts_with("That run already merged"), "{why}");
            assert!(why.contains("type /follow-up"), "{why}");
        }
        let ViewOutcome::Notice(why) =
            key_in(&mut view, KeyCode::Char('x'), KeyModifiers::NONE, &finished)
        else {
            panic!("x says why");
        };
        assert!(why.ends_with("already finished"), "{why}");
        assert!(view.steer.is_none() && !view.confirm_stop);

        key_in(&mut view, KeyCode::Enter, KeyModifiers::NONE, &finished);
        let text = draw_in(&mut view, 124, &finished);
        assert!(footer(&text).contains("⏎ collapse"), "{text}");
        assert!(text.contains("Outcome · 12 lines · ⏎ collapse"), "{text}");
        key_in(&mut view, KeyCode::Enter, KeyModifiers::NONE, &finished);
        let text = draw_in(&mut view, 124, &finished);
        assert!(footer(&text).contains("⏎ expand"), "{text}");
    }

    /// `r` in a running agent asks once more, and the second `r` has the run start the
    /// agent's task over with the worker it has. Any other key drops the ask.
    #[test]
    fn r_twice_starts_the_agents_task_over() {
        let mut view = in_phase("");
        let state = live_state();
        let press =
            |view: &mut AgentView, code: KeyCode| key_in(view, code, KeyModifiers::NONE, &state);
        assert!(matches!(
            press(&mut view, KeyCode::Char('r')),
            ViewOutcome::Consumed
        ));
        let text = draw_in(&mut view, 124, &state);
        assert_eq!(
            footer(&text).trim(),
            "r again to restart build:b · esc to keep"
        );
        // Scrolling is no answer, so the ask goes.
        press(&mut view, KeyCode::Char('j'));
        let text = draw_in(&mut view, 124, &state);
        assert!(footer(&text).contains("r restart"), "{text}");

        press(&mut view, KeyCode::Char('r'));
        let outcome = press(&mut view, KeyCode::Char('r'));
        let ViewOutcome::Send(UiCommand::Reassign { run, task, to }) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(run.as_str(), "run-01");
        assert_eq!(task.as_str(), "task-002");
        // The screen holds no worker for the task, so the row's own is who restarts it.
        assert_eq!(to.to_string(), "claude-code/opus");
    }

    /// A finished agent's session carries on in the conversation once its run ended. Until then the
    /// run owns it, and a running one is taken over.
    #[test]
    fn c_carries_a_finished_agents_session_on_once_its_run_ended() {
        let mut done = data("Done.");
        done.row.status = RowStatus::Done;
        let row = done.row.clone();
        let mut view = AgentView::in_phase(done, &phase(vec![WorkflowRow::Agent(row)]));
        // Its run still goes, and its next round on the task resumes the same session.
        let going = live_state();
        let text = draw_in(&mut view, 120, &going);
        assert!(!footer(&text).contains("c continue"), "{text}");
        match key_in(&mut view, KeyCode::Char('c'), KeyModifiers::NONE, &going) {
            ViewOutcome::Notice(text) => {
                assert!(
                    text.starts_with("run-01 hasn't ended, so it still owns"),
                    "{text}"
                );
            }
            other => panic!("{other:?}"),
        }

        let mut ended = live_state();
        for run in ended.runs.values_mut() {
            run.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        }
        let text = draw_in(&mut view, 120, &ended);
        assert!(footer(&text).contains("c continue"), "{text}");
        match key_in(&mut view, KeyCode::Char('c'), KeyModifiers::NONE, &ended) {
            ViewOutcome::SendToConversation(UiCommand::Slash {
                command:
                    argonauts_core::input::SlashCommand::Resume {
                        target: Some(argonauts_core::input::ResumeTarget::Agent(agent)),
                    },
            }) => {
                assert_eq!(agent.run, view.data.run);
                assert_eq!(agent.session, view.data.row.session);
            }
            other => panic!("{other:?}"),
        }

        let mut running = in_phase("Working.");
        let text = draw_in(&mut running, 120, &ended);
        assert!(!footer(&text).contains("c continue"), "{text}");
        assert!(matches!(
            key_in(&mut running, KeyCode::Char('c'), KeyModifiers::NONE, &ended),
            ViewOutcome::Notice(_)
        ));
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
        key(&mut view, KeyCode::Char('m'), KeyModifiers::NONE);
        for c in "use serde_json for the writer and keep the csv path as it is".chars() {
            key(&mut view, KeyCode::Char(c), KeyModifiers::NONE);
        }
        let text = draw(&mut view, 50);
        let footer = footer(&text);
        assert!(footer.contains("…"), "{footer}");
        assert!(footer.contains("as it is"), "{footer}");
    }

    #[test]
    fn only_a_long_outcome_folds() {
        let mut view = in_phase("Done.");
        let text = draw(&mut view, 80);
        assert!(text.contains("│ Outcome  "), "{text}");
        assert!(!text.contains("lines · ⏎ expand"), "{text}");

        let long = (1..=12)
            .map(|n| format!("Line {n}."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut view = in_phase(&long);
        let text = draw(&mut view, 80);
        assert!(text.contains("lines · ⏎ expand"), "{text}");
    }

    /// An agent that ended with a plan shows it as a table, with its words under it.
    #[test]
    fn a_plan_is_a_table_above_the_outcomes_words() {
        let mut planned = data("Two tasks, the interface first.");
        planned.row.status = RowStatus::Done;
        planned.plan = Some(serde_json::json!({
            "tasks": [
                {"key": "iface", "title": "Due-date stubs", "task": "Define the type.",
                 "depends_on": []},
                {"key": "impl", "title": "Store, list, overdue", "task": "Store them.",
                 "depends_on": ["iface"]}
            ],
            "assignments": [
                {"task": "iface", "worker": "claude-code", "reviewers": ["codex"]},
                {"task": "impl", "worker": "codex", "reviewers": ["claude-code"]}
            ]
        }));
        let mut view = AgentView::new(planned);
        let text = draw(&mut view, 100);
        let rows: Vec<&str> = text.lines().collect();
        let head = rows
            .iter()
            .position(|row| row.contains("● Plan · 2 tasks"))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(rows[head - 1].contains("│ Outcome"), "{text}");
        assert!(
            rows[head + 1].contains("task   what it does          worker       reviewed by"),
            "{text}"
        );
        assert!(
            rows[head + 3].contains("impl   Store, list, overdue  Codex        Claude Code"),
            "{text}"
        );
        assert!(rows[head + 4].contains("impl waits on iface"), "{text}");
        // This view takes no key to expand a task, so it offers none.
        assert!(!text.contains("expands the tasks"), "{text}");
        assert!(
            rows[head + 6].contains("Two tasks, the interface first."),
            "{text}"
        );
    }

    #[test]
    fn d_opens_the_diff() {
        let mut data = data("Done.");
        data.diff = Some(DiffStat {
            files_changed: 2,
            insertions: 10,
            deletions: 3,
        });
        let mut view = AgentView::new(data);
        let text = draw(&mut view, 80);
        assert!(text.contains("Diff · 2 files · +10 -3 · d open"), "{text}");

        // With no task to diff, `d` says so. Enter is for the sections that fold.
        let outcome = key(&mut view, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(
            matches!(&outcome, ViewOutcome::Notice(text) if text.contains("no diff")),
            "{outcome:?}"
        );
        assert!(matches!(
            key(&mut view, KeyCode::Enter, KeyModifiers::NONE),
            ViewOutcome::Consumed
        ));
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
            // `l` is Enter with vim keys on, which folds and unfolds, and no key without.
            let outcome = key_in(&mut view, KeyCode::Char('l'), plain, &state);
            assert_eq!(matches!(outcome, ViewOutcome::Consumed), vim, "{outcome:?}");
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

    /// The view's `p` is the workflow view's: it reads `p resume` on a paused run and resumes
    /// it, and takes back a pause still under way.
    #[test]
    fn the_agent_views_p_toggles_as_the_workflow_views_does() {
        let mut view = in_phase("");
        let run = view.data.run.clone();
        let mut paused = live_state();
        paused.attached = Some(run.clone());
        if let Some(held) = paused.runs.get_mut(&run) {
            held.waiting = Some(argonauts_core::events::run::WaitReason::Paused);
        }
        let text = draw_in(&mut view, 124, &paused);
        assert!(footer(&text).contains("p resume"), "{text}");
        assert!(matches!(
            key_in(&mut view, KeyCode::Char('p'), KeyModifiers::NONE, &paused),
            ViewOutcome::Send(UiCommand::Resume { run: Some(_) })
        ));
        let mut pausing = paused.clone();
        if let Some(held) = pausing.runs.get_mut(&run) {
            held.waiting = None;
            held.pausing = true;
        }
        assert!(matches!(
            key_in(&mut view, KeyCode::Char('p'), KeyModifiers::NONE, &pausing),
            ViewOutcome::Send(UiCommand::CancelPause)
        ));
        let mut going = pausing.clone();
        if let Some(held) = going.runs.get_mut(&run) {
            held.pausing = false;
        }
        let text = draw_in(&mut view, 124, &going);
        assert!(footer(&text).contains("p pause"), "{text}");
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
            label: "worker · task-002".into(),
            worker: "claude-code/opus".parse().unwrap(),
            status,
            tokens: 0,
            spent: Spend::default(),
            elapsed_ms: 0,
            subagents: 0,
            context: None,
            fallback_for: None,
            answered: false,
            tries: Vec::new(),
            note: None,
            inner: None,
            left_out: false,
            fallback_why: None,
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
            goal: None,
            plan: None,
            facts: Vec::new(),
            to_fix: None,
            asked_for: None,
            earlier: None,
            moved: None,
            given: None,
            prompt_about: None,
            review: None,
            started_from: None,
        };
        let siblings = vec![
            WorkflowRow::Agent(row("s-001", RowStatus::Done)),
            WorkflowRow::Agent(row("s-002", RowStatus::Done)),
        ];
        let view = AgentView::in_phase(data, &phase(siblings));
        assert_eq!(view.selected, 1);
        assert_eq!(view.siblings[1], fresh);
    }

    /// Under the state and who runs it, the counts as Claude Code gives them, and how full
    /// the agent's context is. The workflow's rows and header name the run.
    #[test]
    fn the_state_and_the_counts_head_the_agent() {
        let mut data = data("done");
        data.tool_calls = 23;
        data.facts = vec!["worker".to_owned(), "worktree".to_owned()];
        data.row.context = Some(argonauts_core::views::ContextUse {
            used: 62_000,
            window: Some(200_000),
            cap: Some(200_000),
        });
        let rows = vec![WorkflowRow::Agent(data.row.clone())];
        let mut view = AgentView::in_phase(data, &phase(rows));
        let text = draw(&mut view, 100);
        let lines: Vec<&str> = text.lines().collect();
        let head = lines
            .iter()
            .position(|line| line.contains("⏺ Running · Opus on Claude Code · worker · worktree"))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(
            lines[head + 1].contains("381.0k tok · 23 tool calls · 6m 2s · context 31% of 200k"),
            "{text}"
        );
        assert!(!text.contains("run-01"), "{text}");
    }

    #[test]
    fn repeated_labels_are_told_apart_by_the_agent() {
        let labelled = |session: &str, label: &str, worker: &str| AgentRow {
            label: label.into(),
            worker: worker.parse().unwrap(),
            ..row(session, RowStatus::Done)
        };
        let codex = labelled("s-002", "draft", "codex");
        let data = AgentData {
            row: codex.clone(),
            ..data("done")
        };
        let rows = vec![
            WorkflowRow::Agent(labelled("s-001", "draft", "claude-code")),
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
        assert_eq!(left("✔ draft  Claude Code"), "✔ draft  Claude Code");
        assert!(left("❯ ✔ draft").ends_with("draft  Codex"), "{text}");
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
            labelled("s-004", "worker", "codex"),
        ];
        let shown: Vec<String> = sibling_names(&rows, &DisplayNames::default())
            .into_iter()
            .map(|(label, repeated)| format!("{label} {repeated}"))
            .collect();
        assert_eq!(
            shown,
            ["judge 1 true", "judge 2 true", "judge true", "worker false"]
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
        assert!(text.contains("Prompt · 4 lines · ⏎ expand"), "{text}");
        assert!(
            text.contains("  You make this change on your own."),
            "{text}"
        );
        assert!(!text.contains("  - Work only in the worktree."), "{text}");
        assert!(text.contains("  … 3 more lines "), "{text}");
    }

    #[test]
    fn a_finished_agent_had_no_tool_calls_rather_than_none_yet() {
        let mut done = data("done");
        done.row.status = RowStatus::Done;
        let text = draw(&mut AgentView::new(done.clone()), 100);
        assert!(text.contains("  No tool calls. "), "{text}");
        let text = draw(&mut AgentView::new(data("done")), 100);
        assert!(text.contains("  No tool calls yet. "), "{text}");

        done.activity = vec![ActivityLine {
            tool: "Bash".into(),
            summary: "npm test".into(),
            ok: Some(true),
            note: None,
        }];
        done.tool_calls = 1;
        let text = draw(&mut AgentView::new(done.clone()), 100);
        assert!(text.contains("Activity · 1 tool call "), "{text}");
        // Only some show: the latest of how many.
        done.tool_calls = 9;
        let text = draw(&mut AgentView::new(done), 100);
        assert!(
            text.contains("Activity · last 1 of 9 tool calls · ⏎ expand "),
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
            parent: None,
            worker: None,
            phases: Vec::new(),
            rows: Vec::new(),
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
