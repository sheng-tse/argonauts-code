//! The goal view: what a goal-style run is trying to achieve, and whether it's
//! there yet, in one pane of the docked frame. The done-when list with each criterion's state,
//! the tries with their verdicts, the last check's reason, and the assumptions with who
//! decided each. Tab goes back to the workflow, and Esc back to the prompt.

use argonauts_core::RunId;
use argonauts_core::app::{ViewData, ViewRequest};
use argonauts_core::events::goal::{Assumption, CriterionState, Decider, GoalStatus, GoalVerdict};
use argonauts_core::views::GoalView as GoalData;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::text::{sanitize_line, str_width};
use crate::theme::{Role, Theme};

use super::dock::{self, Heading};
use super::layout::{cut, left};
use super::list::{self, Nav};
use super::panes::{self, Footer, Marks};
use super::{View, ViewCtx, ViewOutcome};

#[derive(Debug)]
pub struct GoalView {
    data: GoalData,
    /// The selected row among the criteria, the tries, and the assumptions, top down.
    selected: usize,
    /// Opened from the workflow view, so Tab goes back to it.
    from_workflow: bool,
    confirm_stop: bool,
}

/// A row the cursor can rest on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Criterion(usize),
    Try(usize),
    Assumption(usize),
}

impl GoalView {
    pub fn new(data: GoalData) -> Self {
        Self {
            data,
            selected: 0,
            from_workflow: false,
            confirm_stop: false,
        }
    }

    /// Opened with Tab from the workflow view, which Tab goes back to.
    pub fn from_workflow(mut self) -> Self {
        self.from_workflow = true;
        self
    }

    pub fn data(&self) -> &GoalData {
        &self.data
    }

    fn picks(&self) -> Vec<Pick> {
        (0..self.data.criteria.len())
            .map(Pick::Criterion)
            .chain((0..self.data.tries.len()).map(Pick::Try))
            .chain((0..self.data.assumptions.len()).map(Pick::Assumption))
            .collect()
    }

    fn pick(&self) -> Option<Pick> {
        self.picks().get(self.selected).copied()
    }

    fn met(&self) -> usize {
        self.data
            .criteria
            .iter()
            .filter(|row| row.state == Some(CriterionState::Met))
            .count()
    }

    fn pursuing(&self) -> bool {
        matches!(self.data.status, GoalStatus::Pursuing | GoalStatus::Paused)
    }

    /// `Goal · pursuing · round 2`, with `of up to 5` when you set a cap.
    fn title(&self) -> String {
        let status = match self.data.status {
            GoalStatus::Pursuing => "pursuing",
            GoalStatus::Paused => "paused",
            GoalStatus::Met => "met",
            GoalStatus::Stuck => "stuck",
            GoalStatus::OverLimit => "over its limit",
            GoalStatus::Cleared => "cleared",
        };
        match (self.data.tries.len(), self.data.max_rounds) {
            (0, _) => format!("Goal · {status}"),
            (n, None) => format!("Goal · {status} · round {n}"),
            (n, Some(max)) => format!("Goal · {status} · round {n} of up to {max}"),
        }
    }

    fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let text = theme.style(Role::Text);
        let accent = theme.style(Role::Question);
        let bold = theme.style(Role::Bright);
        let picked = self.pick();
        let cursor = |pick: Pick| {
            if picked == Some(pick) {
                Span::styled(format!("{} ", marks.selected), accent)
            } else {
                Span::raw("  ")
            }
        };
        let open = if theme.glyphs.prompt.is_ascii() {
            "[ ]"
        } else {
            "□"
        };
        let mut lines = Vec::new();

