//! Everything under the transcript that redraws in place. The card sits right above the
//! queued text and the input, where your keys go once it has them.

mod agents;
mod answered;
mod bar;
mod card;
mod composer;
mod menu;
mod mode_line;
mod narrator;
pub mod picker;
pub mod verbs;
mod workflows;
mod working;

use std::time::Instant;

use crossterm::event::KeyEvent;
use ratatui::text::{Line, Span};

use crate::render::RenderCtx;
use crate::text::{sanitize_line, truncate_line};
use crate::theme::Role;

pub use agents::{
    AgentLabel, AgentPanel, AgentRef, AgentRow, AgentState, CIRCLE_PERIOD, PanelEvent,
};
pub use answered::{AnsweredPick, REMEMBER_KEY};
pub use card::{
    CardAgent, CardChip, CardEvent, CardNote, CardOption, CardPrompt, CardView, Checklist,
    NOTE_PLACEHOLDER, OptionKind,
};
pub use composer::{
    BUILT_IN_EXAMPLE, Composer, ComposerEvent, ComposerState, ComposerView, MAX_INPUT_ROWS,
};
pub use menu::{MenuEvent, MenuItem, MenuKind, Menus};
pub use mode_line::{ApprovalMode, ModeLine, ModeNotice, ModeState, VimMode, status_line};
pub use narrator::{NARRATOR_MAX_ROWS, narrator_lines};
pub use picker::{Picker, PickerEvent, PickerRow};
pub use workflows::{
    PanelRowId, StopAsk, WorkflowPanel, WorkflowRow, WorkflowRowState, ask_before_stop,
    handle_panel_key, panel_has_rows, panel_lines,
};
pub use working::{Activity, SPINNER_PERIOD, STALL_AFTER, WorkingLine};

use crate::cell::Tone;

#[derive(Debug, Clone, Default)]
pub struct BottomState {
    pub working: Option<WorkingLine>,
    /// Messages sent while work runs, shown dim until they start.
    pub queued: Vec<String>,
    /// Where each queued message goes when it's for one agent, drawn after it, such as
    /// `to agy, at its next turn`. Kept apart so taking the texts back gets them as typed.
    pub queued_where: Vec<Option<String>>,
    pub card: Option<CardPrompt>,
    /// Where the card was, while no card shows: the last handoff pick, offered as a rule.
    pub answered_pick: Option<AnsweredPick>,
    pub composer: Composer,
    /// Open under the input in place of the mode line and everything below it.
    pub menus: Menus,
    pub mode: ModeLine,
    /// Vendor quotas, spend, and context, at the mode line's right end when they fit.
    pub status_line: Vec<StatusPart>,
    pub agents: AgentPanel,
    /// One row per run, under the mode line, with the agents above when they show.
    pub workflows: WorkflowPanel,
    /// The narrator's latest line, shown just above the working line.
    pub narrator: Option<String>,
    /// A settings picker open under the input, in place of the bottom rule and what follows.
    pub picker: Option<Picker>,
    pub hint: Option<Hint>,
    /// A first `x` on a panel row, shown on the mode line until the next key.
    pub stop_asked: Option<StopAsk>,
    /// Added to the exit hint, such as `the run keeps going`.
    pub exit_note: Option<String>,
    /// Where Enter sends a draft now, shown while you type during a run or while a card
    /// waits, so it's clear the text doesn't answer the card.
    pub enter_sends: Option<String>,
    /// A run takes what you type, also while it's parked at a card, so a waiting card's
    /// placeholder offers guiding it.
    pub run_takes_text: bool,
    /// `ui.live_area_max`: the most of the screen's height the live region may take.
    pub live_area_max: Option<f64>,
    pub synced: Synced,
}

/// What the status sync last applied, so running it again changes nothing, and a change made
/// on screen (Shift+Tab) stays until the controller says otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Synced {
    /// The work the working line shows, so its clock keeps running from one sync to the next.
    pub work: Option<String>,
    /// That work's event count when the line last moved, for the stall fade.
    pub events: u64,
    /// The approval mode last taken from the controller.
    pub approval: Option<ApprovalMode>,
}

pub const CARD_PLACEHOLDER_RUN: &str = "↑ to answer the card, or type to guide the run";

pub const CARD_PLACEHOLDER: &str = "↑ to answer the card";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPart {
    pub text: String,
    pub tone: Tone,
    /// Parts give way on a narrow terminal lowest rank first, and never at
    /// [`KEEP`](crate::text::KEEP).
    pub rank: u16,
}

