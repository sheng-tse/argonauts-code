//! Everything under the transcript that redraws in place. The question sits right above the
//! queued text and the input, where your keys go once it has them.

mod agents;
mod answered;
mod band;
mod bar;
mod bottom_row;
mod composer;
mod files;
mod history_search;
pub mod line;
mod menu;
mod narrator;
mod notice;
pub mod picker;
mod question;
mod retry;
mod tips;
pub mod verbs;
mod workflows;
mod working;

use std::time::{Duration, Instant};

use argonauts_core::settings::ui::WhenQuestion;
use crossterm::event::KeyEvent;
use ratatui::text::{Line, Span};

use crate::render::RenderCtx;
use crate::text::{sanitize_line, truncate_line};
use crate::theme::Role;

pub use agents::{
    AgentLabel, AgentPanel, AgentRef, AgentRow, AgentState, CIRCLE_PERIOD, LIST_KEYS, MAIN,
    PanelEvent, RowKind,
};
pub use answered::{AnsweredPick, REMEMBER_KEY};
pub use band::{BAND_KEYS, Band, BandEvent, PinnedRun, wall_now};
pub use bottom_row::{ApprovalMode, BottomRow, ModeNotice, ModeState, PASTING, VimMode};
pub use composer::{
    BUILT_IN_EXAMPLE, Composer, ComposerEvent, ComposerState, ComposerView, DraftAside,
    MAX_INPUT_ROWS, SHELL_PLACEHOLDER, SHELL_PROMPT, ignored_notice, is_on_disk, requested_notice,
};
pub use history_search::{HistorySearch, SearchEvent, SearchScope};
pub use line::{
    AddedPart, BottomLine, CliState, CliUse, LinePart, NEAR_LIMIT, PartBody, WindowUse,
};
pub use menu::{MenuEvent, MenuItem, MenuKind, Menus};
pub use narrator::{NARRATOR_MAX_ROWS, narrator_lines};
pub use notice::{NOTICE_KEYS, Notice, NoticeEvent};
pub use picker::{Picker, PickerEvent, PickerRow};
pub use question::{
    Checklist, Countdown, ESC_HINT, NOTE_PLACEHOLDER, OptionKind, OptionModel, QUESTION_GUARD,
    QuestionAgent, QuestionChip, QuestionEvent, QuestionNote, QuestionOption, QuestionPrompt,
    QuestionView,
};
pub use retry::RetryNote;
pub use workflows::{
    PanelRowId, StopAsk, WorkflowPanel, WorkflowRow, WorkflowRowState, ask_before_stop,
    handle_panel_key, panel_has_rows, panel_lines,
};
pub use working::{Activity, SPINNER_PERIOD, WorkingLine};