        let total = self.data.criteria.len();
        lines.push(Line::from(vec![
            Span::styled("Done when", bold),
            Span::styled(format!(" · {} of {total} met", self.met()), dim),
        ]));
        let ids = self
            .data
            .criteria
            .iter()
            .map(|row| str_width(&row.criterion.id))
            .max()
            .unwrap_or(0);
        let notes: Vec<String> = self
            .data
            .criteria
            .iter()
            .map(|row| match row.state {
                Some(CriterionState::Met) => "met".to_owned(),
                Some(_) if !row.missing.trim().is_empty() => {
                    format!("not yet · {}", sanitize_line(row.missing.trim()))
                }
                Some(_) => "not yet".to_owned(),
                None => String::new(),
            })
            .collect();
        let note_width = notes.iter().map(|note| str_width(note)).max().unwrap_or(0);
        let margin = 2 + 1 + 1 + ids + 2;
        let room = width.saturating_sub(margin);
        let note_width = note_width.min(room / 2);
        let text_width = room.saturating_sub(if note_width > 0 { note_width + 2 } else { 0 });
        for (i, row) in self.data.criteria.iter().enumerate() {
            let (glyph, role) = match row.state {
                Some(CriterionState::Met) => (marks.finished, Role::Success),
                _ => (open, Role::Dim),
            };
            let pick = Pick::Criterion(i);
            let style = if picked == Some(pick) { accent } else { text };
            let mut spans = vec![
                cursor(pick),
                Span::styled(glyph.to_owned(), theme.style(role)),
                Span::raw(" "),
                Span::styled(left(&row.criterion.id, ids), dim),
                Span::raw("  "),
                Span::styled(
                    left(
                        sanitize_line(row.criterion.text.trim()).as_ref(),
                        text_width,
                    ),
                    style,
                ),
            ];
            if note_width > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(cut(&notes[i], note_width), dim));
            }
            lines.push(Line::from(spans));
        }

        if !self.data.tries.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled("Rounds", bold),
                Span::styled(format!(" · {}", self.data.tries.len()), dim),
            ]));
            let titles = self
                .data
                .tries
                .iter()
                .map(|a_try| str_width(a_try.title.trim()))
                .max()
                .unwrap_or(0)
                .min(width / 3);
            let last = self.data.tries.len() - 1;
            for (i, a_try) in self.data.tries.iter().enumerate() {
                let (glyph, role, word) = match a_try.verdict {
                    Some(GoalVerdict::Met) => (marks.finished, Role::Success, "met"),
                    Some(GoalVerdict::NotYet) => (marks.finished, Role::Success, "not yet met"),
                    Some(GoalVerdict::Impossible) => (marks.failed, Role::Danger, "can't be met"),
                    None if i == last && self.pursuing() => (marks.running, Role::Faint, "running"),
                    None => (marks.waiting, Role::Dim, "stopped"),
                };
                let pick = Pick::Try(i);
                let style = if picked == Some(pick) { accent } else { text };
                lines.push(Line::from(vec![
                    cursor(pick),
                    Span::styled(glyph.to_owned(), theme.style(role)),
                    Span::raw(" "),
                    Span::styled(format!("{}  ", a_try.n), dim),
                    Span::styled(
                        left(sanitize_line(a_try.title.trim()).as_ref(), titles),
                        style,
                    ),
                    Span::styled(format!("  {word}"), dim),
                ]));
            }
        }

        if let Some(reason) = self
            .data
            .last_reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
        {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("Last check", bold)));
            let indent = [Span::raw("  ")];
            lines.extend(crate::text::wrap_text(
                &sanitize_line(reason),
                text,
                width,
                &indent,
                &indent,
            ));
        }

        if !self.data.assumptions.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled("Assumptions", bold),
                Span::styled(format!(" · {}", self.data.assumptions.len()), dim),
            ]));
            let texts = self
                .data
                .assumptions
                .iter()
                .map(|assumption| str_width(assumption.text.trim()))
                .max()
                .unwrap_or(0)
                .min(width.saturating_sub(4) * 2 / 3);
            for (i, assumption) in self.data.assumptions.iter().enumerate() {
                let pick = Pick::Assumption(i);
                let style = if picked == Some(pick) { accent } else { text };
                lines.push(Line::from(vec![
                    cursor(pick),
                    Span::styled(
                        left(sanitize_line(assumption.text.trim()).as_ref(), texts),
                        style,
                    ),
                    Span::styled(format!("  {}", decided_by(assumption)), dim),
                ]));
            }
        }

        lines.push(Line::default());
        let mut spent = vec![Span::styled("Spent", bold)];
        if let Some(spend) = panes::spend(&self.data.spent) {
            spent.push(Span::styled(format!(" · {spend}"), dim));
        }
        lines.push(Line::from(spent));
        lines
    }

    fn footer(&self) -> Footer {
        if self.confirm_stop {
            return Footer::hints(&[("x again to stop pursuing the goal", 0), ("esc to keep", 0)]);
        }
        let mut hints = vec![("↑↓ select", 0), ("⏎ open", 2), ("tab workflow", 3)];
        if self.pursuing() {
            hints.extend([("p pause", 1), ("x stop goal", 4)]);
        }
        hints.push((panes::ESC_BACK, 0));
        Footer::hints(&hints)
    }

    /// Back to the workflow view it was opened from, or asked for when it opened alone.
    fn back_to_workflow(&self) -> ViewOutcome {
        if self.from_workflow {
            ViewOutcome::Close
        } else {
            ViewOutcome::Request(ViewRequest::Workflow {
                run: self.data.run.clone(),
                workflow: None,
            })
        }
    }
}

