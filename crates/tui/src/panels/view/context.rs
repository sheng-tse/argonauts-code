//! The Context tab on the panel: its sections, scrolled by its pane with the pointer on the
//! block the keys are on, and its keys.

use argonauts_core::RunId;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::{OverviewView, note, run_name};
use crate::base::draw::panes::Marks;
use crate::base::kit::Kit;
use crate::base::text::truncate_spans;
use crate::frame::state::Places;
use crate::frame::view::{ViewCtx, ViewOutcome};
use crate::panels::context::{self, ContextTab, Section};
use crate::panels::view::subtabs::Level;

impl OverviewView {
    /// The Context tab's blocks: the main session as its CLI counted it, each run's crew, and
    /// then your mods for the tab. With a crew agent opened, its own block alone.
    pub(super) fn context_sections(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        width: usize,
    ) -> Vec<Section> {
        let Some(view) = &self.context else {
            return vec![Section::of(note(kit, "Reading context…"))];
        };
        let state = ctx.state;
        let places = match &self.places {
            Some(places) => places.clone(),
            None => Places::of_conversation(state),
        };
        let run_title = |run: &RunId| {
            let known = state.runs.get(run).map(|run| run.title.trim().to_owned());
            known
                .filter(|title| !title.is_empty())
                .or_else(|| run_name(ctx, run))
                .unwrap_or_else(|| run.to_string())
        };
        let facts = context::CrewFacts::of(state, view);
        let tab = ContextTab {
            view,
            names: &state.names,
            count: state.conversation.context.as_ref(),
            main: state.conversation.worker.as_ref(),
            places: &places,
            now: self.wall(),
            run_title: &run_title,
            facts: &facts,
            pane: &self.pane,
        };
        if self.pane.opened().is_some() {
            return tab.sections(kit, width);
        }
        let mut sections = tab.sections(kit, width);
        if let (Some(view), Some(place)) = (state.mods.as_ref(), self.tab.mod_place()) {
            let yours = crate::panels::mods::on_tab(kit, view, place, width);
            if !yours.lines.is_empty() {
                sections.push(Section::of(yours));
            }
        }
        sections
    }

    /// The Context tab's rows as its pane scrolls them, with the pointer on the block the keys
    /// are on, and the key line under them.
    pub(super) fn render_context(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &ViewCtx<'_>,
        room: usize,
    ) {
        let kit = Kit::new(ctx.theme);
        let width = usize::from(area.width);
        let sections = self.context_sections(&kit, ctx, width);
        let pointer = (self.level() == Level::Cards).then(|| Marks::for_theme(ctx.theme).selected);
        let (rows, more) = self.pane.rows(&kit, sections, room, pointer);
        let top = area.y + self.head_rows();
        for (row, line) in rows.iter().enumerate() {
            buf.set_line(area.x, top + row as u16, line, area.width);
        }
        let keys = self.keys(ctx, more, width);
        let keys = Line::from(truncate_spans(&[kit.dim(format!(" {keys}"))], width));
        buf.set_line(area.x, area.bottom() - 1, &keys, area.width);
    }

    /// In the cards ↑ and ↓ step through the tab's blocks, Enter folds or opens the list the
    /// keys are on or opens the agent's own block, and ← comes back from it as Esc does. A page
    /// at a time works from the tabs too.
    pub(super) fn context_key(&mut self, key: KeyEvent, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        match key.code {
            KeyCode::Enter => {
                self.pane.enter();
                ViewOutcome::Consumed
            }
            KeyCode::Left => {
                self.pane.back();
                ViewOutcome::Consumed
            }
            KeyCode::Right => ViewOutcome::Consumed,
            KeyCode::Char('q') => ViewOutcome::Close,
            code if self.pane.key(code) => ViewOutcome::Consumed,
            _ => ViewOutcome::Ignored,
        }
    }
}
