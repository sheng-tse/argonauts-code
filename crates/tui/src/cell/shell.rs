//! A `!` command you ran: the command on your message band behind a pink `!`, as Claude Code
//! echoes bash mode, and its output hanging under `  ⎿  `. Output taller than a screen folds,
//! and Ctrl+O shows all of it.

use std::cell::RefCell;
use std::time::Instant;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::HistoryCell;
use super::layout::{BODY_COL, hook, indent, more_row};
use crate::bottom::SHELL_PROMPT;
use crate::render::RenderCtx;
use crate::text::{fit_lines, marks, sanitize_line, str_width, wrap_spans};
use crate::theme::Role;

/// A command's own lines past this many fold, as a tool call's do.
const COMMAND_LINES: usize = 10;

/// Assumed where the screen's height isn't known.
const SCREEN_ROWS: usize = 24;

/// What else shares the screen with a long output: the command, the fold row, the input, and
/// the mode line.
const OTHER_ROWS: usize = 8;

/// A line keeps this many bytes of its start and of its end, and the middle of a longer one
/// is cut: a minified bundle or `base64` has nothing to read there, and would wrap to
/// thousands of rows.
const LINE_PART: usize = 2048;

/// Output kept in memory: the first lines, up to [`KEPT_HEAD`] of them and a quarter of the
/// bytes, and the newest. A command that prints without end can't grow the screen's memory
/// without end.
const KEPT_LINES: usize = 10_000;
const KEPT_BYTES: usize = 1 << 20;
const KEPT_HEAD: usize = 1_000;

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellEnd {
    Exit(i32),
    /// A signal ended it, with no exit code.
    Signal,
    /// Ctrl+C stopped it.
    Stopped,
}

#[derive(Debug, Clone)]
pub struct ShellRun {
    pub command: String,
    /// The last line is the one still printing, empty once a newline ends the one before.
    lines: Vec<OutputLine>,
    /// What `lines` hold, in bytes.
    bytes: usize,
    /// Lines kept at the start once lines are dropped from the middle.
    head: Option<usize>,
    /// Lines dropped from the middle past [`KEPT_LINES`] or [`KEPT_BYTES`].
    dropped: usize,
    pub started: Instant,
    pub end: Option<ShellEnd>,
    /// The rows of the finished lines as last drawn, so a frame wraps only the line printing.
    wrapped: RefCell<Wrapped>,
}

/// One line of output. One too long to keep whole keeps its start and its end.
#[derive(Debug, Clone, Default)]
struct OutputLine {
    text: String,
    /// Once the middle is cut: where the start ends in `text`, and the characters cut.
    cut: Option<(usize, usize)>,
}

impl OutputLine {
    fn push(&mut self, more: &str) {
        self.text.push_str(more);
        // A carriage return starts the line over, as a progress bar does, so what came before
        // the last one that text followed is gone for good.
        let body = self.text.trim_end_matches('\r');
        if let Some(at) = body.rfind('\r') {
            self.text.drain(..=at);
            self.cut = None;
        }
        // Cut in batches, so a line that grows a little at a time isn't copied at every read.
        if self.text.len() > 3 * LINE_PART {
            let start = self
                .cut
                .map_or_else(|| floor_boundary(&self.text, LINE_PART), |(start, _)| start);
            let end = ceil_boundary(&self.text, self.text.len() - LINE_PART).max(start);
            let cut = self.text[start..end].chars().count();
            self.text.drain(start..end);
            let before = self.cut.map_or(0, |(_, cut)| cut);
            self.cut = Some((start, before + cut));
        }
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The line as a terminal leaves it, with a dim note where its middle was cut.
    fn spans(&self, text: Style, dim: Style, ellipsis: &str) -> Vec<Span<'static>> {
        match self.cut {
            None => vec![Span::styled(settled(&self.text), text)],
            Some((start, cut)) => {
                let unit = if cut == 1 { "character" } else { "characters" };
                vec![
                    Span::styled(settled(&self.text[..start]), text),
                    Span::styled(format!(" {ellipsis} {cut} {unit} cut {ellipsis} "), dim),
                    Span::styled(settled(&self.text[start..]), text),
                ]
            }
        }
    }
}

