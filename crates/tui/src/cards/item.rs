//! The conversation item a card's long content goes into: a bold heading after a bullet, then
//! lines, indented lists, and tables under it.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::table::Table;
use crate::cell::HistoryCell;
use crate::render::RenderCtx;
use crate::text::{fit_lines, sanitize, sanitize_line, str_width, wrap_spans};
use crate::theme::Role;

/// Where the body starts, under the heading's text.
const BODY: usize = 2;

/// Each level of an indented list.
const STEP: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct AboutCell {
    pub heading: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Wrapped with a hanging indent, `depth` steps in.
    Line {
        text: String,
        role: Role,
        depth: usize,
    },
    /// A dim label, then the text: `Tests  the export writes one row per line`.
    Labeled {
        label: String,
        text: String,
    },
    Table(Table),
    Blank,
}

impl AboutCell {
    pub fn new(heading: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            blocks: Vec::new(),
        }
    }

    pub fn line(&mut self, text: impl Into<String>) {
        self.push(text, Role::Text, 0);
    }

    pub fn dim(&mut self, text: impl Into<String>) {
        self.push(text, Role::Dim, 0);
    }

    pub fn push(&mut self, text: impl Into<String>, role: Role, depth: usize) {
        self.blocks.push(Block::Line {
            text: text.into(),
            role,
            depth,
        });
    }

    pub fn labeled(&mut self, label: impl Into<String>, text: impl Into<String>) {
        self.blocks.push(Block::Labeled {
            label: label.into(),
            text: text.into(),
        });
    }

    pub fn table(&mut self, table: Table) {
        if !table.rows.is_empty() {
            self.blocks.push(Block::Table(table));
        }
    }

    /// A blank row, never two in a row or one first.
    pub fn blank(&mut self) {
        if !matches!(self.blocks.last(), None | Some(Block::Blank)) {
            self.blocks.push(Block::Blank);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Unwrapped rows for the pager, tables laid out `width` cells wide.
    pub fn plain(&self, width: usize) -> Vec<String> {
        let mut rows = vec![
            sanitize_line(&self.heading).trim().to_owned(),
            String::new(),
        ];
        for block in &self.blocks {
            match block {
                Block::Line { text, depth, .. } => {
                    let indent = " ".repeat(depth * STEP);
                    rows.extend(
                        sanitize(text)
                            .lines()
                            .map(|line| format!("{indent}{}", line.trim_end())),
                    );
                }
                Block::Labeled { label, text } => rows.push(format!(
                    "{}  {}",
                    sanitize_line(label).trim(),
                    sanitize_line(text).trim()
                )),
                Block::Table(table) => rows.extend(table.plain(width, 0)),
                Block::Blank => rows.push(String::new()),
            }
        }
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows
    }
}

impl HistoryCell for AboutCell {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let bullet = theme.glyphs.bullet;
        let hang = str_width(bullet) + 1;
        let first = [
            Span::styled(bullet.to_owned(), theme.style(Role::Text)),
            Span::raw(" "),
        ];
        let rest = [Span::raw(" ".repeat(hang))];
        let heading = Span::styled(
            sanitize_line(&self.heading).trim().to_owned(),
            theme.style(Role::Text).add_modifier(Modifier::BOLD),
        );
        let mut lines = wrap_spans(&[heading], width, &first, &rest);
        let mut blocks = self.blocks.as_slice();
        while let Some(Block::Blank) = blocks.last() {
            blocks = &blocks[..blocks.len() - 1];
        }
        for block in blocks {
            match block {
                Block::Line { text, role, depth } => {
                    let lead = [Span::raw(" ".repeat(BODY + depth * STEP))];
                    let hang = [Span::raw(" ".repeat(BODY + depth * STEP + STEP))];
                    let spans = [Span::styled(
                        sanitize(text).into_owned(),
                        theme.style(*role),
                    )];
                    lines.extend(wrap_spans(&spans, width, &lead, &hang));
                }
                Block::Labeled { label, text } => {
                    let label = sanitize_line(label).trim().to_owned();
                    let lead = [
                        Span::raw(" ".repeat(BODY)),
                        Span::styled(format!("{label}  "), theme.style(Role::Dim)),
                    ];
                    let hang = [Span::raw(" ".repeat(BODY + STEP))];
                    let spans = [Span::styled(
                        sanitize(text).into_owned(),
                        theme.style(Role::Text),
                    )];
                    lines.extend(wrap_spans(&spans, width, &lead, &hang));
                }
                Block::Table(table) => lines.extend(table.lines(theme, width, BODY)),
                Block::Blank => lines.push(Line::default()),
            }
        }
        fit_lines(&mut lines, width);
        lines
    }
}
