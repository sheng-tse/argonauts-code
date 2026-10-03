use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::HistoryCell;
use super::layout::{EXPAND_HINT, NEST, TEXT_COL, indent, result_prefix};
use super::tool::body::nested_rows;
use super::tool::{ToolCall, ToolKind, ToolState, ToolTitle, running_bullet};
use crate::render::RenderCtx;
use crate::text::{fit_lines, sanitize_line, spans_width, str_width, wrap_spans};
use crate::theme::Role;

/// Quiet tool calls shown as one summary line. The transcript overlay shows each in full.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollapsedGroup {
    pub calls: Vec<ToolCall>,
}

impl CollapsedGroup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, call: ToolCall) {
        self.calls.push(call);
    }

    pub fn is_running(&self) -> bool {
        self.calls
            .iter()
            .any(|call| call.state == ToolState::Running)
    }

    /// Like `Read 3 files, ran 2 shell commands`, or `Reading 2 files…` while a call runs.
    pub fn summary(&self) -> String {
        let text: String = self
            .summary_spans(Style::new())
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        if self.is_running() && !text.is_empty() {
            format!("{text}…")
        } else {
            text
        }
    }

    fn summary_spans(&self, style: Style) -> Vec<Span<'static>> {
        let mut counts: Vec<(ToolKind, usize)> = Vec::new();
        for call in &self.calls {
            match counts.iter_mut().find(|(kind, _)| *kind == call.kind) {
                Some((_, count)) => *count += 1,
                None => counts.push((call.kind, 1)),
            }
        }
        let running = self.is_running();
        let bold = style.add_modifier(Modifier::BOLD);
        let mut spans = Vec::new();
        for (i, (kind, count)) in counts.iter().enumerate() {
            let (verb, noun) = phrase(*kind, *count, running);
            let verb = if i == 0 {
                capitalize(verb)
            } else {
                format!(", {verb}")
            };
            spans.push(Span::styled(format!("{verb} "), style));
            spans.push(Span::styled(count.to_string(), bold));
            spans.push(Span::styled(format!(" {noun}"), style));
        }
        spans
    }

    fn failed_spans(&self, ctx: &RenderCtx<'_>) -> Vec<Span<'static>> {
        let failed = self
            .calls
            .iter()
            .filter(|call| call.state == ToolState::Failed)
            .count();
        if failed == 0 {
            return Vec::new();
        }
        let theme = ctx.theme;
        vec![
            Span::styled(" · ", theme.style(Role::Dim)),
            Span::styled(
                format!("{failed} failed"),
                theme.style(Role::Error).add_modifier(Modifier::BOLD),
            ),
        ]
    }

    fn current_target(&self) -> Option<String> {
        let call = self
            .calls
            .iter()
            .rev()
            .find(|call| call.state == ToolState::Running)?;
        let target = match &call.title {
            ToolTitle::Call { name, args } if args.trim().is_empty() => name.clone(),
            ToolTitle::Call { args, .. } => args.clone(),
            title => title.plain(),
        };
        let target = sanitize_line(&target).trim().to_owned();
        Some(if call.kind == ToolKind::Shell {
            format!("$ {target}")
        } else {
            target
        })
    }

    fn summary_rows(
        &self,
        ctx: &RenderCtx<'_>,
        lead: Vec<Span<'static>>,
        hang: usize,
    ) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let running = self.is_running();
        let style = if running {
            theme.style(Role::Text)
        } else {
            dim
        };
        let mut spans = self.summary_spans(style);
        spans.extend(self.failed_spans(ctx));
        let width = usize::from(ctx.width);
        if running {
            spans.push(Span::styled(theme.glyphs.ellipsis, style));
        } else if !ctx.expanded
            && spans_width(&lead) + spans_width(&spans) + str_width(EXPAND_HINT) <= width
        {
            // The hint rides on the summary's row, or stays out rather than wrap on its own.
            spans.push(Span::styled(EXPAND_HINT, dim));
        }
        wrap_spans(&spans, width, &lead, &[indent(hang)])
    }

    /// Inside a call's body, starting `indent_cols` columns in.
    pub fn render(&self, ctx: &RenderCtx<'_>, indent_cols: u16) -> Vec<Line<'static>> {
        if self.calls.is_empty() {
            return Vec::new();
        }
        let col = usize::from(indent_cols);
        let width = usize::from(ctx.width);
        let mut rows = Vec::new();
        if ctx.expanded {
            for call in &self.calls {
                rows.extend(nested_rows(call, ctx, col));
            }
        } else {
            rows = self.summary_rows(ctx, vec![indent(col)], col);
            if let Some(target) = self.current_target() {
                // Under the summary like a nested call's result, without a hook of its own.
                rows.push(Line::from(vec![
                    indent(col + NEST),
                    Span::styled(target, ctx.theme.style(Role::Dim)),
                ]));
            }
        }
        fit_lines(&mut rows, width);
        rows
    }
}

