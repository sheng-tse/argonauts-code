mod parse;
mod render;
mod syntax;
mod words;

use std::fmt;
use std::time::Duration;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use super::HistoryCell;
use super::layout::{BODY_COL, result_prefix};
use crate::base::draw::render::RenderCtx;
use crate::base::text::marks;
use crate::base::text::{fit_lines, sanitize_line, wrap_spans};
use crate::base::theme::{Role, Theme};

/// Past this, [`DiffView::between`] settles for a correct but less tidy diff rather than
/// stall the screen on huge or very different texts.
const DIFF_TIME_LIMIT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffView {
    pub hunks: Vec<DiffHunk>,
    /// Says what the change was in place of the plain `Added 1 line` summary.
    pub caption: Option<DiffCaption>,
}

/// The counts are the whole change's, which a preview cut to its first rows can't count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffCaption {
    /// `Added 2 lines, removed 1 line`, or `Updated src/app.rs` when neither count is known.
    Updated {
        path: String,
        added: usize,
        removed: usize,
    },
    /// `Wrote 3 lines to hello.txt`
    Wrote { path: String, lines: usize },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffHunk {
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
}

impl DiffLine {
    /// The line number shown in the gutter.
    pub fn number(&self) -> Option<u32> {
        match self.kind {
            DiffKind::Removed => self.old_line,
            DiffKind::Added | DiffKind::Context => self.new_line,
        }
    }
}

impl DiffView {
    /// Reads a unified diff such as `git diff` output, skipping file headers.
    pub fn from_unified(diff: &str) -> Result<Self, DiffParseError> {
        parse::unified(diff)
    }

    /// With `context` unchanged lines around each change.
    pub fn between(old: &str, new: &str, context: usize) -> Self {
        let diff = TextDiff::configure()
            .timeout(DIFF_TIME_LIMIT)
            .diff_lines(old, new);
        let number = |index: Option<usize>| index.map(|i| u32::try_from(i + 1).unwrap_or(u32::MAX));
        let hunks = diff
            .grouped_ops(context)
            .iter()
            .map(|group| DiffHunk {
                lines: group
                    .iter()
                    .flat_map(|op| diff.iter_changes(op))
                    .map(|change| {
                        let kind = match change.tag() {
                            ChangeTag::Equal => DiffKind::Context,
                            ChangeTag::Delete => DiffKind::Removed,
                            ChangeTag::Insert => DiffKind::Added,
                        };
                        let (old_line, new_line) = match kind {
                            DiffKind::Context => {
                                (number(change.old_index()), number(change.new_index()))
                            }
                            DiffKind::Removed => (number(change.old_index()), None),
                            DiffKind::Added => (None, number(change.new_index())),
                        };
                        DiffLine {
                            kind,
                            old_line,
                            new_line,
                            text: change.value().trim_end_matches(['\n', '\r']).to_owned(),
                        }
                    })
                    .collect(),
            })
            .collect();
        Self {
            hunks,
            caption: None,
        }
    }

    /// Captioned `Added 2 lines, removed 1 line` with the whole change's counts.
    pub fn updated(mut self, path: impl Into<String>, added: usize, removed: usize) -> Self {
        self.caption = Some(DiffCaption::Updated {
            path: path.into(),
            added,
            removed,
        });
        self
    }

    /// Captioned `Wrote 3 lines to hello.txt`.
    pub fn wrote(mut self, path: impl Into<String>, lines: usize) -> Self {
        self.caption = Some(DiffCaption::Wrote {
            path: path.into(),
            lines,
        });
        self
    }

    /// For a diff of snippets, whose line numbers would be counted from the snippet and not
    /// the file.
    pub fn without_numbers(mut self) -> Self {
        for line in self.hunks.iter_mut().flat_map(|hunk| &mut hunk.lines) {
            line.old_line = None;
            line.new_line = None;
        }
        self
    }

    /// The file the caption names, whose name gives the rows their syntax colours.
    pub fn file(&self) -> Option<&str> {
        match self.caption.as_ref()? {
            DiffCaption::Updated { path, .. } | DiffCaption::Wrote { path, .. } => {
                Some(path.as_str()).filter(|path| !path.is_empty())
            }
        }
    }