/// What the finished lines' rows were wrapped for, and the rows.
#[derive(Debug, Clone, Default)]
struct Wrapped {
    key: Option<WrapKey>,
    /// The rows of `lines[i]` for each finished line so far.
    rows: Vec<Vec<Line<'static>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WrapKey {
    width: usize,
    text: Style,
    dim: Style,
    ellipsis: &'static str,
}

impl WrapKey {
    fn of(ctx: &RenderCtx<'_>) -> Self {
        Self {
            width: usize::from(ctx.width),
            text: ctx.theme.style(Role::Text),
            dim: ctx.theme.style(Role::Dim),
            ellipsis: ctx.theme.glyphs.ellipsis,
        }
    }

    fn wrap(&self, line: &OutputLine) -> Vec<Line<'static>> {
        let lead = [indent(BODY_COL)];
        let spans = line.spans(self.text, self.dim, self.ellipsis);
        wrap_spans(&spans, self.width, &lead, &lead)
    }
}

impl ShellRun {
    pub fn new(command: impl Into<String>, started: Instant) -> Self {
        Self {
            command: command.into(),
            lines: Vec::new(),
            bytes: 0,
            head: None,
            dropped: 0,
            started,
            end: None,
            wrapped: RefCell::default(),
        }
    }

    pub fn push_output(&mut self, text: &str) {
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            if self.lines.is_empty() {
                self.lines.push(OutputLine::default());
            }
            let open = self.lines.last_mut().expect("a line to print on");
            self.bytes -= open.text.len();
            open.push(first);
            self.bytes += open.text.len();
        }
        for part in parts {
            let mut line = OutputLine::default();
            line.push(part);
            self.bytes += line.text.len();
            self.lines.push(line);
        }
        self.trim();
    }

    /// Drops lines from the middle once the output passes what's kept, in batches, so a
    /// chatty command isn't copied once per read.
    fn trim(&mut self) {
        if self.lines.len() <= KEPT_LINES && self.bytes <= KEPT_BYTES {
            return;
        }
        let head = *self.head.get_or_insert_with(|| {
            let mut bytes = 0;
            self.lines
                .iter()
                .take(KEPT_HEAD)
                .take_while(|line| {
                    bytes += line.text.len();
                    bytes <= KEPT_BYTES / 4
                })
                .count()
        });
        let (lines_to, bytes_to) = (KEPT_LINES * 9 / 10, KEPT_BYTES * 9 / 10);
        let mut end = head;
        let mut freed = 0;
        let last = self.lines.len() - 1;
        while end < last
            && (self.lines.len() - (end - head) > lines_to || self.bytes - freed > bytes_to)
        {
            freed += self.lines[end].text.len();
            end += 1;
        }
        self.lines.drain(head..end);
        self.bytes -= freed;
        self.dropped += end - head;
        let wrapped = self.wrapped.get_mut();
        if wrapped.rows.len() > head {
            let cached_end = end.min(wrapped.rows.len());
            wrapped.rows.drain(head..cached_end);
        }
    }

    pub fn finish(&mut self, end: ShellEnd) {
        self.end = Some(end);
    }

    pub fn is_running(&self) -> bool {
        self.end.is_none()
    }

    /// The lines printed so far. A last line still empty isn't output yet.
    fn output(&self) -> &[OutputLine] {
        match self.lines.split_last() {
            Some((last, before)) if last.is_empty() => before,
            _ => &self.lines,
        }
    }

    /// The cache, brought up to date with every line but the last, which may still grow.
    fn wrapped(&self, key: WrapKey) -> std::cell::RefMut<'_, Wrapped> {
        let mut wrapped = self.wrapped.borrow_mut();
        if wrapped.key != Some(key) {
            wrapped.key = Some(key);
            wrapped.rows.clear();
        }
        let finished = self.lines.len().saturating_sub(1);
        while wrapped.rows.len() < finished {
            let rows = key.wrap(&self.lines[wrapped.rows.len()]);
            wrapped.rows.push(rows);
        }
        wrapped
    }

    fn command_rows(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let command = crate::text::sanitize(self.command.trim_end_matches('\n'));
        let lines: Vec<&str> = command.split('\n').collect();
        let shown = if ctx.expanded {
            lines.len()
        } else {
            lines.len().min(COMMAND_LINES)
        };
        let marker = [
            marks::decorative(SHELL_PROMPT, theme.style(Role::Shell)),
            Span::raw(" "),
        ];
        let hang = [Span::raw(" ".repeat(str_width(SHELL_PROMPT) + 1))];
        let bright = theme.style(Role::Bright);
        let mut rows = Vec::new();
        for (index, line) in lines.iter().take(shown).enumerate() {
            let spans = [Span::styled((*line).to_owned(), bright)];
            let first: &[Span<'static>] = if index == 0 { &marker } else { &hang };
            rows.extend(wrap_spans(&spans, width, first, &hang));
        }
        if shown < lines.len() {
            rows.push(more_row(
                hang.to_vec(),
                theme,
                lines.len() - shown,
                true,
                width,
            ));
        }
        fit_lines(&mut rows, width);
        let band = theme.style(Role::UserBand);
        rows.into_iter().map(|row| row.style(band)).collect()
    }

    fn body_rows(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);
        let lead = [indent(BODY_COL)];
        let key = WrapKey::of(ctx);
        let wrapped = self.wrapped(key);
        let output = self.output();
        // Only the line still printing is wrapped again; the rest come from the cache.
        let printing = if output.len() > wrapped.rows.len() {
            key.wrap(&output[output.len() - 1])
        } else {
            Vec::new()
        };
        let rows_of = |index: usize| match wrapped.rows.get(index) {
            Some(rows) => rows.as_slice(),
            None => printing.as_slice(),
        };
        let fold = fold_at(ctx);
        let all: usize = (0..output.len()).map(|index| rows_of(index).len()).sum();
        let mut rows = Vec::new();
        if ctx.expanded || all <= fold {
            for index in 0..output.len() {
                if Some(index) == self.head && self.dropped > 0 {
                    rows.push(more_row(lead.to_vec(), theme, self.dropped, false, width));
                }
                rows.extend_from_slice(rows_of(index));
            }
        } else if self.is_running() {
            // While it prints, the newest rows matter, as in a terminal. A line cut by the
            // fold counts as hidden.
            let mut newest: Vec<&[Line<'static>]> = Vec::new();
            let mut room = fold;
            let mut hidden = self.dropped;
            for index in (0..output.len()).rev() {
                let line = rows_of(index);
                if line.len() > room {
                    newest.push(&line[line.len() - room..]);
                    hidden += index + 1;
                    break;
                }
                room -= line.len();
                newest.push(line);
            }
            rows.push(more_row(lead.to_vec(), theme, hidden, false, width));
            for line in newest.into_iter().rev() {
                rows.extend_from_slice(line);
            }
        } else {
            let mut room = fold;
            let mut hidden = self.dropped;
            for (index, _) in output.iter().enumerate() {
                let line = rows_of(index);
                if line.len() > room {
                    rows.extend_from_slice(&line[..room]);
                    hidden += output.len() - index;
                    break;
                }
                room -= line.len();
                rows.extend_from_slice(line);
            }
            rows.push(more_row(lead.to_vec(), theme, hidden, true, width));
        }
        let end = match self.end {
            None => {
                let elapsed = ctx.clock.now.saturating_duration_since(self.started);
                let running = format!(
                    "Running{} ({})",
                    theme.glyphs.ellipsis,
                    crate::format::duration(elapsed)
                );
                rows.extend(wrap_spans(
                    &[Span::styled(running, dim)],
                    width,
                    &lead,
                    &lead,
                ));
                Some(("(ctrl+c to stop)".to_owned(), dim))
            }
            Some(ShellEnd::Exit(0)) if output.is_empty() => Some(("(No output)".to_owned(), dim)),
            Some(ShellEnd::Exit(0)) => None,
            Some(ShellEnd::Exit(code)) => {
                Some((format!("Exit code {code}"), theme.style(Role::Error)))
            }
            Some(ShellEnd::Signal) => {
                Some(("Ended by a signal".to_owned(), theme.style(Role::Error)))
            }
            Some(ShellEnd::Stopped) => Some(("Stopped".to_owned(), dim)),
        };
        if let Some((words, style)) = end {
            rows.extend(wrap_spans(
                &[Span::styled(words, style)],
                width,
                &lead,
                &lead,
            ));
        }
        if let Some(first) = rows.first_mut() {
            hook(first, theme);
        }
        rows
    }
}