#[derive(Debug, Clone, Default)]
pub struct BottomState {
    pub working: Option<WorkingLine>,
    /// What the work waits for while nothing else of it goes on, such as `Waiting for 1
    /// workflow to finish`, drawn dim and still in the working line's place, as Claude Code
    /// ends a turn whose workflows go on.
    pub waits_for: Option<String>,
    /// Messages sent while work runs, shown dim until they start.
    pub queued: Vec<String>,
    /// Where each queued message goes when it's for one agent, drawn after it, such as
    /// `to agy, at its next turn`. Kept apart so taking the texts back gets them as typed.
    pub queued_where: Vec<Option<String>>,
    /// Under the queued messages where the session takes them now, such as `ctrl+enter to
    /// send now`.
    pub send_now_hint: Option<String>,
    /// The running turn's session takes send now, so the key works.
    pub send_now: bool,
    /// Claude Code's You should know, right above the input.
    pub notice: Notice,
    pub question: Option<QuestionPrompt>,
    /// Where the question was, while no question shows: the last fallback pick, offered as a rule.
    pub answered_pick: Option<AnsweredPick>,
    pub composer: Composer,
    /// Open under the input in place of the bottom line and everything below it.
    pub menus: Menus,
    pub mode: BottomRow,
    /// The bottom line's parts beside the mode: each CLI's plan usage, and what you added.
    pub line: BottomLine,
    /// Parts other code draws on the bottom line, by the name `left` and `right` know them by,
    /// such as `awake`, `stats`, `checks`, or a mod's name. The line shows one only while it's
    /// named in the settings, except `awake`: a hold you asked for shows while it lasts.
    pub line_slots: std::collections::BTreeMap<String, PartBody>,
    /// Your mods from `mods/`, as the controller last read them. Those placed above the input
    /// sit under the band's own cards.
    pub file_mods: Option<argonauts_core::settings::mods::ModsView>,
    pub agents: AgentPanel,
    /// One row per run, under the bottom line, with the agents above when they show.
    pub workflows: WorkflowPanel,
    /// The narrator's latest line, shown just above the working line.
    pub narrator: Option<String>,
    /// The first start after an update says so above the input until your first key.
    pub updated: Option<String>,
    /// A settings picker open under the input, in place of the bottom rule and what follows.
    pub picker: Option<Picker>,
    /// Prompt history search, open in place of the input and everything below it.
    pub history_search: Option<HistorySearch>,
    pub hint: Option<Hint>,
    /// The key that ignores the word `ultracode` in a draft, as the notice names it.
    pub workflow_key: String,
    /// Said for a moment after that key ignored the word, in the notice's place.
    pub word_notice: Option<Hint>,
    /// When the draft began to ask for a workflow, which is what the notice's time counts from.
    pub word_asked_at: Option<Instant>,
    /// A first `x` on a panel row, shown on the bottom line until the next key.
    pub stop_asked: Option<StopAsk>,
    /// Added to the exit hint, such as `the run keeps going`.
    pub exit_note: Option<String>,
    /// Where Enter sends a draft now, shown while you type during a run or while a question
    /// waits, so it's clear the text doesn't answer the question.
    pub enter_sends: Option<String>,
    /// A run takes what you type, also while it's waiting at a question, so a waiting question's
    /// placeholder offers guiding it.
    pub run_takes_text: bool,
    /// A `!` command runs, so Ctrl+C stops it rather than clearing or quitting.
    pub shell_running: bool,
    /// `ui.live_area_max`: the most of the screen's height the live region may take.
    pub live_area_max: Option<f64>,
    pub synced: Synced,
    /// The draft that was in the input when a question took the keys, back once the question is
    /// answered or passed on.
    pub draft_aside: Option<DraftAside>,
    /// The mods pinned above the input.
    pub band: Band,
}

/// What the status sync last applied, so running it again changes nothing, and a change made
/// on screen (Shift+Tab) stays until the controller says otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Synced {
    /// The work the working line shows, so its clock keeps running from one sync to the next.
    pub work: Option<String>,
    /// The approval mode last taken from the controller.
    pub approval: Option<ApprovalMode>,
}

pub const QUESTION_PLACEHOLDER_RUN: &str = "↑  to answer the question, or type to guide the run";

pub const QUESTION_PLACEHOLDER: &str = "↑  to answer the question";

/// How long the screen says the draft asks for a workflow, as Claude Code does. The word
/// keeps its color after.
pub const WORD_REQUESTED_NOTICE: Duration = Duration::from_secs(30);