impl StatusPart {
    /// A part that always shows, such as `Codex down` or `agy out`.
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
            rank: crate::text::KEEP,
        }
    }

    /// Chips under 90% used give way before the spend, the least used first. A chip at 90%
    /// or more stays.
    pub fn quota(text: impl Into<String>, used_percent: u8, tone: Tone) -> Self {
        let rank = if used_percent >= 90 {
            crate::text::KEEP
        } else {
            status_rank::KNOWN + u16::from(used_percent)
        };
        Self {
            rank,
            ..Self::new(text, tone)
        }
    }

    /// A reading older than its refresh, which gives way before every fresh one.
    pub fn stale(text: impl Into<String>, used_percent: u8) -> Self {
        Self {
            rank: status_rank::STALE + u16::from(used_percent.min(99)),
            ..Self::new(text, Tone::Dim)
        }
    }

    /// An agent without a quota, such as `OpenRouter $0.06`, gives way with the quota chips.
    pub fn no_quota(text: impl Into<String>) -> Self {
        Self {
            rank: status_rank::KNOWN,
            ..Self::new(text, Tone::Dim)
        }
    }

    /// The busiest session's context against its cap, which goes just before the spend.
    pub fn context(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            rank: status_rank::CONTEXT,
            ..Self::new(text, tone)
        }
    }

    /// The narrator's spend against its cap.
    pub fn narrator(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            rank: status_rank::NARRATOR,
            ..Self::new(text, tone)
        }
    }

    pub fn spend(text: impl Into<String>) -> Self {
        Self {
            rank: status_rank::SPEND,
            ..Self::new(text, Tone::Dim)
        }
    }

    pub fn model(text: impl Into<String>) -> Self {
        Self {
            rank: status_rank::MODEL,
            ..Self::new(text, Tone::Dim)
        }
    }
}

/// When status line parts give way, lowest first: stale readings, fresh quota chips by how
/// little is used, the context and the narrator, the spend, and the model. Pinned chips
/// never go.
mod status_rank {
    pub const STALE: u16 = 1;
    pub const KNOWN: u16 = 200;
    pub const CONTEXT: u16 = 398;
    pub const NARRATOR: u16 = 399;
    pub const SPEND: u16 = 400;
    pub const MODEL: u16 = 500;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub text: String,
    pub until: Instant,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BottomView {
    pub lines: Vec<Line<'static>>,
    /// (column, row) within `lines`.
    pub cursor: Option<(u16, u16)>,
}

impl BottomState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A card that waits without focus leaves the keys with the composer.
    pub fn composer_has_focus(&self) -> bool {
        self.focused_card().is_none() && !self.agents.has_focus() && !self.pick_selected()
    }

    /// ↑ picked the answered handoff pick's line, and no card covers it.
    pub fn pick_selected(&self) -> bool {
        self.card.is_none()
            && self
                .answered_pick
                .as_ref()
                .is_some_and(|pick| pick.selected)
    }

    pub fn expanded_card(&self) -> Option<&CardPrompt> {
        self.card.as_ref().filter(|card| !card.pinned)
    }

    pub fn focused_card(&self) -> Option<&CardPrompt> {
        self.card
            .as_ref()
            .filter(|card| card.focused && !card.pinned)
    }

    pub fn waiting_card(&self) -> Option<&CardPrompt> {
        self.card.as_ref().filter(|card| card.waits())
    }

    pub fn active_hint(&self, now: Instant) -> Option<&str> {
        let timed = self
            .hint
            .as_ref()
            .filter(|hint| hint.until > now)
            .map(|hint| hint.text.as_str());
        timed.or_else(|| {
            self.stop_asked
                .as_ref()
                .filter(|asked| self.panel_selection().as_ref() == Some(&asked.row))
                .map(|asked| asked.hint.as_str())
        })
    }

    /// Keys for the focused panel, with a stop asked for before it goes.
    pub fn panel_key(&mut self, key: KeyEvent) -> PanelEvent {
        let event = handle_panel_key(&mut self.workflows, &mut self.agents, key);
        ask_before_stop(&mut self.stop_asked, event)
    }

    /// Focus moves into the panel, on its first row, with no stop asked yet.
    pub fn focus_panel(&mut self) {
        self.stop_asked = None;
        // The panel has a row per run, with its agents under it only with `ui.panel.agents`,
        // so focus starts on the first run when there is one.
        if self.workflows.rows.is_empty() {
            self.agents.focus();
        } else {
            self.workflows.focus();
        }
    }

    /// The panel row under the pointer while the panel has focus.
    pub fn panel_selection(&self) -> Option<PanelRowId> {
        if self.workflows.has_focus() {
            return self.workflows.selected_id(&self.agents);
        }
        let rows = &self.agents.rows;
        let selected = self.agents.selected.filter(|_| !rows.is_empty())?;
        Some(PanelRowId::Agent(
            rows[selected.min(rows.len() - 1)].agent_ref(),
        ))
    }

