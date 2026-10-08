//! A committed cell renders once, at the width of the frame that prints it, and goes into
//! scrollback. Live cells redraw every frame until they finish, then print whole. Only the chat
//! answer streams into scrollback line by line; agents' text stays inside their live blocks so
//! lines from several agents never interleave.

use std::collections::VecDeque;
use std::ops::Range;

use ratatui::text::Line;

use crate::cell::{AssistantMessage, Cell, HistoryCell, SessionHeader};
use crate::markdown::stream::MarkdownStream;
use crate::render::RenderCtx;

#[derive(Debug, Clone, PartialEq)]
pub struct LiveBlock {
    /// The live cell drawn, or `None` for the stream's tail.
    pub id: Option<CellId>,
    /// False when it continues the last printed item, as a stream's tail does once the
    /// answer's first rows have printed.
    pub gap_before: bool,
    pub lines: Vec<Line<'static>>,
}

/// Names a live cell or a stream so the host can update it later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellId(u64);

impl CellId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Default)]
pub struct History {
    /// Kept for the transcript overlay.
    cells: Vec<Cell>,
    queue: VecDeque<Pending>,
    active: Vec<(CellId, Cell)>,
    stream: Option<OpenStream>,
    printed_any: bool,
    next_id: u64,
    /// Where the last drain printed the header, as a range of the rows it returned.
    header_rows: Option<Range<usize>>,
    /// The header as it now reads, to print over the one on screen.
    header_redrawn: Option<Vec<Line<'static>>>,
}

#[derive(Debug)]
enum Pending {
    /// Index into `cells`.
    Cell(usize),
    /// The last rows of a finished answer. Its first rows may already be out.
    StreamEnd {
        stream: Box<MarkdownStream>,
        printed: bool,
        /// Blank rows held back from its last batch, which print only if rows follow.
        blanks: Vec<Line<'static>>,
    },
    /// The header at this index in `cells` changed, and prints over its old rows.
    HeaderChanged(usize),
}

#[derive(Debug)]
struct OpenStream {
    id: CellId,
    stream: MarkdownStream,
    printed: bool,
    stamp: Option<String>,
    /// Blank rows a batch of stable rows ended with, such as the separator before a block that
    /// has no rows yet. They wait for the next row, so an answer stopped between blocks never
    /// ends in a gap.
    blanks: Vec<Line<'static>>,
    /// Cells committed while it streams. They print after it, so scrollback keeps order.
    held: Vec<Cell>,
    /// The finished reply, kept in place of what streamed when the two differ after rows of it
    /// printed.
    reply: Option<String>,
}

