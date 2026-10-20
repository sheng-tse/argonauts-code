//! The Context tab's rows in the panel. Every list shows in full, so the tab is often taller
//! than the panel: it scrolls by rows, and in the cards ↑ and ↓ step through its blocks, a
//! page at a time inside a block taller than the panel.

use std::cell::RefCell;

use argonauts_core::RunId;
use argonauts_core::names::SessionKey;
use crossterm::event::KeyCode;
use ratatui::text::{Line, Span};

use super::lists::Fold;
use crate::kit::{Drawn, Kit};
use crate::theme::Role;

/// One block of the tab.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub drawn: Drawn,
    /// The row the pointer goes on while the keys are on this block.
    pub pointer_row: usize,
    /// The crew agent the block is about, which Enter opens.
    pub agent: Option<(RunId, SessionKey)>,
    /// A list long enough to fold, which Enter folds or shows whole.
    pub fold: Option<Fold>,
}

impl Section {
    pub fn of(drawn: Drawn) -> Self {
        Self {
            drawn,
            ..Self::default()
        }
    }
}

/// What the last frame laid out.
#[derive(Debug, Default)]
struct Laid {
    /// Each block's first row and its height.
    blocks: Vec<(usize, usize)>,
    agents: Vec<Option<(RunId, SessionKey)>>,
    folds: Vec<Option<Fold>>,
    rows: usize,
    /// The rows the panel had for them.
    room: usize,
}

impl Laid {
    fn last_top(&self) -> usize {
        self.rows.saturating_sub(self.room)
    }
}

/// Where the tab is scrolled to, and the block the keys are on.
#[derive(Debug, Default)]
pub struct ContextPane {
    /// The first row shown.
    top: usize,
    focus: usize,
    laid: RefCell<Laid>,
}

impl ContextPane {
    /// The rows the panel shows of `sections`, one blank row between blocks, and whether more
    /// rows are below. With `pointer`, the keys are in the cards, and the block they are on
    /// carries it two cells before its text.
    pub fn rows(
        &mut self,
        kit: &Kit<'_>,
        sections: Vec<Section>,
        room: usize,
        pointer: Option<&str>,
    ) -> (Vec<Line<'static>>, bool) {
        let mut laid = Laid {
            room,
            ..Laid::default()
        };
        self.focus = self.focus.min(sections.len().saturating_sub(1));
        let mut rows: Vec<Line<'static>> = Vec::new();
        for (nth, section) in sections.into_iter().enumerate() {
            if !rows.is_empty() {
                rows.push(Line::default());
            }
            laid.blocks.push((rows.len(), section.drawn.height()));
            laid.agents.push(section.agent);
            laid.folds.push(section.fold);
            for (row, line) in section.drawn.lines.into_iter().enumerate() {
                let pointed = nth == self.focus && row == section.pointer_row;
                rows.push(match pointer.filter(|_| pointed) {
                    Some(pointer) => with_pointer(kit, line, pointer),
                    None => line,
                });
            }
        }
        laid.rows = rows.len();
        self.top = self.top.min(laid.last_top());
        let more = self.top + room < rows.len();
        let shown = rows.into_iter().skip(self.top).take(room).collect();
        *self.laid.borrow_mut() = laid;
        (shown, more)
    }

    /// The keys are on the first block with the tab's top in view, so ↑ leaves the cards.
    pub fn on_first(&self) -> bool {
        self.focus == 0 && self.top == 0
    }

    /// The crew agent the keys are on, if they are on one.
    pub fn agent(&self) -> Option<(RunId, SessionKey)> {
        self.laid.borrow().agents.get(self.focus).cloned().flatten()
    }

    /// How the list the keys are on shows, where it is long enough to fold.
    pub fn fold(&self) -> Option<Fold> {
        self.laid.borrow().folds.get(self.focus).copied().flatten()
    }

    /// Whether the tab is taller than the panel.
    #[cfg(test)]
    pub fn scrolls(&self) -> bool {
        let laid = self.laid.borrow();
        laid.rows > laid.room
    }

    /// The block the keys are on, counted from the first.
    #[cfg(test)]
    pub fn focused(&self) -> usize {
        self.focus
    }

    /// Moves by a key. False for a key that isn't the tab's.
    pub fn key(&mut self, code: KeyCode) -> bool {
        let laid = self.laid.borrow();
        let page = laid.room.saturating_sub(1).max(1);
        let last = laid.blocks.len().saturating_sub(1);
        let (start, height) = laid.blocks.get(self.focus).copied().unwrap_or((0, 0));
        match code {
            // More of a tall block first, then the next block.
            KeyCode::Down if start + height > self.top + laid.room => {
                let bottom = (start + height).saturating_sub(laid.room);
                self.top = (self.top + page).min(bottom);
            }
            KeyCode::Down => {
                self.focus = (self.focus + 1).min(last);
                self.top = revealed(&laid, self.focus, self.top);
            }
            KeyCode::Up if start < self.top => {
                self.top = self.top.saturating_sub(page).max(start);
            }
            KeyCode::Up => {
                self.focus = self.focus.saturating_sub(1);
                self.top = revealed(&laid, self.focus, self.top);
            }
            KeyCode::PageDown => {
                self.top = (self.top + page).min(laid.last_top());
                self.focus = block_at(&laid, self.top).unwrap_or(last);
            }
            KeyCode::PageUp => {
                self.top = self.top.saturating_sub(page);
                self.focus = block_at(&laid, self.top).unwrap_or(0);
            }
            KeyCode::Home => (self.top, self.focus) = (0, 0),
            KeyCode::End => (self.top, self.focus) = (laid.last_top(), last),
            _ => return false,
        }
        true
    }
}

