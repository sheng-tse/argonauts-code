//! Editing a setting inside `/config` or `/approvals`: the picker's values or typed line, then
//! the scope question, drawn in the view's box in place of its list.

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::flow::{self, Step};
use crate::bottom::{Picker, PickerEvent};
use crate::render::{Clock, RenderCtx};
use crate::views::panes;
use crate::views::{ViewCtx, ViewOutcome};

/// Gives the key to the open editor. `None` when there's no editor, so the view handles it.
pub fn edit_key(editor: &mut Option<Picker>, key: KeyEvent) -> Option<ViewOutcome> {
    let picker = editor.as_mut()?;
    let event = picker.handle_key(key);
    if event == PickerEvent::Unhandled {
        return Some(ViewOutcome::Ignored);
    }
    // The editor is a level under the view's list, so ← from its first level goes back to it.
    if event == PickerEvent::Left && !picker.flow.can_go_back() && !picker.flow.arrows_adjust() {
        *editor = None;
        return Some(ViewOutcome::Consumed);
    }
    Some(match flow::advance(picker, event) {
        Step::Stay | Step::Preview { .. } => ViewOutcome::Consumed,
        Step::Ask(view) => ViewOutcome::Request(view),
        Step::Send(command) => ViewOutcome::Send(command),
        Step::Done(command) => {
            *editor = None;
            ViewOutcome::Send(command)
        }
        // Esc leaves the editor for the list; the change is dropped.
        Step::Cancel { .. } => {
            *editor = None;
            ViewOutcome::Consumed
        }
    })
}

/// Gives a paste to the open editor's typed line or filter. `None` when there's no editor.
pub fn edit_paste(editor: &mut Option<Picker>, text: &str) -> Option<ViewOutcome> {
    let picker = editor.as_mut()?;
    let event = picker.handle_paste(text);
    if event == PickerEvent::Unhandled {
        return Some(ViewOutcome::Ignored);
    }
    Some(match flow::advance(picker, event) {
        Step::Ask(view) => ViewOutcome::Request(view),
        _ => ViewOutcome::Consumed,
    })
}

/// Draws the editor in `area`, the box's inside. The box is its frame, so its edge rule
/// isn't drawn.
pub fn draw_editor(picker: &Picker, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
    let render = RenderCtx::new(area.width, ctx.theme, Clock::still(ctx.now));
    let lines = picker.lines(&render);
    panes::put_lines(buf, area, lines.get(1..).unwrap_or_default());
}