/// `rows` after the blank rows held back before them, keeping back the blank rows they end
/// with. Rows that are all blank are held whole.
fn release_blanks(
    mut rows: Vec<Line<'static>>,
    held: &mut Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    let blank = |row: &Line<'_>| row.spans.iter().all(|span| span.content.trim().is_empty());
    let trailing = rows.iter().rev().take_while(|row| blank(row)).count();
    let kept = rows.len() - trailing;
    let tail = rows.split_off(kept);
    if rows.is_empty() {
        held.extend(tail);
        return rows;
    }
    let mut out = std::mem::replace(held, tail);
    out.append(&mut rows);
    out
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Prints on the next frame, or after the open stream ends.
    pub fn push(&mut self, cell: impl Into<Cell>) {
        let cell = cell.into();
        match self.stream.as_mut() {
            Some(open) => open.held.push(cell),
            None => self.commit(cell),
        }
    }

    /// Kept for the transcript overlay and never printed, such as the events a resumed run
    /// replays behind its condensed summary. Nothing streams during a replay, so it goes
    /// straight into [`cells`](Self::cells).
    pub fn keep(&mut self, cell: impl Into<Cell>) {
        self.cells.push(cell.into());
    }

    fn commit(&mut self, cell: Cell) {
        self.cells.push(cell);
        self.queue.push_back(Pending::Cell(self.cells.len() - 1));
    }

    /// The header printed last now reads as `header`, such as once the setup saves. It prints
    /// over the old one where that still shows, so the screen never stacks two headers. False
    /// when no header printed yet.
    pub fn replace_header(&mut self, header: SessionHeader) -> bool {
        let Some(index) = self
            .cells
            .iter()
            .rposition(|cell| matches!(cell, Cell::Header(_)))
        else {
            return false;
        };
        self.cells[index] = Cell::Header(header);
        self.queue.push_back(Pending::HeaderChanged(index));
        true
    }

    /// Where the rows of the last [`drain_printable`](Self::drain_printable) hold the header.
    pub fn take_header_rows(&mut self) -> Option<Range<usize>> {
        self.header_rows.take()
    }

    /// The changed header's rows, to print over the old ones.
    pub fn take_header_redrawn(&mut self) -> Option<Vec<Line<'static>>> {
        self.header_redrawn.take()
    }

    /// Adds a cell to the live region, where it redraws until [`finish_active`](Self::finish_active).
    pub fn push_active(&mut self, cell: impl Into<Cell>) -> CellId {
        let id = self.alloc_id();
        self.active.push((id, cell.into()));
        id
    }

    pub fn active(&self, id: CellId) -> Option<&Cell> {
        self.active
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, cell)| cell)
    }

    pub fn active_mut(&mut self, id: CellId) -> Option<&mut Cell> {
        self.active
            .iter_mut()
            .find(|(i, _)| *i == id)
            .map(|(_, cell)| cell)
    }

    pub fn active_cells(&self) -> impl Iterator<Item = (CellId, &Cell)> {
        self.active.iter().map(|(id, cell)| (*id, cell))
    }

    /// Moves a live cell into history, where it prints like [`push`](Self::push).
    pub fn finish_active(&mut self, id: CellId) -> bool {
        match self.remove_active(id) {
            Some(cell) => {
                self.push(cell);
                true
            }
            None => false,
        }
    }

    /// Moves the open answer and every live cell into history as they stand, so an exit leaves
    /// them in scrollback instead of wiping them with the live region.
    pub fn settle(&mut self) {
        if let Some(id) = self.stream.as_ref().map(|open| open.id) {
            self.finish_stream(id);
        }
        let ids: Vec<CellId> = self.active.iter().map(|(id, _)| *id).collect();
        for id in ids {
            self.finish_active(id);
        }
    }

    /// Drops a live cell without printing it.
    pub fn remove_active(&mut self, id: CellId) -> Option<Cell> {
        let index = self.active.iter().position(|(i, _)| *i == id)?;
        Some(self.active.remove(index).1)
    }

    /// Opens a streaming assistant message, finishing any stream still open.
    pub fn start_stream(&mut self) -> CellId {
        if let Some(open) = &self.stream {
            let id = open.id;
            self.finish_stream(id);
        }
        let id = self.alloc_id();
        self.stream = Some(OpenStream {
            id,
            stream: MarkdownStream::new(),
            printed: false,
            stamp: None,
            blanks: Vec::new(),
            held: Vec::new(),
            reply: None,
        });
        id
    }

    /// The transcript overlay shows the stamp above the answer, such as
    /// `01:07 PM claude-haiku-4-5`.
    pub fn stamp_stream(&mut self, id: CellId, stamp: impl Into<String>) -> bool {
        match self.stream.as_mut() {
            Some(open) if open.id == id => {
                open.stamp = Some(stamp.into());
                true
            }
            _ => false,
        }
    }

    pub fn push_stream(&mut self, id: CellId, delta: &str) -> bool {
        match self.stream.as_mut() {
            Some(open) if open.id == id => {
                open.stream.push(delta);
                true
            }
            _ => false,
        }
    }

    /// Ends the answer with `markdown`, the reply as the agent finished it. A reply that only
    /// adds to what streamed prints the rest. One that differs replaces the preview when none of
    /// it printed yet; otherwise the printed rows stay and the overlay keeps the reply.
    pub fn finish_stream_with(&mut self, id: CellId, markdown: &str) -> bool {
        let Some(open) = self.stream.as_mut().filter(|open| open.id == id) else {
            return false;
        };
        let source = open.stream.source();
        if source.trim_end() != markdown.trim_end() {
            if let Some(rest) = markdown.strip_prefix(source) {
                let rest = rest.to_owned();
                open.stream.push(&rest);
            } else if !open.printed {
                open.stream = MarkdownStream::new();
                open.stream.push(markdown);
                open.blanks.clear();
            } else {
                open.reply = Some(markdown.to_owned());
            }
        }
        self.finish_stream(id)
    }

    /// Its last rows print on the next frame, and the whole message is kept as an assistant
    /// cell.
    pub fn finish_stream(&mut self, id: CellId) -> bool {
        if self.stream.as_ref().is_none_or(|open| open.id != id) {
            return false;
        }
        let Some(open) = self.stream.take() else {
            return false;
        };
        let source = open
            .reply
            .clone()
            .unwrap_or_else(|| open.stream.source().to_owned());
        if !source.trim().is_empty() {
            let mut message = AssistantMessage::new(source);
            message.stamp = open.stamp;
            self.cells.push(Cell::Assistant(message));
        }
        self.queue.push_back(Pending::StreamEnd {
            stream: Box::new(open.stream),
            printed: open.printed,
            blanks: open.blanks,
        });
        for cell in open.held {
            self.commit(cell);
        }
        true
    }

    pub fn is_streaming(&self) -> bool {
        self.stream.is_some()
    }

    /// Part of the answer may have printed already, so a transcript view that shows everything
    /// draws it from here, whole.
    pub fn stream_source(&self) -> Option<&str> {
        self.stream.as_ref().map(|open| open.stream.source())
    }

    /// Cells committed while the answer streams. They join [`cells`](Self::cells) when it ends.
    pub fn held_cells(&self) -> &[Cell] {
        self.stream
            .as_ref()
            .map_or(&[], |open| open.held.as_slice())
    }

    pub fn printed_any(&self) -> bool {
        self.printed_any
    }

    /// Cells held behind an open stream don't count.
    pub fn has_unprinted(&self) -> bool {
        !self.queue.is_empty()
            || self
                .stream
                .as_ref()
                .is_some_and(|open| open.stream.has_new_lines())
    }

    pub fn is_animating(&self) -> bool {
        self.active.iter().any(|(_, cell)| cell.is_animating())
    }

    /// Rows to print above the live region this frame, blank separators included. Each cell
    /// renders once, at `ctx.width` with a still clock.
    pub fn drain_printable(&mut self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        while let Some(pending) = self.queue.pop_front() {
            match pending {
                Pending::Cell(index) => {
                    let cell = &self.cells[index];
                    let header = matches!(cell, Cell::Header(_));
                    let (rows, continues) = (cell.lines(ctx), cell.continues_previous());
                    let printed = self.print(&mut out, rows, continues);
                    if header {
                        self.header_rows = printed;
                    }
                }
                Pending::StreamEnd {
                    mut stream,
                    printed,
                    mut blanks,
                } => {
                    // Blank rows at the very end go, held ones included.
                    let rows = release_blanks(stream.finish(ctx), &mut blanks);
                    self.print(&mut out, rows, printed);
                }
                Pending::HeaderChanged(index) => {
                    self.header_redrawn = Some(self.cells[index].lines(ctx));
                }
            }
        }
        if let Some(mut open) = self.stream.take() {
            let rows = open.stream.take_stable(ctx);
            let rows = release_blanks(rows, &mut open.blanks);
            if !rows.is_empty() {
                self.print(&mut out, rows, open.printed);
                open.printed = true;
            }
            self.stream = Some(open);
        }
        out
    }

    /// Where `rows` went in `out`, after any blank row put before them.
    fn print(
        &mut self,
        out: &mut Vec<Line<'static>>,
        rows: Vec<Line<'static>>,
        continues: bool,
    ) -> Option<Range<usize>> {
        if rows.is_empty() {
            return None;
        }
        if self.printed_any && !continues {
            out.push(Line::default());
        }
        let start = out.len();
        out.extend(rows);
        self.printed_any = true;
        Some(start..out.len())
    }

    /// The stream's tail, then each live cell, rendered in full. The live region shrinks blocks
    /// that don't fit with [`HistoryCell::lines_within`].
    pub fn live_blocks(&self, ctx: &RenderCtx<'_>) -> Vec<LiveBlock> {
        let mut blocks = Vec::new();
        if let Some(open) = &self.stream {
            let mut lines = open.stream.tail(ctx);
            // Held blank rows lead the tail, so the screen reads as it will print.
            if !lines.is_empty() {
                lines.splice(0..0, open.blanks.iter().cloned());
            }
            if !lines.is_empty() {
                blocks.push(LiveBlock {
                    id: None,
                    gap_before: !open.printed && self.printed_any,
                    lines,
                });
            }
        }
        // Live cells are previews that redraw in place, so long code rows are cut, not wrapped.
        // The transcript overlay shows them whole.
        let live = ctx.with_live(!ctx.expanded);
        for (id, cell) in &self.active {
            let lines = cell.lines(&live);
            if lines.is_empty() {
                continue;
            }
            let gap_before = !blocks.is_empty() || self.printed_any;
            blocks.push(LiveBlock {
                id: Some(*id),
                gap_before,
                lines,
            });
        }
        blocks
    }

    fn alloc_id(&mut self) -> CellId {
        self.next_id += 1;
        CellId(self.next_id)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::cell::{Recap, ToolCall, ToolItem, ToolKind, ToolState, UserMessage};
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn drain(history: &mut History, theme: &Theme) -> String {
        let ctx = RenderCtx::new(60, theme, Clock::still(Instant::now()));
        plain_text(&history.drain_printable(&ctx))
    }

    fn header(agent: &str) -> SessionHeader {
        SessionHeader {
            app: "Argonauts Code".to_owned(),
            version: "0.0.1".to_owned(),
            main_agent: Some(agent.to_owned()),
            model: None,
            effort: None,
            workflow: "solo".to_owned(),
            folder: "~/repo".to_owned(),
            agents: Vec::new(),
            runs_going: 0,
            runs_need_you: 0,
        }
    }

    /// The drain says where the header's rows are, and a changed header comes back as rows to
    /// print over them, never as rows of its own.
    #[test]
    fn a_changed_header_prints_over_the_old_one() {
        let theme = theme();
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let mut history = History::new();
        assert!(!history.replace_header(header("Codex")));
        history.push(UserMessage::new("hi"));
        history.push(header("Claude Code"));
        history.push(UserMessage::new("bye"));
        let rows = history.drain_printable(&ctx);
        let at = history.take_header_rows().expect("the header's rows");
        let shown = plain_text(&rows[at.clone()]);
        assert!(shown.contains("Claude Code · solo"), "{shown}");
        assert_eq!(at.start, 2, "after the first row and the blank one");
        assert_eq!(plain_text(&rows[at.end..]), "\n❯ bye");
        assert_eq!(history.take_header_redrawn(), None);

        assert!(history.replace_header(header("Codex")));
        assert!(history.has_unprinted());
        assert!(history.drain_printable(&ctx).is_empty());
        assert_eq!(history.take_header_rows(), None);
        let redrawn = history.take_header_redrawn().expect("the new header");
        assert_eq!(redrawn.len(), at.len());
        assert!(plain_text(&redrawn).contains("Codex · solo"));
        let headers = history
            .cells()
            .iter()
            .filter(|cell| matches!(cell, Cell::Header(_)))
            .count();
        assert_eq!(headers, 1, "the overlay keeps one header too");
    }

    #[test]
    fn items_print_once_with_one_blank_row_between() {
        let theme = theme();
        let mut history = History::new();
        assert!(!history.has_unprinted());
        history.push(UserMessage::new("hi"));
        history.push(AssistantMessage::new("Hello."));
        assert!(history.has_unprinted());
        assert_eq!(drain(&mut history, &theme), "❯ hi\n\n● Hello.");
        assert!(history.printed_any());
        assert!(!history.has_unprinted());
        assert_eq!(drain(&mut history, &theme), "");
        history.push(Recap::new("Done."));
        assert_eq!(
            drain(&mut history, &theme),
            "\n※ recap: Done. (disable recaps in /config)"
        );
        assert_eq!(history.cells().len(), 3);
    }

    #[derive(Debug)]
    struct Hanging;

    impl HistoryCell for Hanging {
        fn lines(&self, _: &RenderCtx<'_>) -> Vec<Line<'static>> {
            vec![Line::from("  ⎿  Interrupted")]
        }

        fn continues_previous(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_cell_can_hang_under_the_item_before_it() {
        let theme = theme();
        let mut history = History::new();
        history.push(UserMessage::new("hi"));
        let id = history.start_stream();
        history.push_stream(id, "Half an answer");
        history.push(Cell::Custom(Box::new(Hanging)));
        assert_eq!(drain(&mut history, &theme), "❯ hi");
        history.finish_stream(id);
        assert_eq!(
            drain(&mut history, &theme),
            "\n● Half an answer\n  ⎿  Interrupted"
        );
        history.push(Recap::new("Done."));
        assert!(drain(&mut history, &theme).starts_with("\n※ recap"));
    }

    #[test]
    fn a_streamed_answer_prints_as_it_goes_and_continues_without_gaps() {
        let theme = theme();
        let mut history = History::new();
        history.push(UserMessage::new("hi"));
        let id = history.start_stream();
        assert!(history.is_streaming());
        assert!(history.push_stream(id, "First line.\nSecond "));
        // The newest complete line waits, so only the user message prints.
        assert_eq!(drain(&mut history, &theme), "❯ hi");
        history.push_stream(id, "line.\nThird");
        assert!(history.has_unprinted());
        assert_eq!(drain(&mut history, &theme), "\n● First line.");
        history.push_stream(id, " line.\n");
        assert_eq!(drain(&mut history, &theme), "  Second line.");
        assert!(history.finish_stream(id));
        assert!(!history.is_streaming());
        assert_eq!(drain(&mut history, &theme), "  Third line.");
        assert!(matches!(
            history.cells().last(),
            Some(Cell::Assistant(message)) if message.markdown == "First line.\nSecond line.\nThird line.\n"
        ));
        assert!(!history.push_stream(id, "late"));
        assert!(!history.finish_stream(id));
    }

    #[test]
    fn the_finished_reply_completes_or_replaces_the_preview() {
        let theme = theme();
        let mut history = History::new();
        let id = history.start_stream();
        history.push_stream(id, "Line one.\nLine ");
        assert!(history.finish_stream_with(id, "Line one.\nLine two.\n"));
        assert_eq!(drain(&mut history, &theme), "● Line one.\n  Line two.");

        // Nothing printed yet, so a different reply takes the preview's place.
        let id = history.start_stream();
        history.push_stream(id, "Let me check the");
        history.finish_stream_with(id, "The answer is **42**.");
        assert_eq!(drain(&mut history, &theme), "\n● The answer is 42.");
        assert!(matches!(
            history.cells().last(),
            Some(Cell::Assistant(message)) if message.markdown == "The answer is **42**."
        ));

        // Printed rows stay, and the overlay keeps the reply.
        let id = history.start_stream();
        history.push_stream(id, "Draft one.\nDraft two.\n");
        assert_eq!(drain(&mut history, &theme), "\n● Draft one.");
        history.finish_stream_with(id, "Final.");
        assert_eq!(drain(&mut history, &theme), "  Draft two.");
        assert!(matches!(
            history.cells().last(),
            Some(Cell::Assistant(message)) if message.markdown == "Final."
        ));
        assert!(!history.finish_stream_with(id, "late"));
    }

    #[test]
    fn kept_cells_never_print() {
        let theme = theme();
        let mut history = History::new();
        history.push(UserMessage::new("hi"));
        history.keep(Recap::new("Replayed."));
        history.push(AssistantMessage::new("Hello."));
        assert_eq!(drain(&mut history, &theme), "❯ hi\n\n● Hello.");
        assert_eq!(history.cells().len(), 3);
        assert!(matches!(history.cells()[1], Cell::Recap(_)));
    }

    #[test]
    fn cells_committed_during_a_stream_wait_for_it() {
        let theme = theme();
        let mut history = History::new();
        let id = history.start_stream();
        history.push_stream(id, "Answer.\n");
        history.push(Recap::new("After."));
        assert_eq!(drain(&mut history, &theme), "");
        assert_eq!(history.stream_source(), Some("Answer.\n"));
        assert!(matches!(history.held_cells(), [Cell::Recap(_)]));
        history.finish_stream(id);
        assert_eq!(history.stream_source(), None);
        assert!(history.held_cells().is_empty());
        assert_eq!(
            drain(&mut history, &theme),
            "● Answer.\n\n※ recap: After. (disable recaps in /config)"
        );
        assert!(matches!(
            history.cells(),
            [Cell::Assistant(_), Cell::Recap(_)]
        ));
    }

    #[test]
    fn a_new_stream_finishes_the_last_one() {
        let theme = theme();
        let mut history = History::new();
        let first = history.start_stream();
        history.push_stream(first, "One.");
        let second = history.start_stream();
        assert_ne!(first, second);
        history.push_stream(second, "Two.");
        history.finish_stream(second);
        assert_eq!(drain(&mut history, &theme), "● One.\n\n● Two.");
    }

    #[test]
    fn an_empty_stream_leaves_nothing_behind() {
        let theme = theme();
        let mut history = History::new();
        let id = history.start_stream();
        history.finish_stream(id);
        assert_eq!(drain(&mut history, &theme), "");
        assert!(history.cells().is_empty());
        assert!(!history.printed_any());
    }

    #[test]
    fn live_cells_print_whole_when_they_finish() {
        let theme = theme();
        let mut history = History::new();
        let id = history.push_active(ToolCall::call(ToolKind::Shell, "Bash", "ls"));
        assert!(history.is_animating());
        assert_eq!(drain(&mut history, &theme), "");
        let call = history
            .active_mut(id)
            .and_then(Cell::as_tool_mut)
            .expect("tool");
        call.push(ToolItem::text("a.rs"));
        call.finish(true);
        assert!(!history.is_animating());
        assert_eq!(history.active_cells().count(), 1);
        assert!(history.finish_active(id));
        assert!(!history.finish_active(id));
        assert!(history.active(id).is_none());
        assert_eq!(drain(&mut history, &theme), "● Bash(ls)\n  ⎿  a.rs");
        let gone = history.push_active(ToolCall::call(ToolKind::Shell, "Bash", "pwd"));
        assert!(history.remove_active(gone).is_some());
        assert_eq!(drain(&mut history, &theme), "");
    }

    #[test]
    fn live_blocks_know_when_a_gap_goes_above_them() {
        let theme = theme();
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let mut history = History::new();
        let first = history.push_active(ToolCall::call(ToolKind::Shell, "Bash", "a"));
        let blocks = history.live_blocks(&ctx);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].id, Some(first));
        assert!(!blocks[0].gap_before, "nothing printed and nothing above");

        let second = history.push_active(ToolCall::call(ToolKind::Shell, "Bash", "b"));
        let blocks = history.live_blocks(&ctx);
        assert_eq!(blocks[1].id, Some(second));
        assert!(blocks[1].gap_before);

        let id = history.start_stream();
        history.push_stream(id, "Streaming");
        let blocks = history.live_blocks(&ctx);
        assert_eq!(blocks[0].id, None);
        assert!(!blocks[0].gap_before);
        assert!(
            blocks[1].gap_before,
            "the first live cell sits under the tail"
        );
        assert_eq!(plain_text(&blocks[0].lines), "● Streaming");

        history.push_stream(id, " text.\nMore.\nAnd");
        history.push(UserMessage::new("queued behind the stream"));
        drain(&mut history, &theme);
        let blocks = history.live_blocks(&ctx);
        assert!(!blocks[0].gap_before, "the tail continues the printed rows");
        assert_eq!(plain_text(&blocks[0].lines), "  More.\n  And");
        assert!(blocks[1].gap_before);
    }

    #[test]
    fn printed_history_matches_the_screenshot_order() {
        let theme = theme();
        let mut history = History::new();
        history.push(UserMessage::new("Add a --json flag to the export command"));
        history.push(AssistantMessage::new(
            "That's a code change, so I'll run it with the crew.",
        ));
        history.push(
            ToolCall::call(ToolKind::Other, "Goal", "Add a --json flag to export")
                .with_state(ToolState::Done)
                .with_item(ToolItem::text("Agreed by 3 agents · 1 question answered")),
        );
        let stream = history.start_stream();
        history.push_stream(stream, "Merged task-002.\n\n- 3 files\n- tests pass\n");
        history.finish_stream(stream);
        insta::assert_snapshot!(drain(&mut history, &theme));
    }

    #[test]
    fn an_answer_stopped_between_blocks_ends_without_a_gap() {
        let theme = theme();
        let mut history = History::new();
        let id = history.start_stream();
        history.push_stream(id, "To try it:\n\n```bash\n");
        assert_eq!(drain(&mut history, &theme), "● To try it:");
        history.finish_stream(id);
        assert_eq!(drain(&mut history, &theme), "");

        // Fed word by word, the blank row before the code block arrives alone and waits for
        // the rows after it.
        let mut history = History::new();
        let id = history.start_stream();
        let mut printed = Vec::new();
        for piece in ["To try it:\n", "\n", "```bash\n", "mkdir", " x\n", "cd y\n"] {
            history.push_stream(id, piece);
            let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
            printed.push(history.drain_printable(&ctx).len());
        }
        assert_eq!(printed, [0, 1, 0, 0, 0, 3]);
    }
}
