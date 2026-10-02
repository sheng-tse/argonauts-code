//! Adapted from the `Writer` in Codex's `markdown_render.rs` (Apache-2.0): pulldown-cmark events
//! drive a stack of containers (quotes, lists, items) whose prefixes every row sits behind, with
//! inline styles on a stack of their own. Unlike Codex, blank rows follow the source's blank
//! lines plus one after every heading, headings drop their `#`, and code blocks sit at the text
//! column without fences, all as Claude Code prints Markdown.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::highlight::{LineHighlighter, highlight_code, split_lines};
use super::list::Numbering;
use super::table::TableBuilder;
use super::{Continuation, Indent, code_rows, pad};
use crate::render::RenderCtx;
use crate::text::{spans_width, str_width, wrap_spans};
use crate::theme::{Role, Theme};

/// Renders already sanitized Markdown.
pub(super) fn render(
    src: &str,
    ctx: &RenderCtx<'_>,
    indent: &Indent,
    cont: Continuation,
) -> Vec<Line<'static>> {
    render_traced(src, ctx, indent, cont).0
}

/// Renders already sanitized Markdown and reports where its rows and blocks come from.
pub(super) fn render_traced(
    src: &str,
    ctx: &RenderCtx<'_>,
    indent: &Indent,
    cont: Continuation,
) -> (Vec<Line<'static>>, Trace) {
    let mut writer = Writer::new(src, ctx, indent, cont);
    for (event, range) in Parser::new_ext(src, options()).into_offset_iter() {
        writer.event(event, range);
    }
    writer.finish()
}

/// Where rendered rows come from, so a stream knows which rows are final and where it can start
/// rendering again from scratch. Offsets are bytes into the rendered source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Trace {
    pub rows: Vec<usize>,
    /// Where top-level blocks start, and items of top-level bullet lists. Rendering from any of
    /// these on its own gives the same rows as rendering everything.
    pub starts: Vec<usize>,
    /// The starts that come right after a heading, so a blank row goes before them.
    pub after_heading: Vec<usize>,
    /// The start of the last top-level block when it holds a table, which can still reshape.
    pub table_hold: Option<usize>,
    /// Top-level fenced code blocks that open at column 0.
    pub fences: Vec<FenceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FenceSpan {
    /// The fence character, a backtick or a tilde, and how many open the block.
    pub marker: char,
    pub len: usize,
    /// Where the first content line starts.
    pub content: usize,
    /// Where the block ends: after the closing fence, or at the end of the source.
    pub end: usize,
    pub info: String,
}

fn options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options
}

pub(super) fn quote_bar(theme: &Theme) -> [Span<'static>; 2] {
    [
        Span::styled(theme.glyphs.quote, theme.style(Role::Dim)),
        Span::raw(" "),
    ]
}

/// A container the rows are inside of.
#[derive(Debug)]
enum Frame {
    Quote {
        blocks: usize,
    },
    List {
        /// The next item's number in a numbered list.
        next: Option<u64>,
        numbering: Numbering,
        blocks: usize,
    },
    Item {
        marker: Vec<Span<'static>>,
        /// Columns the item's text hangs at, past the marker and any task box.
        width: usize,
        /// The marker hasn't been drawn yet.
        pending: bool,
        blocks: usize,
        start: usize,
    },
}

impl Frame {
    fn blocks_mut(&mut self) -> &mut usize {
        match self {
            Frame::Quote { blocks } | Frame::List { blocks, .. } | Frame::Item { blocks, .. } => {
                blocks
            }
        }
    }
}

#[derive(Debug)]
struct Link {
    url: String,
    /// The visible label, to tell whether the url adds anything.
    label: String,
}

#[derive(Debug)]
struct CodeBlock {
    lang: Option<String>,
    text: String,
    start: usize,
    /// Where each code line starts in the source.
    line_starts: Vec<usize>,
}

impl CodeBlock {
    fn push(&mut self, text: &str, at: usize) {
        let mut offset = at;
        for piece in text.split_inclusive('\n') {
            if self.text.is_empty() || self.text.ends_with('\n') {
                self.line_starts.push(offset);
            }
            self.text.push_str(piece);
            offset += piece.len();
        }
    }

    fn line_start(&self, index: usize) -> usize {
        self.line_starts.get(index).copied().unwrap_or(self.start)
    }
}