/// How long the screen says the workflow word was ignored, as Claude Code does.
pub const WORD_IGNORED_NOTICE: Duration = Duration::from_secs(5);

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

    /// The agent's work goes on: its working line shows, or the line that says what the work
    /// waits for, so the input and its hint line draw as they do while it works.
    fn at_work(&self) -> bool {
        self.working.is_some() || self.waits_for.is_some()
    }

    /// An open picker takes the input's place, as a dialog does in Claude Code 2.1.303: the
    /// input's rules, its row and the cursor go, and so do the band and the notice over it,
    /// until it closes. A question that holds the keys, or the prompt search, stands in for
    /// the picker instead.
    pub fn picker_in_place(&self) -> bool {
        self.picker.is_some() && self.history_search.is_none() && self.focused_question().is_none()
    }

    /// A question that waits without focus leaves the keys with the composer.
    pub fn composer_has_focus(&self) -> bool {
        self.focused_question().is_none()
            && !self.agents.has_focus()
            && !self.pick_selected()
            && !self.notice.has_focus()
            && !self.band.has_focus()
    }

    /// The question on screen takes the keys the moment it shows if it asks, from wherever they
    /// were: the input, a panel, the notice, a picker, or the prompt search.
    /// The panels let go, a draft goes aside, and a picker or search waits under the question to
    /// come back with the input. Returns whether it took them.
    pub fn hand_keys_to_question(&mut self, now: Instant) -> bool {
        let took = self
            .question
            .as_mut()
            .is_some_and(|question| question.take_keys(now));
        if took {
            self.agents.blur();
            self.workflows.blur();
            self.notice.blur();
            self.band.blur();
            self.stop_asked = None;
            self.settle_draft();
        }
        took
    }

    /// A draft goes aside while a question holds the keys and comes back once it doesn't, after
    /// the question is answered or passed on. Returns whether the input changed.
    pub fn settle_draft(&mut self) -> bool {
        let question_holds_keys = self.focused_question().is_some();
        match self.draft_aside.take() {
            Some(aside) if question_holds_keys => {
                self.draft_aside = Some(aside);
                false
            }
            Some(aside) => {
                self.composer.bring_back(aside);
                self.menus.text_changed();
                true
            }
            None if question_holds_keys => {
                self.draft_aside = self.composer.set_aside();
                self.draft_aside.is_some()
            }
            None => false,
        }
    }

    /// ↑ picked the answered fallback pick's line, and no question covers it.
    pub fn pick_selected(&self) -> bool {
        self.question.is_none()
            && self
                .answered_pick
                .as_ref()
                .is_some_and(|pick| pick.selected)
    }

    pub fn expanded_question(&self) -> Option<&QuestionPrompt> {
        self.question
            .as_ref()
            .filter(|question| !question.dismissed)
    }

    pub fn focused_question(&self) -> Option<&QuestionPrompt> {
        self.question
            .as_ref()
            .filter(|question| question.focused && !question.dismissed)
    }

    pub fn waiting_question(&self) -> Option<&QuestionPrompt> {
        self.expanded_question().filter(|question| question.waits())
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

    /// Notes when the draft began to ask for a workflow, after whatever changed the draft, and
    /// forgets it once the draft no longer asks.
    pub fn settle_workflow_word(&mut self, now: Instant) {
        let requested = self.composer.workflow_requested();
        self.word_asked_at = if requested {
            self.word_asked_at.or(Some(now))
        } else {
            None
        };
        // The word left the draft, or the ignore was undone: "ignored" no longer holds.
        if requested || !self.composer.holds_workflow_word() {
            self.word_notice = None;
        }
    }

    /// When the notice that the draft asks for a workflow goes. A draft nothing has timed yet
    /// keeps it.
    fn word_notice_ends(&self) -> Option<Instant> {
        self.word_asked_at.map(|at| at + WORD_REQUESTED_NOTICE)
    }

    /// Keys for the focused panel, with a stop asked for before it goes.
    pub fn panel_key(&mut self, key: KeyEvent) -> PanelEvent {
        if let Some(run) = self.pause_asked(&key) {
            self.stop_asked = None;
            return PanelEvent::PauseRun(run);
        }
        let event = handle_panel_key(&mut self.workflows, &mut self.agents, key);
        ask_before_stop(&mut self.stop_asked, event)
    }

    /// `p` on the run's row a stop was just asked on: the run to pause.
    fn pause_asked(&self, key: &KeyEvent) -> Option<String> {
        if key.code != crossterm::event::KeyCode::Char('p') || !key.modifiers.is_empty() {
            return None;
        }
        let asked = &self.stop_asked.as_ref()?.row;
        match asked {
            PanelRowId::Run(run) if self.panel_selection().as_ref() == Some(asked) => {
                Some(run.clone())
            }
            _ => None,
        }
    }

    /// Focus moves into the panel, on its first row past your own conversation's, with no
    /// stop asked yet.
    pub fn focus_panel(&mut self) {
        self.stop_asked = None;
        // The panel has a row per run, with its agents under it only with `ui.panel.agents`,
        // so focus starts on the first run when there is one.
        if self.workflows.rows.is_empty() {
            self.agents.focus();
        } else {
            self.workflows.focus_past_main(&self.agents);
        }
    }

    /// The agent your typing goes to: the one whose row is selected in the list. `None` on
    /// your own conversation's row, on a run's, and without a selection, where typing goes to
    /// the main agent as always.
    pub fn messaging(&self) -> Option<&AgentRow> {
        let PanelRowId::Agent(selected) = self.panel_selection()? else {
            return None;
        };
        self.agents
            .rows
            .iter()
            .find(|row| row.agent_ref() == selected)
            .filter(|row| row.kind != RowKind::Main)
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

    /// Parts give way in this order: the agent panel, queued messages, the notice, the working
    /// line, then the question's details. If what's left still doesn't fit, the top rows are cut.
    pub fn render_within(&self, ctx: &RenderCtx<'_>, max_rows: usize) -> BottomView {
        self.render_live(ctx, max_rows, false)
    }

    /// A conversation turn's working line steps aside while its answer text streams above.
    pub fn render_live(
        &self,
        ctx: &RenderCtx<'_>,
        max_rows: usize,
        text_streaming: bool,
    ) -> BottomView {
        let mut parts = self.parts(ctx, text_streaming, max_rows);
        // An open picker hides the band with the input, as Claude Code hides its panes with
        // the prompt.
        if !self.picker_in_place() {
            // The band takes only rows nothing else wants: the input and a question never
            // lose one to it.
            let room = max_rows.saturating_sub(parts.rows());
            let question_up = self.focused_question().is_some();
            parts.band = self.band.lines(ctx, room, question_up, self.at_work());
            // Your mods share the band's rows, under its cap.
            let left = room
                .min(self.band.max_rows)
                .saturating_sub(parts.band.len());
            parts
                .band
                .extend(self.file_mods_above(ctx, left, question_up));
        }
        parts.fit(max_rows);
        parts.assemble(max_rows)
    }

    /// Your mods placed above the input, in the `room` the band's own cards left, as the band
    /// shows them: not while it's off or hidden, folded to their lines while a question has
    /// the keys, or gone then when `when_question` says so.
    fn file_mods_above(
        &self,
        ctx: &RenderCtx<'_>,
        room: usize,
        question_up: bool,
    ) -> Vec<Line<'static>> {
        let band = &self.band;
        let gone = question_up && band.when_question == WhenQuestion::Hide;
        let Some(view) = self
            .file_mods
            .as_ref()
            .filter(|_| band.shown && !band.hidden && !gone)
        else {
            return Vec::new();
        };
        let room = crate::mods::BandRoom {
            screen: usize::from(ctx.width),
            rows: room,
            folded: question_up && band.when_question == WhenQuestion::Fold,
        };
        crate::mods::above_input(&crate::kit::Kit::new(ctx.theme), view, room).lines
    }

    /// The region while a docked view is up: the narrator and the working line stay, then a
    /// blank row, and the view takes the rows the input, the bottom line, and the panel had.
    /// The view keeps its rows when the screen is short; the working line goes first. With
    /// `over_question`, the question that holds the keys draws under the view, whose own keys'
    /// row the caller left out.
    pub fn render_docked(
        &self,
        ctx: &RenderCtx<'_>,
        max_rows: usize,
        text_streaming: bool,
        view: Vec<Line<'static>>,
        over_question: bool,
    ) -> BottomView {
        let mut lines = narrator_lines(self.narrator.as_deref(), ctx);
        match &self.working {
            Some(line) if line.shows(text_streaming) => {
                lines.extend(line.lines(ctx));
                lines.push(Line::default());
            }
            _ if !lines.is_empty() => lines.push(Line::default()),
            _ => {}
        }
        let question = over_question
            .then(|| self.question_under_view(ctx))
            .flatten();
        let question_rows = question.as_ref().map_or(0, |question| question.lines.len());
        if lines.len() + view.len() + question_rows > max_rows {
            lines.clear();
        }
        lines.extend(view);
        let mut cursor = None;
        if let Some(question) = question {
            let top = u16::try_from(lines.len()).unwrap_or(u16::MAX);
            cursor = question
                .cursor
                .map(|(column, row)| (column, row.saturating_add(top)));
            lines.extend(question.lines);
        }
        BottomView { lines, cursor }
    }

    /// The question that holds the keys as it draws under a docked view that stays up: in the
    /// view's own columns, as wide as its rule.
    pub fn question_under_view(&self, ctx: &RenderCtx<'_>) -> Option<QuestionView> {
        let question = self.focused_question()?;
        let ctx = RenderCtx {
            width: crate::views::dock::rule_width(ctx.width),
            ..ctx.with_vim(self.mode.vim.is_some())
        };
        Some(question.layout_under_view(&ctx))
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
            self.workflows.next_change(&self.agents, clock, ctx.theme),
            self.composer.next_change(clock, ctx.theme),
            self.band.next_change(),
            hint,
            notice,
            self.word_notice
                .as_ref()
                .map(|notice| notice.until)
                .filter(|until| *until > clock.now),
            self.word_notice_ends().filter(|ends| *ends > clock.now),
            self.question
                .as_ref()
                .and_then(|question| question.next_change(clock.now)),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// What the screen says about the word `ultracode` in the draft: that it asks for a
    /// workflow, or for a moment that the key ignored it.
    fn word_notice_line(&self, ctx: &RenderCtx<'_>) -> Option<Line<'static>> {
        let still_ignored =
            self.composer.holds_workflow_word() && !self.composer.workflow_requested();
        let ignored = self
            .word_notice
            .as_ref()
            .filter(|notice| notice.until > ctx.clock.now && still_ignored)
            .map(|notice| notice.text.clone());
        let asking = || {
            self.composer.workflow_requested()
                && self
                    .word_notice_ends()
                    .is_none_or(|ends| ends > ctx.clock.now)
        };
        let text = ignored.or_else(|| asking().then(|| requested_notice(&self.workflow_key)))?;
        let width = usize::from(ctx.width);
        let text = sanitize_line(&text).into_owned();
        let pad = width.saturating_sub(crate::text::str_width(&text) + 1);
        let line = Line::from(vec![
            Span::raw(" ".repeat(pad)),
            Span::styled(text, ctx.theme.style(Role::Dim)),
        ]);
        Some(truncate_line(&line, width))
    }

    fn parts(&self, ctx: &RenderCtx<'_>, text_streaming: bool, max_rows: usize) -> Parts {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);

        // The narrator's line sits just above the working line.
        let mut working = narrator_lines(self.narrator.as_deref(), ctx);
        match &self.working {
            Some(line) if line.shows(text_streaming) => {
                working.extend(line.lines(ctx));
                working.push(Line::default());
            }
            None if self.waits_for.is_some() => {
                let waits = self.waits_for.as_deref().unwrap_or_default();
                let line = Line::from(vec![
                    Span::styled(format!("{} ", theme.glyphs.turn_end), dim),
                    Span::styled(sanitize_line(waits).trim().to_owned(), dim),
                ]);
                working.push(truncate_line(&line, width));
                working.push(Line::default());
            }
            _ if !working.is_empty() => working.push(Line::default()),
            _ => {
                if let Some(words) = &self.updated {
                    let row = crate::bridge::status::updated_row(words, theme);
                    working.extend([truncate_line(&row, width), Line::default()]);
                }
            }
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
        let mut queued: Vec<Line<'static>> = queued;
        if let Some(hint) = self
            .send_now_hint
            .as_ref()
            .filter(|_| !self.queued.is_empty())
        {
            let line = Line::from(vec![Span::raw("  "), Span::styled(hint.clone(), dim)]);
            queued.extend([truncate_line(&line, width), Line::default()]);
        }
        // A question with the keys hides it until it's answered or passed on, and so does a
        // picker in the input's place.
        let notice = if self.focused_question().is_some() || self.picker_in_place() {
            Vec::new()
        } else {
            self.notice.lines(ctx)
        };
        let question_ctx = ctx.with_vim(self.mode.vim.is_some());
        let question_in = |rows: usize| {
            let mut view = self
                .question
                .as_ref()
                .map(|question| question.layout_within(&question_ctx, rows))
                .unwrap_or_default();
            if self.focused_question().is_none() {
                view.cursor = None;
            }
            view
        };
        let mut question = question_in(usize::MAX);
        if let Some(pick) = self
            .answered_pick
            .as_ref()
            .filter(|_| self.question.is_none())
        {
            question.lines = vec![pick.line(ctx)];
        }

        let focused = self.composer_has_focus();
        let busy = self.at_work();
        let waiting = self.waiting_question().is_some();
        // With an agent selected in the list, the input is a message to it, as in Claude Code.
        let messaging = self.messaging();
        let message_to = messaging.and_then(AgentRow::placeholder);
        let placeholder =
            message_to
                .as_deref()
                .or(waiting.then_some(if busy || self.run_takes_text {
                    QUESTION_PLACEHOLDER_RUN
                } else {
                    QUESTION_PLACEHOLDER
                }));
        // An example prompt is a way to start, so it waits while work runs or a question is up.
        let offer_example = !busy && self.expanded_question().is_none();
        // A question that holds the keys stands in for the input, as Claude Code's permission
        // prompts do, so nothing on screen looks typeable while typing goes nowhere. A picker
        // or the prompt search it took the keys from waits under it.
        let question_holds_keys = self.focused_question().is_some();
        let search = self
            .history_search
            .as_ref()
            .filter(|_| !question_holds_keys);
        let picker = self.picker.as_ref().filter(|_| !question_holds_keys);
        let mut composer = match search {
            Some(search) => {
                let (lines, cursor) = search.lines(ctx, max_rows);
                ComposerView { lines, cursor }
            }
            None if question_holds_keys || picker.is_some() => ComposerView {
                lines: Vec::new(),
                cursor: (0, 0),
            },
            None => {
                let mut view = self.composer.render_with(
                    ctx,
                    ComposerState {
                        focused: focused || messaging.is_some(),
                        // The mark dims only under a working line: while the work only waits,
                        // a message goes in as a turn of its own, and the mark reads as ready.
                        busy: self.working.is_some(),
                        offer_example,
                        placeholder,
                    },
                );
                // The workflow word's notice sits right above the rule, at its right end.
                if let Some(notice) = self.word_notice_line(ctx) {
                    view.lines.insert(0, notice);
                    view.cursor.1 += 1;
                }
                view
            }
        };
        // The rule over the input names who the message is for.
        if let (Some(agent), Some(rule)) = (messaging, composer.lines.first_mut())
            && self.history_search.is_none()
            && !question_holds_keys
        {
            *rule = agents::message_rule(agent, ctx);
        }
        // An open picker's edge rule is the only one: its rows take the input's place.
        let menu = if search.is_some() || question_holds_keys {
            Vec::new()
        } else if let Some(picker) = picker {
            picker.lines(ctx)
        } else if focused && !self.composer.is_shell() {
            self.menus
                .rows(self.composer.text(), ctx, usize::MAX)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let typing = !self.composer.is_empty();
        let notice_keys = if self.band.has_focus() {
            Some(BAND_KEYS)
        } else {
            self.notice.has_focus().then_some(NOTICE_KEYS)
        };
        // While the draft is a command, the line says what Enter does with it. A notice just
        // raised says more, such as why the command was refused, and a hint would take its
        // slot, so the line is the notice's until it goes.
        let noticed = self
            .mode
            .notice
            .as_ref()
            .is_some_and(|notice| notice.until > ctx.clock.now);
        let command = self
            .composer
            .enter_does(usize::from(ctx.width))
            .filter(|_| focused && search.is_none() && !question_holds_keys && picker.is_none())
            .filter(|_| !noticed);
        let panel_focused = self.agents.has_focus() || self.workflows.selected.is_some();
        // The focused list's keys take the line, as they do under Claude Code's list.
        let list_keys = panel_focused.then_some(LIST_KEYS);
        let mode = self.mode.line(
            ctx,
            ModeState {
                hint: notice_keys
                    .or_else(|| self.active_hint(ctx.clock.now))
                    .or_else(|| self.composer.holds_paste().then_some(PASTING))
                    .or(list_keys)
                    .or(command.as_deref()),
                working: self.working.as_ref().is_some_and(|line| line.esc_stops),
                waiting: self.working.is_none() && self.waits_for.is_some(),
                typing,
                // A question passed on leaves the screen, so what still waits is in `needs_you`.
                pinned_questions: 0,
                panel: panel_has_rows(&self.workflows, &self.agents),
                panel_focused,
                question_focused: self.focused_question().is_some(),
                shell: self.composer.is_shell(),
                // A command runs when Enter is pressed, whatever works above it.
                enter_sends: self
                    .enter_sends
                    .as_deref()
                    .filter(|_| typing && focused && (busy || waiting))
                    .filter(|_| !self.composer.text().trim_start().starts_with('/')),
                line: Some(&self.line),
            },
        );
        let searching = search.is_some();
        // What never gives way to a question: the input, or its menu, and the bottom line. A
        // question too tall for the rest of the screen is laid out again in those rows, where
        // a long list of options takes fewer and its head and its key line stay.
        let under_question = composer.lines.len()
            + match menu.len() {
                0 => usize::from(!searching),
                rows => rows.min(MENU_MIN_ROWS),
            };
        let room = max_rows.saturating_sub(under_question);
        if self.question.is_some() && question.lines.len() > room {
            question = question_in(room);
        }
        let question_rows = question.lines.len();
        Parts {
            working,
            queued,
            band: Vec::new(),
            notice,
            question_gap: question_rows > 0,
            question_tail: question.tail,
            question_extra: question.extra,
            question_head_rows: question_rows.min(2),
            question_cursor: question.cursor,
            question: question.lines,
            composer_cursor: ((focused && picker.is_none()) || searching)
                .then_some(composer.cursor),
            composer: composer.lines,
            menu,
            mode: (!searching).then_some(mode),
            agents: if searching {
                Vec::new()
            } else {
                panel_lines(&self.workflows, &self.agents, ctx)
            },
        }
    }
}

struct Parts {
    working: Vec<Line<'static>>,
    queued: Vec<Line<'static>>,
    /// The mods pinned above the input, over the notice.
    band: Vec<Line<'static>>,
    notice: Vec<Line<'static>>,
    question: Vec<Line<'static>>,
    question_gap: bool,
    /// Rows at the question's bottom (blank row, key hints, footer) that go when rows are short.
    question_tail: usize,
    /// The question's chips, agent, details, and body, which go next.
    question_extra: std::ops::Range<usize>,
    /// Rows at the question's top that always stay: its rule and the title's first row.
    question_head_rows: usize,
    /// (column, row) within `question`.
    question_cursor: Option<(u16, u16)>,
    composer: Vec<Line<'static>>,
    /// (column, row) within `composer`.
    composer_cursor: Option<(u16, u16)>,
    menu: Vec<Line<'static>>,
    /// `None` while the prompt search stands in for it.
    mode: Option<Line<'static>>,
    agents: Vec<Line<'static>>,
}

impl Parts {
    fn rows(&self) -> usize {
        if !self.menu.is_empty() {
            return self.working.len()
                + self.question.len()
                + usize::from(self.question_gap)
                + self.queued.len()
                + self.band.len()
                + self.notice.len()
                + self.composer.len()
                + self.menu.len();
        }
        let agents = match self.agents.len() {
            0 => 0,
            rows => rows + 1,
        };
        self.working.len()
            + self.question.len()
            + usize::from(self.question_gap)
            + self.queued.len()
            + self.band.len()
            + self.notice.len()
            + self.composer.len()
            + usize::from(self.mode.is_some())
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
        // The notice gives way after the panel and before the working line.
        if self.rows() > max_rows {
            self.notice.clear();
        }
        if self.rows() > max_rows {
            self.working.clear();
        }
        if self.rows() > max_rows {
            self.question_gap = false;
        }
        if self.rows() > max_rows && self.question_tail > 0 {
            let keep = self.question.len().saturating_sub(self.question_tail);
            self.question.truncate(keep);
        }
        let over = self.rows().saturating_sub(max_rows);
        // The question's extras go from the top, never into its head rows.
        let extra = self.question_extra.clone();
        let start = extra.start.max(self.question_head_rows);
        let cut = over.min(extra.end.saturating_sub(start));
        if cut > 0 {
            self.question.drain(start..start + cut);
            self.question_cursor = self.question_cursor.and_then(|(column, row)| {
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
        let question_top = lines.len();
        lines.extend(self.question);
        if self.question_gap {
            lines.push(Line::default());
        }
        lines.extend(self.queued);
        lines.extend(self.band);
        lines.extend(self.notice);
        let composer_top = lines.len();
        lines.extend(self.composer);
        if !self.menu.is_empty() {
            lines.extend(self.menu);
        } else if let Some(mode) = self.mode {
            lines.push(mode);
            if !self.agents.is_empty() {
                lines.push(Line::default());
                lines.extend(self.agents);
            }
        }

        let cursor = match (self.question_cursor, self.composer_cursor) {
            (Some((column, row)), _) => Some((column, row + question_top as u16)),
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
mod command_tests;
#[cfg(test)]
mod picker_place_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vim_picture_tests;