    /// `(added, removed)` line counts.
    pub fn stats(&self) -> (usize, usize) {
        let rows = self.hunks.iter().flat_map(|hunk| &hunk.lines);
        rows.fold((0, 0), |(added, removed), line| match line.kind {
            DiffKind::Added => (added + 1, removed),
            DiffKind::Removed => (added, removed + 1),
            DiffKind::Context => (added, removed),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.hunks.iter().all(|hunk| hunk.lines.is_empty())
    }

    /// `Added 1 line, removed 1 line` or `(No changes)`, shown above the rows, or the caption.
    pub fn summary(&self, theme: &Theme) -> Vec<Span<'static>> {
        if let Some(caption) = &self.caption {
            return caption.spans(theme);
        }
        let (added, removed) = self.stats();
        let spans = change_spans(added, removed, theme);
        if spans.is_empty() {
            return vec![Span::styled("(No changes)", theme.style(Role::Dim))];
        }
        spans
    }

    /// Fills stop a few cells short of the right edge. Long rows wrap under the text, except in
    /// a live preview (`ctx.live`), which cuts them with `…`.
    pub fn render(&self, ctx: &RenderCtx<'_>, indent: u16) -> Vec<Line<'static>> {
        self.render_with(ctx, indent, render::Fill::ShortOfEdge)
    }

    /// For a diff inside a question or a full-screen view.
    pub fn render_to_edge(&self, ctx: &RenderCtx<'_>, indent: u16) -> Vec<Line<'static>> {
        self.render_with(ctx, indent, render::Fill::ToEdge)
    }

    fn render_with(
        &self,
        ctx: &RenderCtx<'_>,
        indent: u16,
        fill: render::Fill,
    ) -> Vec<Line<'static>> {
        let mut lines = render::rows(self, ctx, usize::from(indent), fill);
        fit_lines(&mut lines, usize::from(ctx.width));
        lines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffParseError {
    /// One-based.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for DiffParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "diff line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for DiffParseError {}

impl DiffCaption {
    fn spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        let dim = theme.style(Role::Dim);
        match self {
            DiffCaption::Updated {
                path,
                added,
                removed,
            } => {
                let spans = change_spans(*added, *removed, theme);
                if spans.is_empty() {
                    return vec![Span::styled(
                        format!("Updated {}", sanitize_line(path)),
                        dim,
                    )];
                }
                spans
            }
            DiffCaption::Wrote { path, lines } => {
                let unit = if *lines == 1 { " line" } else { " lines" };
                vec![
                    Span::styled("Wrote ", dim),
                    Span::styled(lines.to_string(), dim.add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{unit} to {}", sanitize_line(path)), dim),
                ]
            }
        }
    }
}

/// `Added 2 lines, removed 1 line` with bold counts, as Claude Code sums up an edit. Empty when
/// nothing changed.
fn change_spans(added: usize, removed: usize, theme: &Theme) -> Vec<Span<'static>> {
    let dim = theme.style(Role::Dim);
    let bold = dim.add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    for (leading, following, n) in [
        ("Added ", "added ", added),
        ("Removed ", "removed ", removed),
    ] {
        if n == 0 {
            continue;
        }
        let verb = if spans.is_empty() {
            leading
        } else {
            spans.push(Span::styled(", ", dim));
            following
        };
        spans.push(Span::styled(verb, dim));
        spans.push(Span::styled(n.to_string(), bold));
        spans.push(Span::styled(if n == 1 { " line" } else { " lines" }, dim));
    }
    spans
}

/// A diff on its own in the transcript, not under a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffCell {
    pub title: String,
    pub diff: DiffView,
}

impl HistoryCell for DiffCell {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let first = [
            marks::decorative(theme.glyphs.bullet, theme.style(Role::Success)),
            Span::raw(" "),
        ];
        let title = [Span::styled(
            sanitize_line(&self.title).into_owned(),
            theme.style(Role::Text),
        )];
        let mut lines = wrap_spans(&title, width, &first, &[Span::raw("  ")]);
        let mut head = result_prefix(theme);
        head.extend(self.diff.summary(theme));
        lines.push(Line::from(head));
        lines.extend(self.diff.render(ctx, BODY_COL as u16));
        fit_lines(&mut lines, width);
        lines
    }
}

#[cfg(test)]
mod tests;
