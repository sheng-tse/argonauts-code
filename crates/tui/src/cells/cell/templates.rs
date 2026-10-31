//! What `/templates` prints: the workflows and the prompts on a row each under the command,
//! then the folder that holds the reference copies and what each part of it is.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use argonauts_core::app::template_list::{TemplateEntry, TemplateList};

use super::HistoryCell;
use crate::base::draw::render::RenderCtx;
use crate::base::text::{fit_lines, sanitize_line, spans_width, str_width, wrap_text};
use crate::base::theme::Role;

/// The column the names start in, after `  workflows   `.
const NAMES_COL: usize = 14;

const COPIES: &str = "enter copies the picked one into .argonauts/ (this folder) or ~/.argonauts/ \
                      (every folder)";

/// `  workflows   Council (in use) · Orchestrator · Spec-first (yours)` and the prompts' row,
/// hung under the command that asked for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateRows {
    list: TemplateList,
}

impl TemplateRows {
    pub fn new(list: TemplateList) -> Self {
        Self { list }
    }
}

impl HistoryCell for TemplateRows {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let dim = ctx.theme.style(Role::Dim);
        let mut lines = row("workflows", &self.list.workflows, ctx);
        lines.extend(row("prompts", &self.list.prompts, ctx));
        lines.push(Line::from(Span::styled(format!("  {COPIES}"), dim)));
        fit_lines(&mut lines, usize::from(ctx.width));
        lines
    }

    fn continues_previous(&self) -> bool {
        true
    }
}

/// One entry as it reads in a row: its name, its marks, and the dot before the next one.
fn entry_spans(entry: &TemplateEntry, last: bool, ctx: &RenderCtx<'_>) -> Vec<Span<'static>> {
    let mut marks: Vec<(&str, Style)> = Vec::new();
    if entry.in_use {
        marks.push(("(in use)", ctx.theme.style(Role::Success)));
    }
    if let Some(changed) = entry.changed {
        marks.push((changed.mark(), ctx.theme.style(Role::Question)));
    }
    let mut spans = vec![Span::raw(sanitize_line(&entry.name).into_owned())];
    if !marks.is_empty() || !last {
        spans.push(Span::raw(" "));
    }
    let count = marks.len();
    for (at, (mark, style)) in marks.into_iter().enumerate() {
        let ends_the_row = last && at + 1 == count;
        let text = if ends_the_row {
            mark.to_owned()
        } else {
            format!("{mark} ")
        };
        spans.push(Span::styled(text, style));
    }
    if !last {
        spans.push(Span::raw("· "));
    }
    spans
}

/// A labelled row of entries, wrapped between entries with the names kept in their column.
fn row(label: &str, entries: &[TemplateEntry], ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    let dim = ctx.theme.style(Role::Dim);
    let width = usize::from(ctx.width);
    let head = Span::styled(format!("  {label:<width$}", width = NAMES_COL - 2), dim);
    let mut lines = Vec::new();
    let mut spans = vec![head];
    let mut used = NAMES_COL;
    for (at, entry) in entries.iter().enumerate() {
        let entry = entry_spans(entry, at + 1 == entries.len(), ctx);
        let wide: usize = entry.iter().map(|span| str_width(&span.content)).sum();
        if used > NAMES_COL && used + wide.saturating_sub(1) > width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            spans.push(Span::raw(" ".repeat(NAMES_COL)));
            used = NAMES_COL;
        }
        used += wide;
        spans.extend(entry);
    }
    lines.push(Line::from(spans));
    lines
}

/// The parts of the folder, and what each holds.
const PARTS: [(&str, &str); 5] = [
    (
        "README.md",
        "what is here, and where a copy goes to take effect",
    ),
    (
        "workflows/",
        "Council, Orchestrator, Maker-checker, Best-of-n, … + README.md",
    ),
    (
        "prompts/",
        "each role's frame and the other prompts + README.md",
    ),
    (
        "roles/",
        "an example instructions file per role + README.md",
    ),
    ("mods/", "the starter mods + README.md"),
];

/// The column a part's description starts in, after `├─ workflows/`.
const ABOUT_COL: usize = 22;

/// `~/.argonauts/templates/` and its parts as a tree, so you know where to copy from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateFolder {
    folder: String,
}

impl TemplateFolder {
    pub fn new(folder: impl Into<String>) -> Self {
        Self {
            folder: folder.into(),
        }
    }
}

impl HistoryCell for TemplateFolder {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let dim = ctx.theme.style(Role::Dim);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let width = usize::from(ctx.width);
        let folder = Span::styled(sanitize_line(&self.folder).into_owned(), bold);
        let mut lines = beside(vec![folder, Span::raw(ABOUT_GAP)], ABOUT_FOLDER, dim, width);
        for (at, (part, about)) in PARTS.iter().enumerate() {
            let branch = if at + 1 == PARTS.len() {
                "└─"
            } else {
                "├─"
            };
            let name = format!("{branch} {part:<width$}", width = ABOUT_COL - 3);
            lines.extend(beside(vec![Span::raw(name)], about, dim, width));
        }
        fit_lines(&mut lines, width);
        lines
    }
}

const ABOUT_FOLDER: &str =
    "reference copies, refreshed when Argonauts Code updates; edit a copy, not these";

/// The cells between the folder's name and what it is.
const ABOUT_GAP: &str = "        ";

/// `about` beside `head`, wrapped under its own first letter where the row is too narrow for
/// all of it, so nothing is cut.
fn beside(head: Vec<Span<'static>>, about: &str, dim: Style, width: usize) -> Vec<Line<'static>> {
    let hang = spans_width(&head);
    // A row too narrow to hold a few words beside the head keeps one row, cut at its edge.
    if width < hang + 16 {
        let mut spans = head;
        spans.push(Span::styled(about.to_owned(), dim));
        return vec![Line::from(spans)];
    }
    wrap_text(about, dim, width, &head, &[Span::raw(" ".repeat(hang))])
}
