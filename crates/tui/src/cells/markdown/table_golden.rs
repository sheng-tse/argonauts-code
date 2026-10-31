//! The first table of an answer compared with Claude Code's on the same screen, drawn as
//! `tests/golden/markdown/table_after` has it at 118 columns: a rule between every row, headers centred and not
//! bold, the borders in the text colour and code in its own. At 133 columns its columns and the
//! second table's are the widths Claude Code gave them on that screen.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::{Indent, render_markdown};
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::text::plain_text;
use crate::testing::pictures::golden::{Picture, Region, assert_matches, theme};

const RUNTIMES: &str = "\
| Runtime | Reviewer or judge (no tools) | Coding through OpenCode | Tested live |
|---|---|---|---|
| Ollama | built in | Argonauts writes OpenCode's provider entry | yes, today |
| LM Studio | built in | Argonauts writes the entry | unit tests only |
| llama-server | only if you add `[endpoints.<name>]` | you write the OpenCode entry yourself | no |
| vLLM | only if you add `[endpoints.<name>]` | you write the OpenCode entry yourself | no |
";

const CHECKS: &str = "\
| # | Where | Do this | Bug you should see |
|---|---|---|---|
| 1 | TUI | In any repo, start `todo` and type `/export` | The export menu opens behind the ruled input box |
| 2 | CLI | `todo export --json` then `todo export --csv` in the same shell | The JSON file has only the header row, but the CSV file lists all 17 items |
| 3 | Sync | `cd ~/code/todo-app/tests/fixtures/big-list && todo sync`, accept the host's new key, then add the sample list | The sync ends in about a minute, then keeps re-reading the list. The app marks it stuck, retries the same server, and that loops too, so stop the sync |
| 4 | Sync | Right after test 3 ends, run `todo status` | The lock file is still there (25 KB), and its age says about 4 minutes |
| 5 | CLI | In big-list: `todo export --all --plain --format json \"Say hello in one word.\"` | The export fails in under a second and the sync waits on a stuck-task question. The log says `Unrecognized flag: --format`; the app reports \"its CLI's output changed\". Seen on every run so far |
";

/// The answer's lines at `width`, under its 2-cell indent.
fn answer(source: &str, width: u16) -> Vec<Line<'static>> {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    let indent = Indent::new(vec![Span::raw("  ")], vec![Span::raw("  ")]);
    render_markdown(source, &ctx, &indent)
}

/// The text widths of the columns, read off the box's top border.
fn column_widths(lines: &[Line<'_>]) -> Vec<usize> {
    let top = plain_text(&lines[..1]);
    top.trim()
        .trim_start_matches('┌')
        .trim_end_matches('┐')
        .split('┬')
        .map(|column| column.chars().count() - 2)
        .collect()
}

#[test]
fn a_table_draws_as_its_picture_does() {
    let picture = Picture::screen("markdown", "table_after");
    let theme = theme();
    let lines = answer(RUNTIMES, 118);
    assert_eq!(lines.len(), picture.rows.len());
    let mut buf = Buffer::empty(Rect::new(0, 0, 118, lines.len() as u16));
    for (y, line) in lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, 118);
    }
    assert_matches(&picture, &buf, &Region::whole(&picture, (0, 0)), &theme);
    assert_eq!(column_widths(&lines), [12, 33, 40, 14]);
}

#[test]
fn at_133_columns_the_widths_are_claude_codes() {
    assert_eq!(column_widths(&answer(RUNTIMES, 133)), [12, 34, 42, 15]);
    assert_eq!(column_widths(&answer(CHECKS, 133)), [3, 5, 54, 53]);
}