    pub fn render(&self, ctx: &RenderCtx<'_>) -> BottomView {
        self.render_within(ctx, usize::MAX)
    }

    /// Parts give way in this order: the agent panel, queued messages, the working line, then
    /// the card's details. If what's left still doesn't fit, the top rows
    /// are cut.
    pub fn render_within(&self, ctx: &RenderCtx<'_>, max_rows: usize) -> BottomView {
        self.render_live(ctx, max_rows, false)
    }

    /// A chat turn's working line steps aside while its answer text streams above.
    pub fn render_live(
        &self,
        ctx: &RenderCtx<'_>,
        max_rows: usize,
        text_streaming: bool,
    ) -> BottomView {
        let mut parts = self.parts(ctx, text_streaming);
        parts.fit(max_rows);
        parts.assemble(max_rows)
    }

    /// When the region next changes on its own. `None` when nothing animates, so an idle
    /// screen never redraws.
    pub fn next_change(&self, ctx: &RenderCtx<'_>) -> Option<Instant> {
        let clock = &ctx.clock;
        let hint = self
            .hint
            .as_ref()
            .map(|hint| hint.until)
            .filter(|until| *until > clock.now);
        let notice = self
            .mode
            .notice
            .as_ref()
            .map(|notice| notice.until)
            .filter(|until| *until > clock.now);
        [
            self.working
                .as_ref()
                .and_then(|working| working.next_change(clock, ctx.theme)),
            self.agents.next_change(clock, ctx.theme),
            hint,
            notice,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn parts(&self, ctx: &RenderCtx<'_>, text_streaming: bool) -> Parts {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);

        // The narrator's line sits just above the working line.
        let mut working = narrator_lines(self.narrator.as_deref(), ctx);
        match &self.working {
            Some(line) if line.shows(text_streaming) => {
                working.extend([line.line(ctx), Line::default()]);
            }
            _ if !working.is_empty() => working.push(Line::default()),
            _ => {}
        }
        // A queued message looks like a sent one, dimmed until it's delivered.
        let band = theme.style(Role::UserBand);
        let queued = self
            .queued
            .iter()
            .enumerate()
            .map(|(i, message)| {
                let first = message
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("");
                let mut spans = vec![
                    Span::styled(theme.glyphs.prompt, theme.style(Role::Faint)),
                    Span::raw(" "),
                    Span::styled(sanitize_line(first).into_owned(), dim),
                ];
                if let Some(Some(place)) = self.queued_where.get(i) {
                    spans.push(Span::styled(format!(" · {}", sanitize_line(place)), dim));
                }
                truncate_line(&Line::from(spans), width).style(band)
            })
            .collect();
        let card_ctx = ctx.with_vim(self.mode.vim.is_some());
        let mut card = self
            .card
            .as_ref()
            .map(|card| card.layout(&card_ctx))
            .unwrap_or_default();
        if self.focused_card().is_none() {
            card.cursor = None;
        }
        if let Some(pick) = self.answered_pick.as_ref().filter(|_| self.card.is_none()) {
            card.lines = vec![pick.line(ctx)];
        }

        let focused = self.composer_has_focus();
        let busy = self.working.is_some();
        let waiting = self.waiting_card().is_some();
        let placeholder = waiting.then_some(if busy || self.run_takes_text {
            CARD_PLACEHOLDER_RUN
        } else {
            CARD_PLACEHOLDER
        });
        // An example prompt is a way to start, so it waits while work runs or a card is up.
        let offer_example = !busy && self.card.is_none();
        let mut composer = self.composer.render_with(
            ctx,
            ComposerState {
                focused,
                busy,
                offer_example,
                placeholder,
            },
        );
        // An open picker's edge rule takes the bottom rule's place, and its rows take the menu's.
        let menu = if let Some(picker) = &self.picker {
            composer.lines.pop();
            picker.lines(ctx)
        } else if focused {
            self.menus
                .rows(self.composer.text(), ctx, usize::MAX)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let pinned_cards = self
            .card
            .as_ref()
            .filter(|card| card.pinned)
            .map_or(0, |_| 1);
        let typing = !self.composer.is_empty();
        let mode = self.mode.line(
            ctx,
            ModeState {
                hint: self.active_hint(ctx.clock.now),
                working: busy,
                typing,
                pinned_cards,
                panel: panel_has_rows(&self.workflows, &self.agents),
                panel_focused: self.agents.has_focus() || self.workflows.selected.is_some(),
                card_focused: self.focused_card().is_some(),
                enter_sends: self
                    .enter_sends
                    .as_deref()
                    .filter(|_| typing && focused && (busy || waiting)),
                status: &self.status_line,
            },
        );
        let card_rows = card.lines.len();
        Parts {
            working,
            queued,
            card_gap: card_rows > 0,
            card_tail: card.tail,
            card_extra: card.extra,
            // A pinned card is one row.
            card_head_rows: card_rows.min(2),
            card_cursor: card.cursor,
            card: card.lines,
            composer_cursor: focused.then_some(composer.cursor),
            composer: composer.lines,
            menu,
            mode,
            agents: panel_lines(&self.workflows, &self.agents, ctx),
        }
    }
}

struct Parts {
    working: Vec<Line<'static>>,
    queued: Vec<Line<'static>>,
    card: Vec<Line<'static>>,
    card_gap: bool,
    /// Rows at the card's bottom (blank row, key hints, footer) that go when rows are short.
    card_tail: usize,
    /// The card's chips, agent, details, and body, which go next.
    card_extra: std::ops::Range<usize>,
    /// Rows at the card's top that always stay: its rule and the title's first row.
    card_head_rows: usize,
    /// (column, row) within `card`.
    card_cursor: Option<(u16, u16)>,
    composer: Vec<Line<'static>>,
    /// (column, row) within `composer`.
    composer_cursor: Option<(u16, u16)>,
    menu: Vec<Line<'static>>,
    mode: Line<'static>,
    agents: Vec<Line<'static>>,
}

impl Parts {
    fn rows(&self) -> usize {
        if !self.menu.is_empty() {
            return self.working.len()
                + self.card.len()
                + usize::from(self.card_gap)
                + self.queued.len()
                + self.composer.len()
                + self.menu.len();
        }
        let agents = match self.agents.len() {
            0 => 0,
            rows => rows + 1,
        };
        self.working.len()
            + self.card.len()
            + usize::from(self.card_gap)
            + self.queued.len()
            + self.composer.len()
            + 1
            + agents
    }

    fn fit(&mut self, max_rows: usize) {
        let over = self.rows().saturating_sub(max_rows);
        if over > 0 && self.menu.len() > MENU_MIN_ROWS {
            let keep = self.menu.len().saturating_sub(over).max(MENU_MIN_ROWS);
            self.menu.truncate(keep);
        }
        if self.rows() > max_rows {
            self.agents.clear();
        }
        if self.rows() > max_rows {
            self.queued.clear();
        }
        if self.rows() > max_rows {
            self.working.clear();
        }
        if self.rows() > max_rows {
            self.card_gap = false;
        }
        if self.rows() > max_rows && self.card_tail > 0 {
            let keep = self.card.len().saturating_sub(self.card_tail);
            self.card.truncate(keep);
        }
        let over = self.rows().saturating_sub(max_rows);
        // The card's extras go from the top, never into its head rows.
        let extra = self.card_extra.clone();
        let start = extra.start.max(self.card_head_rows);
        let cut = over.min(extra.end.saturating_sub(start));
        if cut > 0 {
            self.card.drain(start..start + cut);
            self.card_cursor = self.card_cursor.and_then(|(column, row)| {
                let row = usize::from(row);
                match row {
                    _ if row < start => Some((column, row as u16)),
                    _ if row < start + cut => None,
                    _ => Some((column, (row - cut) as u16)),
                }
            });
        }
    }

    fn assemble(self, max_rows: usize) -> BottomView {
        let mut lines = self.working;
        let card_top = lines.len();
        lines.extend(self.card);
        if self.card_gap {
            lines.push(Line::default());
        }
        lines.extend(self.queued);
        let composer_top = lines.len();
        lines.extend(self.composer);
        if !self.menu.is_empty() {
            lines.extend(self.menu);
        } else {
            lines.push(self.mode);
            if !self.agents.is_empty() {
                lines.push(Line::default());
                lines.extend(self.agents);
            }
        }

        let cursor = match (self.card_cursor, self.composer_cursor) {
            (Some((column, row)), _) => Some((column, row + card_top as u16)),
            (None, Some((column, row))) => Some((column, row + composer_top as u16)),
            (None, None) => None,
        };
        // Last resort: keep the bottom rows, where typing happens.
        let cut = lines.len().saturating_sub(max_rows);
        lines.drain(..cut);
        let cursor =
            cursor.and_then(|(column, row)| row.checked_sub(cut as u16).map(|row| (column, row)));
        BottomView { lines, cursor }
    }
}

/// Menu entries kept when the screen is short.
const MENU_MIN_ROWS: usize = 3;

#[cfg(test)]
mod tests;