impl HistoryCell for CollapsedGroup {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        if self.calls.is_empty() {
            return Vec::new();
        }
        let width = usize::from(ctx.width);
        let mut rows: Vec<Line<'static>> = Vec::new();
        if ctx.expanded {
            for call in &self.calls {
                if !rows.is_empty() {
                    rows.push(Line::default());
                }
                rows.extend(call.lines(ctx));
            }
            return rows;
        }
        match self.current_target() {
            Some(target) => {
                let lead = vec![running_bullet(ctx), Span::raw(" ")];
                rows = self.summary_rows(ctx, lead, TEXT_COL);
                let mut row = result_prefix(ctx.theme);
                row.push(Span::styled(target, ctx.theme.style(Role::Dim)));
                rows.push(Line::from(row));
            }
            None => rows = self.summary_rows(ctx, vec![indent(TEXT_COL)], TEXT_COL),
        }
        fit_lines(&mut rows, width);
        rows
    }

    fn is_animating(&self) -> bool {
        self.is_running()
    }
}

fn phrase(kind: ToolKind, count: usize, running: bool) -> (&'static str, &'static str) {
    let (done, doing, one, many) = match kind {
        ToolKind::Read => ("read", "reading", "file", "files"),
        ToolKind::Search => ("searched for", "searching for", "pattern", "patterns"),
        ToolKind::List => ("listed", "listing", "directory", "directories"),
        ToolKind::Shell => ("ran", "running", "shell command", "shell commands"),
        ToolKind::Edit => ("edited", "editing", "file", "files"),
        ToolKind::Write => ("wrote", "writing", "file", "files"),
        ToolKind::Fetch => ("fetched", "fetching", "page", "pages"),
        ToolKind::Mcp => ("called", "calling", "MCP tool", "MCP tools"),
        ToolKind::Agent => ("ran", "running", "agent", "agents"),
        ToolKind::Other => ("used", "using", "tool", "tools"),
    };
    let verb = if running { doing } else { done };
    let noun = if count == 1 { one } else { many };
    (verb, noun)
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::cell::ToolItem;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    fn done(kind: ToolKind, name: &str, args: &str) -> ToolCall {
        ToolCall::call(kind, name, args).with_state(ToolState::Done)
    }

    #[test]
    fn phrases_count_by_kind_in_order() {
        let mut group = CollapsedGroup::new();
        group.push(done(ToolKind::Read, "Read", "a.rs"));
        group.push(done(ToolKind::Shell, "Bash", "ls"));
        group.push(done(ToolKind::Read, "Read", "b.rs"));
        group.push(done(ToolKind::Read, "Read", "c.rs"));
        group.push(done(ToolKind::Shell, "Bash", "pwd"));
        assert_eq!(group.summary(), "Read 3 files, ran 2 shell commands");

        let mut group = CollapsedGroup::new();
        group.push(done(ToolKind::List, "LS", "src"));
        group.push(done(ToolKind::Search, "Grep", "fn main"));
        group.push(done(ToolKind::Fetch, "WebFetch", "x"));
        group.push(done(ToolKind::Mcp, "slack", "post"));
        group.push(done(ToolKind::Other, "Thing", ""));
        assert_eq!(
            group.summary(),
            "Listed 1 directory, searched for 1 pattern, fetched 1 page, called 1 MCP tool, used 1 tool"
        );
        let mut two = CollapsedGroup::new();
        two.push(done(ToolKind::List, "LS", "a"));
        two.push(done(ToolKind::List, "LS", "b"));
        two.push(done(ToolKind::Edit, "Edit", "c"));
        two.push(done(ToolKind::Write, "Write", "d"));
        two.push(done(ToolKind::Agent, "Task", "e"));
        assert_eq!(
            two.summary(),
            "Listed 2 directories, edited 1 file, wrote 1 file, ran 1 agent"
        );
    }

    #[test]
    fn a_running_group_speaks_in_the_present() {
        let mut group = CollapsedGroup::new();
        group.push(done(ToolKind::Read, "Read", "a.rs"));
        group.push(ToolCall::call(ToolKind::Read, "Read", "b.rs"));
        group.push(ToolCall::call(ToolKind::Search, "Grep", "x"));
        assert!(group.is_running());
        assert_eq!(group.summary(), "Reading 2 files, searching for 1 pattern…");
    }

    #[test]
    fn collapsed_at_column_two_and_expanded_in_the_overlay() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let mut group = CollapsedGroup::new();
        group.push(
            done(ToolKind::Shell, "Bash", "cargo test").with_item(ToolItem::Command {
                command: "cargo test".into(),
                output: vec!["ok".into()],
                hidden: 0,
            }),
        );
        let lines = group.lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "  Ran 1 shell command (ctrl+o to expand)"
        );
        assert_eq!(lines[0].spans[1].style, theme.style(Role::Dim));
        assert_eq!(
            lines[0].spans[2].style,
            theme.style(Role::Dim).add_modifier(Modifier::BOLD)
        );
        group.push(
            done(ToolKind::Read, "Read", "notes.txt").with_item(ToolItem::text("Read 3 lines")),
        );
        let expanded = group.lines(&ctx.with_expanded(true));
        assert_eq!(
            plain_text(&expanded),
            "● Bash(cargo test)\n  ⎿  $ cargo test\n     ok\n\n● Read(notes.txt)\n  ⎿  Read 3 lines"
        );
        assert!(CollapsedGroup::new().lines(&ctx).is_empty());
    }

    #[test]
    fn a_running_group_names_what_runs_now() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let mut group = CollapsedGroup::new();
        group.push(done(ToolKind::Read, "Read", "notes.txt"));
        group.push(ToolCall::call(ToolKind::Shell, "Bash", "ls src"));
        let lines = group.lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "● Reading 1 file, running 1 shell command…\n  ⎿  $ ls src"
        );
        let bullet = crate::text::marks::decoration(theme.style(Role::Dim));
        assert_eq!(lines[0].spans[0].style, bullet);
        assert_eq!(lines[0].spans[2].style, theme.style(Role::Text));
        assert_eq!(lines[1].spans.last().unwrap().style, theme.style(Role::Dim));
        let nested = group.render(&ctx, 5);
        assert_eq!(
            plain_text(&nested),
            "     Reading 1 file, running 1 shell command…\n        $ ls src"
        );
    }

    #[test]
    fn a_narrow_row_drops_the_expand_hint_rather_than_wrap_it() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let mut group = CollapsedGroup::new();
        group.push(done(ToolKind::Shell, "Bash", "cargo build"));
        group.push(done(ToolKind::Shell, "Bash", "cargo test"));
        let at = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
            plain_text(&group.lines(&ctx))
        };
        assert_eq!(at(60), "  Ran 2 shell commands (ctrl+o to expand)");
        assert_eq!(at(30), "  Ran 2 shell commands");
    }
}
