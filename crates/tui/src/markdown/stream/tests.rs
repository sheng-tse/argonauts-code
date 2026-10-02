use std::time::{Duration, Instant};

use ratatui::text::Line;

use super::MarkdownStream;
use crate::cell::AssistantMessage;
use crate::markdown::render_markdown;
use crate::render::{Clock, RenderCtx};
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn whole(source: &str, width: u16) -> Vec<Line<'static>> {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    render_markdown(source, &ctx, &AssistantMessage::indent(&theme))
}

const ANSWER: &str = "\
Here's the plan. It touches **three** files and one `Cargo.toml`.

# Steps

1. Read the export command.
2. Add a `--json` flag.
   - parse it in `cli.rs`
   - pass it down
3. Write the tests.

Still waiting on you:
- The questions.
- The review follow-ups, which take a while to describe because they wrap past the width.

## Right under
Text right under a heading gets a blank row anyway.
### And again
1. numbered
   1. lettered

```
bare fence line
  indented
```
#### Code right under a heading
```sh
cd /tmp && echo \"hi\"
```

```rust
fn write(&self, out: &mut dyn Write) -> Result<()> {
    match self.format {
        Format::Json => self.write_json(out),

        Format::Csv => self.write_csv(out),
    }
}
```

| File | Change |
|---|---|
| src/export.rs | new writer |
| src/cli.rs | the flag |

> Note: the old CSV path stays
> as it is.

---

- [x] plan
- [ ] build
Done.
";

/// Streams `source` in `chunk` byte pieces, taking stable rows after every piece, and checks
/// the tail against a fresh render each time.
fn stream(source: &str, chunk: usize, width: u16) -> (Vec<Line<'static>>, MarkdownStream) {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    let mut stream = MarkdownStream::new();
    let mut printed = Vec::new();
    let mut at = 0;
    while at < source.len() {
        let mut end = (at + chunk).min(source.len());
        while !source.is_char_boundary(end) {
            end += 1;
        }
        stream.push(&source[at..end]);
        at = end;
        printed.extend(stream.take_stable(&ctx));
        let mut shown = printed.clone();
        shown.extend(stream.tail(&ctx));
        assert_eq!(
            plain_text(&shown),
            plain_text(&whole(&source[..at], width)),
            "after {at} bytes in {chunk} byte pieces at width {width}"
        );
    }
    printed.extend(stream.finish(&ctx));
    (printed, stream)
}

#[test]
fn every_row_prints_once_and_the_whole_matches() {
    for width in [80u16, 40, 24] {
        for chunk in [1, 3, 20, 4096] {
            let (printed, stream) = stream(ANSWER, chunk, width);
            let expected = whole(ANSWER, width);
            assert_eq!(printed, expected, "{chunk} byte pieces at width {width}");
            assert_eq!(stream.emitted(), expected.len());
        }
    }
}

#[test]
fn only_finished_lines_print() {
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let mut stream = MarkdownStream::new();
    stream.push("Hello, wor");
    assert!(stream.take_stable(&ctx).is_empty());
    assert_eq!(plain_text(&stream.tail(&ctx)), "● Hello, wor");
    stream.push("ld!\n");
    assert!(stream.take_stable(&ctx).is_empty());
    stream.push("Next line");
    assert!(!stream.has_new_lines());
    stream.push("\n");
    assert_eq!(plain_text(&stream.take_stable(&ctx)), "● Hello, world!");
    assert_eq!(plain_text(&stream.tail(&ctx)), "  Next line");
    assert_eq!(plain_text(&stream.finish(&ctx)), "  Next line");
}

#[test]
fn reduced_motion_shows_only_whole_lines() {
    let mut theme = theme();
    theme.reduced_motion = true;
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let mut stream = MarkdownStream::new();
    stream.push("Hello, wor");
    assert!(stream.tail(&ctx).is_empty(), "no word by word");
    stream.push("ld!\nNext li");
    assert_eq!(plain_text(&stream.tail(&ctx)), "● Hello, world!");
    stream.push("ne\n");
    assert_eq!(plain_text(&stream.take_stable(&ctx)), "● Hello, world!");
    assert_eq!(plain_text(&stream.tail(&ctx)), "  Next line");
    stream.push("Last");
    assert_eq!(plain_text(&stream.tail(&ctx)), "  Next line");
    assert_eq!(plain_text(&stream.finish(&ctx)), "  Next line\n  Last");
}

#[test]
fn an_underline_turns_the_held_line_into_a_heading() {
    let (printed, _) = stream("Intro\n\nTitle\n---\nBody\n", 2, 40);
    assert_eq!(printed, whole("Intro\n\nTitle\n---\nBody\n", 40));
}