/// The top row that brings block `nth` into view from `top`: its first row, and as much of
/// it as the panel holds.
fn revealed(laid: &Laid, nth: usize, top: usize) -> usize {
    let Some((start, height)) = laid.blocks.get(nth).copied() else {
        return top;
    };
    let end = start + height.min(laid.room);
    if start < top {
        start
    } else if end > top + laid.room {
        end - laid.room
    } else {
        top
    }
}

/// The block the row `top` is in, or the next one when `top` is the blank row above it.
fn block_at(laid: &Laid, top: usize) -> Option<usize> {
    laid.blocks
        .iter()
        .position(|(start, height)| start + height > top)
}

/// `line` with the pointer two cells before its first word, in place of the blanks there. A
/// line with no room for it stays as it is.
fn with_pointer(kit: &Kit<'_>, line: Line<'static>, pointer: &str) -> Line<'static> {
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let blanks = text.len() - text.trim_start_matches(' ').len();
    if blanks < 2 || blanks == text.len() {
        return line;
    }
    let mut spans = vec![
        Span::raw(" ".repeat(blanks - 2)),
        Span::styled(format!("{pointer} "), kit.fg(Role::Question)),
    ];
    let mut skip = blanks;
    for span in line.spans {
        if skip >= span.content.len() {
            skip -= span.content.len();
            continue;
        }
        let kept = span.content[skip..].to_owned();
        skip = 0;
        spans.push(Span::styled(kept, span.style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::plain_text;
    use crate::theme::Theme;

    fn block(name: &str, rows: usize) -> Section {
        let mut drawn = Drawn::new();
        for row in 0..rows {
            drawn.push(vec![Span::raw(format!("     {name}{row}"))]);
        }
        Section::of(drawn)
    }

    fn shown(pane: &mut ContextPane, sections: Vec<Section>, room: usize) -> Vec<String> {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let (rows, _) = pane.rows(&kit, sections, room, Some(">"));
        plain_text(&rows).lines().map(str::to_owned).collect()
    }

    fn tab() -> Vec<Section> {
        vec![block("a", 2), block("b", 9), block("c", 2)]
    }

    #[test]
    fn blocks_stand_a_blank_row_apart_and_the_pointer_marks_the_one_with_the_keys() {
        let mut pane = ContextPane::default();
        let rows = shown(&mut pane, tab(), 20);
        assert_eq!(rows[0], "   > a0");
        assert_eq!(rows[1], "     a1");
        assert_eq!(rows[2], "");
        assert_eq!(rows[3], "     b0");
        assert_eq!(rows.len(), 15);
        assert!(!pane.scrolls());
        assert!(pane.on_first());
    }

    #[test]
    fn down_pages_through_a_tall_block_before_it_leaves_it() {
        let mut pane = ContextPane::default();
        shown(&mut pane, tab(), 6);
        assert!(pane.scrolls());
        assert!(pane.key(KeyCode::Down));
        let rows = shown(&mut pane, tab(), 6);
        assert_eq!(rows[0], "   > b0", "the next block comes to the top");
        assert!(pane.key(KeyCode::Down));
        let rows = shown(&mut pane, tab(), 6);
        assert_eq!(rows[5], "     b8", "then the rest of it");
        assert!(pane.key(KeyCode::Down));
        let rows = shown(&mut pane, tab(), 6);
        assert_eq!(rows[4], "   > c0");
        assert_eq!(rows[5], "     c1");
        // And back up the same way.
        assert!(pane.key(KeyCode::Up));
        let rows = shown(&mut pane, tab(), 6);
        assert_eq!(rows[0], "   > b0");
        assert!(pane.key(KeyCode::Up));
        shown(&mut pane, tab(), 6);
        assert!(pane.on_first());
    }

    #[test]
    fn a_page_and_the_ends_move_the_rows_and_the_keys_follow() {
        let mut pane = ContextPane::default();
        shown(&mut pane, tab(), 6);
        assert!(pane.key(KeyCode::End));
        let rows = shown(&mut pane, tab(), 6);
        assert_eq!(rows[5], "     c1");
        assert_eq!(rows[4], "   > c0");
        assert!(pane.key(KeyCode::PageUp));
        let rows = shown(&mut pane, tab(), 6);
        assert_eq!(rows[0], "     b1");
        assert!(pane.key(KeyCode::Home));
        assert!(pane.on_first());
        assert!(!pane.key(KeyCode::Char('x')));
    }

    #[test]
    fn enter_has_the_agent_of_the_block_with_the_keys() {
        let mut pane = ContextPane::default();
        let agent = (RunId::new("run-03").unwrap(), SessionKey::numbered(2));
        let sections = || {
            vec![
                block("a", 2),
                Section {
                    agent: Some((RunId::new("run-03").unwrap(), SessionKey::numbered(2))),
                    pointer_row: 1,
                    ..block("b", 3)
                },
            ]
        };
        shown(&mut pane, sections(), 20);
        assert_eq!(pane.agent(), None);
        pane.key(KeyCode::Down);
        let rows = shown(&mut pane, sections(), 20);
        assert_eq!(pane.agent(), Some(agent));
        assert_eq!(rows[4], "   > b1", "under the column heads");
    }
}