struct Writer<'a, 'c> {
    src: &'a str,
    ctx: &'a RenderCtx<'c>,
    indent: &'a Indent,
    cont: Continuation,
    lines: Vec<Line<'static>>,
    trace: Trace,
    /// Where the event being handled starts.
    at: usize,
    /// Where the source line being built starts.
    line_at: usize,
    /// The current top-level block's start, and whether it holds a table.
    top_start: usize,
    top_table: bool,
    frames: Vec<Frame>,
    root_blocks: usize,
    /// A heading just ended, so the next block gets a blank row before it.
    after_heading: bool,
    /// Inline styles, innermost last. The bottom one is the text style.
    styles: Vec<Style>,
    line: Vec<Span<'static>>,
    line_open: bool,
    links: Vec<Link>,
    code: Option<CodeBlock>,
    table: Option<TableBuilder>,
}

impl<'a, 'c> Writer<'a, 'c> {
    fn new(src: &'a str, ctx: &'a RenderCtx<'c>, indent: &'a Indent, cont: Continuation) -> Self {
        Self {
            src,
            ctx,
            indent,
            cont,
            lines: Vec::new(),
            trace: Trace::default(),
            at: 0,
            line_at: 0,
            top_start: 0,
            top_table: false,
            frames: Vec::new(),
            root_blocks: 0,
            after_heading: cont.after_heading,
            styles: vec![ctx.theme.style(Role::Text)],
            line: Vec::new(),
            line_open: false,
            links: Vec::new(),
            code: None,
            table: None,
        }
    }

    fn width(&self) -> usize {
        usize::from(self.ctx.width)
    }