#[test]
fn tables_wait_until_they_end() {
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let mut stream = MarkdownStream::new();
    stream.push("Intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n");
    assert_eq!(plain_text(&stream.take_stable(&ctx)), "● Intro");
    assert!(plain_text(&stream.tail(&ctx)).contains("│ a │ b │"));
    stream.push("| 5 | 6 |\n\nAfter\n\n");
    let rows = plain_text(&stream.take_stable(&ctx));
    assert!(
        rows.contains("│ 5 │ 6 │") && rows.contains("After"),
        "{rows}"
    );
}

#[test]
fn split_escape_sequences_are_still_removed() {
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let mut stream = MarkdownStream::new();
    for piece in [
        "red \x1b[3",
        "1mtext\x1b",
        "[0m done\n",
        "tail \x1b]0;ti",
        "tle\x07 end\n",
    ] {
        stream.push(piece);
    }
    let mut rows = stream.take_stable(&ctx);
    rows.extend(stream.finish(&ctx));
    assert_eq!(plain_text(&rows), "● red text done\n  tail  end");
}

#[test]
fn rendering_restarts_at_the_newest_block() {
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let mut stream = MarkdownStream::new();
    for i in 0..50 {
        stream.push(&format!("Paragraph {i} with a few words.\n\n"));
        stream.take_stable(&ctx);
    }
    stream.push("Last.\n");
    stream.take_stable(&ctx);
    let region = stream.clean.len() - stream.checkpoint.at;
    assert!(region < 80, "the region spans {region} bytes");
}

#[test]
fn a_code_block_streams_one_line_at_a_time() {
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let mut stream = MarkdownStream::new();
    stream.push("Code:\n\n```python\n");
    for i in 0..200 {
        stream.push(&format!("x_{i} = \"value {i}\"  # note\n"));
        stream.take_stable(&ctx);
    }
    assert!(stream.checkpoint.fence.is_some());
    let region = stream.clean.len() - stream.checkpoint.at;
    assert!(region < 80, "the region spans {region} bytes");
    stream.push("```\nAfter.\n");
    let mut rows = stream.take_stable(&ctx);
    rows.extend(stream.finish(&ctx));
    assert_eq!(plain_text(&rows).lines().last(), Some("  After."));
}

#[test]
fn a_resize_rewraps_only_what_has_not_printed() {
    let theme = theme();
    let wide = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let narrow = RenderCtx::new(30, &theme, Clock::still(Instant::now()));
    let first = "The first paragraph is long enough to wrap at thirty columns.\n\n";
    let second = "The second paragraph is also long enough to wrap at thirty.\n\nEnd.\n";
    let mut stream = MarkdownStream::new();
    stream.push(first);
    stream.push(&second[..5]);
    let mut printed = stream.take_stable(&wide);
    stream.push(&second[5..]);
    printed.extend(stream.take_stable(&narrow));
    printed.extend(stream.finish(&narrow));
    let wide_part = whole(first, 80);
    let narrow_all = whole(&format!("{first}{second}"), 30);
    let narrow_rest = &narrow_all[whole(first, 30).len()..];
    let mut expected = wide_part;
    expected.extend(narrow_rest.iter().cloned());
    assert_eq!(plain_text(&printed), plain_text(&expected));
}

#[test]
fn a_long_answer_streams_fast() {
    let mut source = String::new();
    for i in 0..140 {
        source.push_str(&format!(
            "## Part {i}\n\nThis part explains step {i} in a sentence long enough to wrap once or twice at eighty columns, with `code` and **bold**.\n\n- first point\n- second point with more words\n- third point\n\n```rust\nfn part_{i}() -> u32 {{\n    let x = {i};\n    x * 2\n}}\n```\n\n"
        ));
    }
    let lines = source.lines().count();
    assert!(lines >= 2_000, "{lines} lines");
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let started = Instant::now();
    let mut stream = MarkdownStream::new();
    let mut printed = Vec::new();
    for piece in source.as_bytes().chunks(20) {
        stream.push(std::str::from_utf8(piece).expect("ascii"));
        printed.extend(stream.take_stable(&ctx));
        stream.tail(&ctx);
    }
    printed.extend(stream.finish(&ctx));
    let spent = started.elapsed();
    assert_eq!(printed.len(), whole(&source, 80).len());
    // Generous, since tests build without optimizations. A release build takes a fraction.
    assert!(spent < Duration::from_secs(20), "took {spent:?}");
}
