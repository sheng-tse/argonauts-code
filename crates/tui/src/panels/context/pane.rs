//! The Context tab's rows in the panel. The tab can be taller than the panel: it scrolls by
//! rows, and in the cards ↑ and ↓ step through its blocks, a page at a time inside a block
//! taller than the panel. The pane keeps which lists are open and which agent's block shows.

use std::cell::RefCell;
use std::collections::BTreeSet;

use argonauts_core::RunId;
use crossterm::event::KeyCode;
use ratatui::text::{Line, Span};

pub use super::lists::List;
use crate::base::kit::{Drawn, Kit};
use crate::base::theme::Role;

/// What Enter does on a block: folds or opens a list, or opens a crew agent's own block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    List(List),
    Agent { run: RunId, label: String },
}

/// One block of the tab.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub drawn: Drawn,
    /// The row the pointer goes on while the keys are on this block.
    pub pointer_row: usize,
    /// What Enter does while the keys are on it.
    pub target: Option<Target>,
    /// It follows the block before with no blank row between, as a list's lines do.
    pub joined: bool,
}

impl Section {
    pub fn of(drawn: Drawn) -> Self {
        Self {
            drawn,
            ..Self::default()
        }
    }
}

/// The sections as blocks a blank row apart, each joined one part of the block above.
pub fn blocks(sections: Vec<Section>) -> Vec<Drawn> {
    let mut blocks: Vec<Drawn> = Vec::new();
    for section in sections {
        match blocks.last_mut() {
            Some(above) if section.joined => above.append(section.drawn),
            _ => blocks.push(section.drawn),
        }
    }
    blocks
}

/// What the last frame laid out.
#[derive(Debug, Default)]
struct Laid {
    /// Each block's first row and its height.
    blocks: Vec<(usize, usize)>,
    targets: Vec<Option<Target>>,
    rows: usize,
    /// The rows the panel had for them.
    room: usize,
}

impl Laid {
    fn last_top(&self) -> usize {
        self.rows.saturating_sub(self.room)
    }
}

/// Where the tab is scrolled to, the block the keys are on, the lists open, and the agent
/// whose block shows.
#[derive(Debug, Default)]
pub struct ContextPane {
    /// The first row shown.
    top: usize,
    focus: usize,
    laid: RefCell<Laid>,
    /// Every list opens, as `/context all` asks.
    every_list: bool,
    /// The lists Enter turned the other way from how they start.
    turned: BTreeSet<List>,
    /// The agent whose own block shows, with where the list was when it opened.
    opened: Option<Opened>,
}

/// A crew agent's block, open in the tab's place.
#[derive(Debug, Clone)]
struct Opened {
    run: RunId,
    label: String,
    /// The list's place, which Esc goes back to.
    top: usize,
    focus: usize,
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
        // An agent's own block has nothing to choose in it.
        let pointer = pointer.filter(|_| self.opened.is_none());
        let mut rows: Vec<Line<'static>> = Vec::new();
        for (nth, section) in sections.into_iter().enumerate() {
            if !rows.is_empty() && !section.joined {
                rows.push(Line::default());
            }
            laid.blocks.push((rows.len(), section.drawn.height()));
            laid.targets.push(section.target);
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

    /// What Enter does where the keys are.
    pub fn target(&self) -> Option<Target> {
        self.laid
            .borrow()
            .targets
            .get(self.focus)
            .cloned()
            .flatten()
    }

    /// Whether `list` shows a line each.
    pub fn is_open(&self, list: &List) -> bool {
        self.every_list != self.turned.contains(list)
    }

    /// Every list shows a line each, as `/context all` asks.
    pub fn open_every_list(&mut self) {
        self.every_list = true;
        self.turned.clear();
    }

    /// The crew agent whose own block shows, by its run and label.
    pub fn opened(&self) -> Option<(&RunId, &str)> {
        self.opened
            .as_ref()
            .map(|opened| (&opened.run, opened.label.as_str()))
    }

    /// Enter: folds or opens the list the keys are on, or opens the agent's own block. False
    /// where the keys are on neither.
    pub fn enter(&mut self) -> bool {
        match self.target() {
            Some(Target::List(list)) => {
                if !self.turned.remove(&list) {
                    self.turned.insert(list);
                }
                true
            }
            Some(Target::Agent { run, label }) => {
                self.opened = Some(Opened {
                    run,
                    label,
                    top: self.top,
                    focus: self.focus,
                });
                (self.top, self.focus) = (0, 0);
                true
            }
            None => false,
        }
    }

    /// Back from an agent's block to the list, where the keys were. False with none open.
    pub fn back(&mut self) -> bool {
        let Some(opened) = self.opened.take() else {
            return false;
        };
        (self.top, self.focus) = (opened.top, opened.focus);
        true
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
    use crate::base::text::plain_text;
    use crate::base::theme::Theme;

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

    /// Enter opens the agent the keys are on, Esc comes back to where the keys were, and a
    /// list's lines follow its title with no blank row between.
    #[test]
    fn enter_opens_the_agent_with_the_keys_and_back_returns_to_it() {
        let mut pane = ContextPane::default();
        let agent = Target::Agent {
            run: RunId::new("run-03").unwrap(),
            label: "build:x".to_owned(),
        };
        let sections = || {
            vec![
                block("a", 2),
                Section {
                    target: Some(Target::List(List::Skills)),
                    ..block("b", 1)
                },
                Section {
                    target: Some(agent.clone()),
                    joined: true,
                    ..block("c", 1)
                },
            ]
        };
        let rows = shown(&mut pane, sections(), 20);
        assert_eq!(rows[3], "     b0");
        assert_eq!(rows[4], "     c0", "joined to the block above");
        assert!(!pane.enter(), "nothing to open on the first block");
        pane.key(KeyCode::Down);
        shown(&mut pane, sections(), 20);
        assert!(!pane.is_open(&List::Skills));
        assert!(pane.enter());
        assert!(pane.is_open(&List::Skills));
        pane.key(KeyCode::Down);
        let rows = shown(&mut pane, sections(), 20);
        assert_eq!(rows[4], "   > c0");
        assert_eq!(pane.target(), Some(agent.clone()));
        assert!(pane.enter());
        assert_eq!(
            pane.opened(),
            Some((&RunId::new("run-03").unwrap(), "build:x"))
        );
        assert!(pane.back());
        assert_eq!(pane.opened(), None);
        shown(&mut pane, sections(), 20);
        assert_eq!(pane.target(), Some(agent));
        assert!(!pane.back());
    }

    /// `/context all` opens every list, and Enter still folds one.
    #[test]
    fn every_list_opens_and_one_still_folds() {
        let mut pane = ContextPane::default();
        pane.open_every_list();
        assert!(pane.is_open(&List::McpTools));
        let sections = vec![Section {
            target: Some(Target::List(List::McpTools)),
            ..block("a", 1)
        }];
        shown(&mut pane, sections, 5);
        assert!(pane.enter());
        assert!(!pane.is_open(&List::McpTools));
        assert!(pane.is_open(&List::Skills));
    }
}