/// Who decided an assumption, as the goal view says it.
fn decided_by(assumption: &Assumption) -> String {
    match assumption.by {
        Some(Decider::You) => "you".to_owned(),
        Some(Decider::MainAgent) => "the main agent".to_owned(),
        Some(Decider::Rules) => "your rules".to_owned(),
        Some(Decider::Majority) | None => sanitize_line(assumption.basis.trim()).into_owned(),
    }
}

impl View for GoalView {
    fn docked(&self) -> bool {
        true
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let run = ctx.state.runs.get(&self.data.run);
        let (name, description) = run
            .and_then(|run| run.named.as_ref())
            .map(|named| (named.name.clone(), named.description.clone()))
            .unwrap_or_else(|| (self.data.run.to_string(), self.data.objective.clone()));
        let heading = Heading {
            name,
            description,
            totals: format!("{} of {} met", self.met(), self.data.criteria.len()),
            ..Heading::default()
        };
        let frame = dock::split(area, 0, 0);
        dock::draw_rule(buf, &frame, theme);
        dock::draw_heading(buf, &frame, &heading, theme);
        let pane = frame.panes.right;
        let lines = self.lines(usize::from(pane.width), theme);
        let height = usize::from(pane.height);
        // The selected row stays in view.
        let at = lines
            .iter()
            .position(|line| {
                line.spans
                    .first()
                    .is_some_and(|span| span.content.starts_with(Marks::for_theme(theme).selected))
            })
            .unwrap_or(0);
        let skip = (at + 1).saturating_sub(height);
        let last = (skip + height).min(lines.len());
        let mark = super::workflow_rows::scroll_mark(skip, last, lines.len(), theme);
        dock::draw_box(buf, &frame, ["", &self.title()], &mark, theme);
        panes::put_lines(buf, pane, &lines[skip.min(lines.len())..]);
        dock::draw_footer(buf, &frame, self.footer(), theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if self.confirm_stop {
            self.confirm_stop = false;
            match key.code {
                KeyCode::Char('x') if plain => {
                    return ViewOutcome::Send(super::stop(&self.data.run, ctx.state));
                }
                KeyCode::Esc => return ViewOutcome::Consumed,
                _ => {}
            }
        }
        let count = self.picks().len();
        let nav = match key.code {
            KeyCode::Up => Some(Nav::By(-1)),
            KeyCode::Down => Some(Nav::By(1)),
            KeyCode::Home => Some(Nav::Top),
            KeyCode::End => Some(Nav::Bottom),
            _ => None,
        };
        if let Some(nav) = nav {
            let last = count.saturating_sub(1);
            self.selected = match nav {
                Nav::By(delta) => self.selected.saturating_add_signed(delta).min(last),
                Nav::Top => 0,
                Nav::Bottom => last,
            };
            return ViewOutcome::Consumed;
        }
        match key.code {
            KeyCode::Tab => self.back_to_workflow(),
            // A try opens in the workflow view, where it's a phase.
            KeyCode::Enter | KeyCode::Right => match self.pick() {
                Some(Pick::Try(_)) => self.back_to_workflow(),
                Some(Pick::Assumption(_)) => ViewOutcome::Notice(
                    "Tell the main agent in the conversation how to change it; the next round follows it"
                        .to_owned(),
                ),
                _ => ViewOutcome::Consumed,
            },
            KeyCode::Char('p') if plain && self.pursuing() => super::pause(&self.data.run, ctx),
            KeyCode::Char('x') if plain && self.pursuing() => {
                self.confirm_stop = true;
                ViewOutcome::Consumed
            }
            KeyCode::Esc | KeyCode::Left => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        match data {
            ViewData::Goal { view } if view.run == self.data.run => {
                self.data = (**view).clone();
                self.selected = self.selected.min(self.picks().len().saturating_sub(1));
                true
            }
            _ => false,
        }
    }
}

/// What `/goal` and Tab ask for.
pub fn request(run: &RunId) -> ViewRequest {
    ViewRequest::Goal { run: run.clone() }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::Spend;
    use argonauts_core::app::UiCommand;
    use argonauts_core::events::goal::{Criterion, Try};
    use argonauts_core::input::SlashCommand;
    use argonauts_core::views::CriterionRow;

    use super::*;
    use crate::stability::{assert_frames, buffer_text};
    use crate::state::AppState;
    use crate::theme::Glyphs;

    fn data() -> GoalData {
        let criterion = |id: &str, text: &str, state, missing: &str| CriterionRow {
            criterion: Criterion::new(id, text),
            state,
            evidence: String::new(),
            missing: missing.to_owned(),
        };
        GoalData {
            run: RunId::new("run-01").unwrap(),
            objective: "Fix slugify and median in textkit, with tests".to_owned(),
            status: GoalStatus::Pursuing,
            criteria: vec![
                criterion(
                    "G1",
                    "slugify(\"Hello, World!\") returns \"hello-world\"",
                    Some(CriterionState::Met),
                    "",
                ),
                criterion(
                    "G4",
                    "median([]) raises ValueError(\"median of empty list\")",
                    Some(CriterionState::NotYet),
                    "returns None",
                ),
            ],
            tries: vec![
                Try {
                    n: 1,
                    title: "Both fixes".to_owned(),
                    phase: 1,
                    verdict: Some(GoalVerdict::NotYet),
                },
                Try {
                    n: 2,
                    title: "Empty list".to_owned(),
                    phase: 2,
                    verdict: None,
                },
            ],
            max_rounds: None,
            last_reason: Some(
                "Not yet. median([]) returns None at textkit/stats.py:41, and no test covers it."
                    .to_owned(),
            ),
            assumptions: vec![Assumption {
                text: "accented letters stay out of this fix".to_owned(),
                basis: "2 of 3 readings".to_owned(),
                by: Some(Decider::You),
            }],
            spent: Spend {
                cost_usd: 0.28,
                ..Spend::default()
            },
        }
    }

    fn draw(view: &mut GoalView, width: u16) -> String {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let area = Rect::new(0, 0, width, 24);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        assert_frames(&buf);
        buffer_text(&buf)
    }

    #[test]
    fn the_goal_view_says_what_its_after_and_how_far_it_got() {
        for width in [80, 100, 120, 200] {
            let text = draw(&mut GoalView::new(data()), width);
            assert!(text.contains("Goal · pursuing · round 2"), "{text}");
            assert!(text.contains("Done when · 1 of 2 met"), "{text}");
            assert!(text.contains("❯ ✔ G1"), "{text}");
            assert!(text.contains("□ G4"), "{text}");
            assert!(text.contains("Rounds · 2"), "{text}");
            assert!(text.contains("● 2  Empty list"), "{text}");
            assert!(text.contains("Last check"), "{text}");
            assert!(text.contains("you"), "{text}");
        }
        let text = draw(&mut GoalView::new(data()), 120);
        assert!(text.contains("not yet · returns None"), "{text}");
        assert!(
            text.contains("↑↓ select · ⏎ open · tab workflow · p pause · x stop goal · esc back"),
            "{text}"
        );
    }

    #[test]
    fn tab_goes_back_to_the_workflow() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let mut view = GoalView::new(data()).from_workflow();
        assert!(matches!(view.handle_key(tab, &ctx), ViewOutcome::Close));
        let mut alone = GoalView::new(data());
        assert!(matches!(
            alone.handle_key(tab, &ctx),
            ViewOutcome::Request(ViewRequest::Workflow { .. })
        ));
    }

    /// `x x` stops the goal's run whether it is on screen or runs beside the conversation.
    #[test]
    fn stopping_the_goal_stops_its_run_on_screen_or_not() {
        let theme = Theme::dark();
        let x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        let beside = AppState::default();
        let mut shown = AppState::default();
        shown.attached = Some(data().run);
        for (state, wanted) in [
            (
                beside,
                UiCommand::Slash {
                    command: SlashCommand::Stop {
                        run: Some(data().run),
                    },
                },
            ),
            (shown, UiCommand::Stop),
        ] {
            let ctx = ViewCtx {
                theme: &theme,
                state: &state,
                now: Instant::now(),
            };
            let mut view = GoalView::new(data());
            assert!(matches!(view.handle_key(x, &ctx), ViewOutcome::Consumed));
            match view.handle_key(x, &ctx) {
                ViewOutcome::Send(command) => assert_eq!(command, wanted),
                other => panic!("x x sent nothing: {other:?}"),
            }
        }
    }
}