/// The line as a terminal leaves it: a carriage return starts it over, as a progress bar does.
fn settled(line: &str) -> String {
    let line = line.trim_end_matches('\r');
    sanitize_line(line.rsplit('\r').next().unwrap_or(line)).into_owned()
}

fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// Output lines that fit on one screen with the rest of the app.
fn fold_at(ctx: &RenderCtx<'_>) -> usize {
    let screen = match usize::from(ctx.rows) {
        0 => SCREEN_ROWS,
        rows => rows,
    };
    screen.saturating_sub(OTHER_ROWS).max(3)
}

impl HistoryCell for ShellRun {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let mut lines = self.command_rows(ctx);
        lines.extend(self.body_rows(ctx));
        lines
    }

    fn is_animating(&self) -> bool {
        self.is_running()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn text(cell: &ShellRun, ctx: &RenderCtx<'_>) -> String {
        plain_text(&cell.lines(ctx))
    }

    #[test]
    fn a_running_command_counts_up_and_says_how_to_stop_it() {
        let theme = theme();
        let start = Instant::now();
        let ctx = RenderCtx::new(
            40,
            &theme,
            Clock::new(start, start + Duration::from_secs(3)),
        );
        let cell = ShellRun::new("sleep 25", start);
        assert_eq!(
            text(&cell, &ctx),
            "! sleep 25\n  ⎿  Running… (3s)\n     (ctrl+c to stop)"
        );
        assert!(cell.is_animating());
        let lines = cell.lines(&ctx);
        assert_eq!(
            lines[0].spans[0].style,
            marks::decoration(theme.style(Role::Shell))
        );
        assert_eq!(lines[0].style.bg, theme.style(Role::UserBand).bg);
    }

    #[test]
    fn output_streams_in_pieces_and_the_end_says_how_it_went() {
        let theme = theme();
        let start = Instant::now();
        let ctx = RenderCtx::new(40, &theme, Clock::still(start));
        let mut cell = ShellRun::new("echo hi", start);
        cell.push_output("h");
        cell.push_output("i\nthere");
        cell.push_output("\n");
        cell.finish(ShellEnd::Exit(0));
        assert_eq!(text(&cell, &ctx), "! echo hi\n  ⎿  hi\n     there");
        assert!(!cell.is_animating());

        cell.finish(ShellEnd::Exit(2));
        assert_eq!(
            text(&cell, &ctx),
            "! echo hi\n  ⎿  hi\n     there\n     Exit code 2"
        );
        cell.finish(ShellEnd::Stopped);
        assert!(text(&cell, &ctx).ends_with("     Stopped"));

        let mut quiet = ShellRun::new("true", start);
        quiet.finish(ShellEnd::Exit(0));
        assert_eq!(text(&quiet, &ctx), "! true\n  ⎿  (No output)");
    }

    #[test]
    fn progress_bars_and_escapes_settle_before_they_show() {
        let theme = theme();
        let start = Instant::now();
        let ctx = RenderCtx::new(40, &theme, Clock::still(start));
        let mut cell = ShellRun::new("fetch", start);
        cell.push_output("10%\r50%\r100%\r\n\u{1b}[31mred\u{1b}[0m\n");
        cell.finish(ShellEnd::Exit(0));
        assert_eq!(text(&cell, &ctx), "! fetch\n  ⎿  100%\n     red");
    }

    #[test]
    fn output_longer_than_a_screen_folds_until_ctrl_o() {
        let theme = theme();
        let start = Instant::now();
        let ctx = RenderCtx::new(60, &theme, Clock::still(start)).with_rows(14);
        let mut cell = ShellRun::new("seq 20", start);
        for n in 1..=20 {
            cell.push_output(&format!("{n}\n"));
        }
        // While it runs, the newest lines show.
        let running = text(&cell, &ctx);
        assert!(
            running.starts_with("! seq 20\n  ⎿  … +14 lines\n     15\n"),
            "{running}"
        );
        assert!(running.contains("     20\n     Running…"), "{running}");

        cell.finish(ShellEnd::Exit(0));
        let folded = text(&cell, &ctx);
        assert_eq!(
            folded,
            "! seq 20\n  ⎿  1\n     2\n     3\n     4\n     5\n     6\n     … +14 lines (ctrl+o to expand)"
        );
        let expanded = text(&cell, &ctx.with_expanded(true));
        assert_eq!(expanded.lines().count(), 21);
        assert!(expanded.ends_with("     20"));
    }

    #[test]
    fn a_long_command_folds_and_wraps_under_its_marker() {
        let theme = theme();
        let start = Instant::now();
        let ctx = RenderCtx::new(20, &theme, Clock::still(start));
        let script: String = (1..=12).map(|n| format!("echo {n}\n")).collect();
        let mut cell = ShellRun::new(script, start);
        cell.finish(ShellEnd::Exit(0));
        let rows = text(&cell, &ctx);
        assert!(rows.starts_with("! echo 1\n  echo 2\n"), "{rows}");
        assert!(rows.contains("  echo 10\n  … +2 lines\n"), "{rows}");

        let wide = ShellRun::new("git log --oneline --graph --all", start);
        let rows = text(&wide, &ctx);
        assert!(
            rows.starts_with("! git log --oneline\n  --graph --all\n"),
            "{rows}"
        );
    }

    #[test]
    fn endless_output_keeps_its_start_and_its_newest_lines() {
        let mut cell = ShellRun::new("yes", Instant::now());
        for _ in 0..3 {
            cell.push_output(&"y\n".repeat(KEPT_LINES));
        }
        assert!(cell.lines.len() <= KEPT_LINES);
        assert_eq!(cell.dropped + cell.lines.len(), 3 * KEPT_LINES + 1);

        // Long lines reach the byte limit first, and the start stays.
        let mut cell = ShellRun::new("cat big.log", Instant::now());
        cell.push_output("first\n");
        let line = format!("{}\n", "z".repeat(1000));
        for _ in 0..3000 {
            cell.push_output(&line);
        }
        let held: usize = cell.lines.iter().map(|line| line.text.len()).sum();
        assert_eq!(held, cell.bytes);
        assert!(cell.bytes <= KEPT_BYTES, "{}", cell.bytes);
        assert!(cell.dropped > 0);
        assert_eq!(cell.lines[0].text, "first");
    }

    #[test]
    fn a_line_without_end_keeps_its_start_and_end_and_says_what_was_cut() {
        let theme = theme();
        let start = Instant::now();
        let ctx = RenderCtx::new(100, &theme, Clock::still(start)).with_rows(30);
        let mut cell = ShellRun::new("base64 -i image.png", start);
        cell.push_output("BEGIN");
        for _ in 0..200 {
            cell.push_output(&"A".repeat(1000));
        }
        cell.push_output("END\n");
        assert!(cell.bytes <= 3 * LINE_PART, "{}", cell.bytes);
        cell.finish(ShellEnd::Exit(0));

        let expanded = text(&cell, &ctx.with_expanded(true));
        assert!(expanded.contains("BEGINAAA"), "{expanded}");
        assert!(expanded.contains("AAAEND"), "{expanded}");
        let Some((LINE_PART, cut)) = cell.lines[0].cut else {
            panic!("the middle is cut: {:?}", cell.lines[0].cut);
        };
        assert_eq!(cut + cell.lines[0].text.len(), 200_008);
        let words: Vec<&str> = expanded.split_whitespace().collect();
        let at = words.iter().position(|word| *word == cut.to_string());
        assert!(
            at.is_some_and(
                |at| words[at - 1..at + 4] == ["…", &cut.to_string(), "characters", "cut", "…"]
            ),
            "{expanded}"
        );
        // At most three parts' worth of rows, where the 200 KB once took 2,000.
        let most = 3 * LINE_PART / (100 - BODY_COL) + 4;
        assert!(
            expanded.lines().count() <= most,
            "{}",
            expanded.lines().count()
        );

        // Folded, the rows still fit the screen, and the line counts as hidden.
        let folded = text(&cell, &ctx);
        assert_eq!(folded.lines().count(), 1 + fold_at(&ctx) + 1, "{folded}");
        assert!(folded.ends_with("… +1 line (ctrl+o to expand)"), "{folded}");
    }

    #[test]
    fn output_folds_on_wrapped_rows_and_shows_the_newest_while_it_runs() {
        let theme = theme();
        let start = Instant::now();
        let ctx = RenderCtx::new(40, &theme, Clock::still(start)).with_rows(14);
        let mut cell = ShellRun::new("npm run build", start);
        for n in 1..=4 {
            cell.push_output(&format!("line {n} {}\n", "word ".repeat(30)));
        }
        let fold = fold_at(&ctx);
        let running = text(&cell, &ctx);
        let body: Vec<&str> = running.lines().skip(1).collect();
        // The fold row, the newest rows, and the two rows that say it's running.
        assert_eq!(body.len(), 1 + fold + 2, "{running}");
        // Line 3 shows its last row, and counts as hidden with the two above it.
        assert!(body[0].ends_with("… +3 lines"), "{running}");
        assert!(running.contains("line 4"), "{running}");

        cell.finish(ShellEnd::Exit(0));
        let folded = text(&cell, &ctx);
        assert_eq!(folded.lines().count(), 1 + fold + 1, "{folded}");
        assert!(folded.contains("line 1"), "{folded}");
        assert!(!folded.contains("line 4"), "{folded}");
    }

    #[test]
    fn finished_lines_are_wrapped_once_and_kept() {
        let theme = theme();
        let start = Instant::now();
        let ctx = RenderCtx::new(60, &theme, Clock::still(start));
        let mut cell = ShellRun::new("make", start);
        cell.push_output("one\ntwo\nthr");
        let first = cell.lines(&ctx);
        assert_eq!(
            cell.wrapped.borrow().rows.len(),
            2,
            "the line printing isn't kept"
        );
        let kept = cell.wrapped.borrow().rows.clone();
        cell.push_output("ee\nfour");
        let second = cell.lines(&ctx);
        assert_eq!(cell.wrapped.borrow().rows[..2], kept[..]);
        assert_eq!(cell.wrapped.borrow().rows.len(), 3);
        assert_ne!(first, second);
        assert!(text(&cell, &ctx).contains("three\n     four"));

        // Another width wraps them all again.
        let narrow = RenderCtx::new(30, &theme, Clock::still(start));
        cell.lines(&narrow);
        assert_eq!(cell.wrapped.borrow().key.map(|key| key.width), Some(30));
    }

    #[test]
    fn a_progress_bar_that_never_ends_its_line_stays_small() {
        let mut cell = ShellRun::new("download", Instant::now());
        for n in 0..100_000 {
            cell.push_output(&format!("{}%\r", n % 100));
        }
        assert!(cell.bytes < 16, "{}", cell.bytes);
        cell.push_output("\n");
        assert_eq!(settled(&cell.lines[0].text), "99%");
    }
}