    fn theme(&self) -> &'c Theme {
        self.ctx.theme
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, patch: Style) {
        let style = self.style().patch(patch);
        self.styles.push(style);
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    /// Adds rows that all come from `at` in the source.
    fn push_rows(&mut self, rows: impl IntoIterator<Item = Line<'static>>, at: usize) {
        for row in rows {
            self.lines.push(row);
            self.trace.rows.push(at);
        }
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        self.at = range.start;
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => {
                self.label(&code);
                let style = self.style().patch(self.theme().style(Role::Code));
                self.push_span(Span::styled(code.into_string(), style));
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => self.text(&math),
            Event::Html(html) => self.html_line(&html),
            Event::InlineHtml(html) => self.text(&html),
            Event::SoftBreak | Event::HardBreak => self.line_break(),
            Event::Rule => self.rule(range),
            Event::TaskListMarker(done) => self.task_marker(done),
            Event::FootnoteReference(name) => self.text(&format!("[^{name}]")),
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.begin_block(range.start),
            // Every heading is bold, italic, and underlined, as Claude Code draws them.
            Tag::Heading { .. } => {
                self.begin_block(range.start);
                self.push_style(
                    Style::new()
                        .add_modifier(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED),
                );
            }
            Tag::BlockQuote(_) => {
                self.begin_block(range.start);
                self.frames.push(Frame::Quote { blocks: 0 });
                self.push_style(Style::new().add_modifier(Modifier::ITALIC));
            }
            Tag::CodeBlock(kind) => {
                self.begin_block(range.start);
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => Some(info.into_string()),
                    CodeBlockKind::Indented => None,
                };
                if let Some(info) = &lang {
                    self.note_fence(info, range.clone());
                }
                self.code = Some(CodeBlock {
                    lang,
                    text: String::new(),
                    start: range.start,
                    line_starts: Vec::new(),
                });
            }
            Tag::List(start) => {
                self.begin_block(range.start);
                let numbering = Numbering::for_list(self.item_depth(), start.unwrap_or(1));
                self.frames.push(Frame::List {
                    next: start,
                    numbering,
                    blocks: 0,
                });
            }
            Tag::Item => self.start_item(range.start),
            Tag::Table(aligns) => {
                self.begin_block(range.start);
                self.top_table = true;
                self.table = Some(TableBuilder::new(aligns, range));
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.start_row();
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.start_cell();
                }
            }
            Tag::Emphasis => self.push_style(Style::new().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::new().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(Style::new().add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Superscript | Tag::Subscript => self.push_style(Style::new()),
            Tag::Link { dest_url, .. } => {
                self.push_style(Style::new().add_modifier(Modifier::UNDERLINED));
                self.links.push(Link {
                    url: dest_url.into_string(),
                    label: String::new(),
                });
            }
            Tag::Image { dest_url, .. } => {
                self.push_style(Style::new());
                self.links.push(Link {
                    url: dest_url.into_string(),
                    label: String::new(),
                });
            }
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => self.begin_block(range.start),
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush_line();
                self.pop_style();
                self.after_heading = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.frames.pop();
                self.pop_style();
            }
            TagEnd::CodeBlock => self.end_code(),
            TagEnd::List(_) => {
                self.flush_line();
                self.frames.pop();
            }
            TagEnd::Item => self.end_item(),
            TagEnd::Table => self.end_table(),
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.end_cell();
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.end_row();
                }
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_style(),
            TagEnd::Link | TagEnd::Image => {
                self.pop_style();
                if let Some(link) = self.links.pop() {
                    self.link_target(link);
                }
            }
            TagEnd::Paragraph
            | TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => self.flush_line(),
        }
    }

    fn finish(mut self) -> (Vec<Line<'static>>, Trace) {
        self.flush_line();
        if self.code.is_some() {
            self.end_code();
        }
        if self.table.is_some() {
            self.end_table();
        }
        if self.top_table {
            self.trace.table_hold = Some(self.top_start);
        }
        (self.lines, self.trace)
    }

    /// Remembers a fenced block that opens at column 0 outside any container, which a stream
    /// can follow line by line.
    fn note_fence(&mut self, info: &str, range: Range<usize>) {
        let at_line_start = range.start == 0 || self.src[..range.start].ends_with('\n');
        let opener = &self.src[range.start..];
        let Some(marker) = opener.chars().next().filter(|c| matches!(c, '`' | '~')) else {
            return;
        };
        if !self.frames.is_empty() || !at_line_start {
            return;
        }
        let len = opener.chars().take_while(|c| *c == marker).count();
        let content = opener
            .find('\n')
            .map_or(self.src.len(), |i| range.start + i + 1);
        self.trace.fences.push(FenceSpan {
            marker,
            len,
            content,
            end: range.end,
            info: info.to_owned(),
        });
    }

    /// Starts a block in the innermost container, with a blank row first when the source has a
    /// blank line before it or a heading ends right before it, and it isn't the container's first
    /// block.
    fn begin_block(&mut self, start: usize) {
        self.flush_line();
        let after_heading = std::mem::take(&mut self.after_heading);
        self.note_start(start, after_heading);
        let blank = after_heading || self.blank_line_before(start);
        let first = match self.frames.last_mut() {
            Some(frame) => {
                let blocks = frame.blocks_mut();
                *blocks += 1;
                *blocks == 1
            }
            None => {
                self.root_blocks += 1;
                self.root_blocks == 1 && !self.cont.rows_before
            }
        };
        if !first && blank {
            self.blank_row(start);
        }
    }

    fn note_start(&mut self, start: usize, after_heading: bool) {
        let top_item = matches!(self.frames.as_slice(), [Frame::List { next: None, .. }]);
        if self.frames.is_empty() {
            self.top_start = start;
            self.top_table = false;
        }
        if self.frames.is_empty() || top_item {
            self.trace.starts.push(start);
            if after_heading {
                self.trace.after_heading.push(start);
            }
        }
    }

    /// Whether the source line before the one holding `start` is blank. A line of only quote
    /// markers counts as blank, since it separates blocks inside a quote.
    fn blank_line_before(&self, start: usize) -> bool {
        let start = start.min(self.src.len());
        let Some(line_start) = self.src[..start].rfind('\n').map(|i| i + 1) else {
            return self.cont.blank_before;
        };
        let prev_start = self.src[..line_start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.src[prev_start..line_start - 1]
            .chars()
            .all(|c| c.is_whitespace() || c == '>')
    }

    /// How many list items the next block sits inside, counting back to the nearest quote, which
    /// starts its lists afresh.
    fn item_depth(&self) -> usize {
        self.frames
            .iter()
            .rev()
            .take_while(|frame| !matches!(frame, Frame::Quote { .. }))
            .filter(|frame| matches!(frame, Frame::Item { .. }))
            .count()
    }

    fn start_item(&mut self, start: usize) {
        self.begin_block(start);
        let label = match self.frames.last_mut() {
            Some(Frame::List {
                next: Some(number),
                numbering,
                ..
            }) => {
                let label = numbering.marker(*number);
                *number += 1;
                label
            }
            _ => "- ".to_owned(),
        };
        let width = str_width(&label);
        let marker = vec![Span::styled(label, self.theme().style(Role::Text))];
        self.frames.push(Frame::Item {
            marker,
            width,
            pending: true,
            blocks: 0,
            start,
        });
    }

    fn end_item(&mut self) {
        self.flush_line();
        // An empty item still shows its marker.
        if let Some(&Frame::Item {
            pending: true,
            start,
            ..
        }) = self.frames.last()
        {
            let (first, _) = self.prefixes();
            self.push_rows([Line::from(first)], start);
        }
        self.frames.pop();
    }

    /// The prefix for the next row and for the rows that wrap under it. Taking it draws any list
    /// markers still waiting, so each marker shows once.
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let lead = if self.lines.is_empty() && !self.cont.rows_before {
            &self.indent.first
        } else {
            &self.indent.rest
        };
        let mut first = lead.clone();
        let mut rest = self.indent.rest.clone();
        let bar = quote_bar(self.ctx.theme);
        for frame in &mut self.frames {
            match frame {
                Frame::Quote { .. } => {
                    first.extend(bar.clone());
                    rest.extend(bar.clone());
                }
                Frame::List { .. } => {}
                Frame::Item {
                    marker,
                    width,
                    pending,
                    ..
                } => {
                    if *pending {
                        first.extend(marker.iter().cloned());
                        *pending = false;
                    } else {
                        first.push(pad(*width));
                    }
                    rest.push(pad(*width));
                }
            }
        }
        (first, rest)
    }

    /// A blank row inside the current containers: quote bars stay, trailing spaces go.
    fn blank_row(&mut self, at: usize) {
        let mut spans = self.indent.rest.clone();
        let bar = quote_bar(self.ctx.theme);
        for frame in &self.frames {
            match frame {
                Frame::Quote { .. } => spans.extend(bar.clone()),
                Frame::Item { width, .. } => spans.push(pad(*width)),
                Frame::List { .. } => {}
            }
        }
        while spans
            .last()
            .is_some_and(|span| span.content.trim().is_empty() && span.style == Style::new())
        {
            spans.pop();
        }
        self.push_rows([Line::from(spans)], at);
    }

    fn text(&mut self, text: &str) {
        if let Some(code) = self.code.as_mut() {
            code.push(text, self.at);
            return;
        }
        self.label(text);
        let style = self.style();
        self.push_span(Span::styled(text.to_owned(), style));
    }

    fn label(&mut self, text: &str) {
        if let Some(link) = self.links.last_mut() {
            link.label.push_str(text);
        }
    }

    fn push_span(&mut self, span: Span<'static>) {
        if let Some(table) = self.table.as_mut() {
            table.push(span);
            return;
        }
        if !self.line_open {
            self.line_open = true;
            self.line_at = self.at;
            // Text right inside a tight list item is the item's first block.
            if let Some(Frame::Item { blocks, .. }) = self.frames.last_mut()
                && *blocks == 0
            {
                *blocks = 1;
            }
        }
        self.line.push(span);
    }

    fn line_break(&mut self) {
        if self.table.is_some() {
            let style = self.style();
            self.push_span(Span::styled(" ", style));
            return;
        }
        self.flush_line();
    }

    fn flush_line(&mut self) {
        if !self.line_open {
            return;
        }
        self.line_open = false;
        let spans = std::mem::take(&mut self.line);
        let (first, rest) = self.prefixes();
        let rows = wrap_spans(&spans, self.width(), &first, &rest);
        self.push_rows(rows, self.line_at);
    }

    fn html_line(&mut self, html: &str) {
        let text = html.strip_suffix('\n').unwrap_or(html);
        if text.trim().is_empty() {
            self.blank_row(self.at);
            return;
        }
        let style = self.theme().style(Role::Text);
        self.push_span(Span::styled(text.to_owned(), style));
        self.flush_line();
    }

    fn rule(&mut self, range: Range<usize>) {
        self.begin_block(range.start);
        let (mut first, _) = self.prefixes();
        let cols = self.width().saturating_sub(spans_width(&first));
        let glyph = self.theme().glyphs.rule;
        let count = cols / str_width(glyph).max(1);
        first.push(Span::styled(
            glyph.repeat(count),
            self.theme().style(Role::Dim),
        ));
        self.push_rows([Line::from(first)], range.start);
    }

    fn task_marker(&mut self, done: bool) {
        let theme = self.theme();
        let (glyph, style) = if done {
            (theme.glyphs.todo_done, theme.style(Role::Success))
        } else {
            (theme.glyphs.todo_pending, theme.style(Role::Text))
        };
        let extra = str_width(glyph) + 1;
        self.push_span(Span::styled(glyph, style));
        self.push_span(Span::raw(" "));
        if let Some(Frame::Item { width, .. }) = self.frames.last_mut() {
            *width += extra;
        }
    }

    /// Adds ` (url)` after a link or image whose url says more than its text.
    fn link_target(&mut self, link: Link) {
        let url = link.url.trim();
        let label = link.label.trim();
        let redundant = url.is_empty()
            || url == label
            || url.strip_prefix("mailto:") == Some(label)
            || url.strip_suffix('/') == Some(label);
        if !redundant {
            let dim = self.theme().style(Role::Dim);
            self.push_span(Span::styled(format!(" ({url})"), dim));
        }
    }

    fn end_code(&mut self) {
        let Some(code) = self.code.take() else {
            return;
        };
        let theme = self.theme();
        let lines: Vec<Vec<Span<'static>>> = match code.lang.as_deref() {
            Some(info) if info.trim().is_empty() => {
                let mut bare = LineHighlighter::for_fence(info);
                split_lines(&code.text)
                    .map(|line| bare.line(line, theme))
                    .collect()
            }
            lang => highlight_code(&code.text, lang, theme),
        };
        let width = self.width();
        for (i, spans) in lines.into_iter().enumerate() {
            let (first, rest) = self.prefixes();
            let rows = code_rows(spans, width, &first, &rest);
            self.push_rows(rows, code.line_start(i));
        }
    }

    fn end_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let (first, rest) = self.prefixes();
        let at = table.source().start;
        let avail = self.width().saturating_sub(spans_width(&rest));
        match table.layout(avail, self.theme()) {
            Some(rows) => {
                for (i, row) in rows.into_iter().enumerate() {
                    let mut spans = if i == 0 { first.clone() } else { rest.clone() };
                    spans.extend(row);
                    self.push_rows([Line::from(spans)], at);
                }
            }
            None => {
                // Too wide to align: show the table as written, one source row per line.
                let style = self.theme().style(Role::Text);
                let mut prefix = Some((first, rest.clone()));
                for source_line in self.src[table.source()].lines() {
                    let text = source_line.trim_start_matches(['>', ' ']);
                    let (first, rest) = prefix
                        .take()
                        .unwrap_or_else(|| (rest.clone(), rest.clone()));
                    let spans = [Span::styled(text.to_owned(), style)];
                    let rows = wrap_spans(&spans, self.width(), &first, &rest);
                    self.push_rows(rows, at);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::{Indent, render_markdown};
    use crate::render::{Clock, RenderCtx};
    use crate::stability::samples;
    use crate::text::{plain_text, spans_width};
    use crate::theme::{Glyphs, Theme};

    /// A styled span before a word longer than the row keeps its row, at the widths the agent
    /// view's Outcome gets, and every character of the long word still shows.
    #[test]
    fn a_styled_span_before_an_overlong_word_is_kept() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let words = [
            ("z".repeat(150), 'z'),
            ("漢".repeat(70), '漢'),
            ("🦀".repeat(70), '🦀'),
            (samples::long_word(), 'x'),
        ];
        for (word, unit) in &words {
            let count = word.chars().filter(|c| c == unit).count();
            let items = [
                (format!("- `code` span {word}"), "- code span"),
                (format!("`code` span {word}"), "code span"),
                (format!("1. **bold** span {word}"), "1. bold span"),
            ];
            for (source, first) in &items {
                for width in 62..=84 {
                    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
                    let lines = render_markdown(source, &ctx, &Indent::spaces(2));
                    let text = plain_text(&lines);
                    // CJK breaks between any two characters, so its row goes on.
                    let row = text.lines().next().unwrap_or_default();
                    assert!(row.starts_with(&format!("  {first}")), "{text}");
                    assert_eq!(text.chars().filter(|c| c == unit).count(), count, "{text}");
                    for line in &lines {
                        assert!(spans_width(&line.spans) <= usize::from(width), "{text}");
                    }
                }
            }
        }
    }
}
