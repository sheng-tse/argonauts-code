use std::time::Instant;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Indent, render_markdown};
use crate::render::{Clock, RenderCtx};
use crate::text::{plain_text, spans_width};
use crate::theme::{Glyphs, Role, Theme};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn render_with(source: &str, width: u16, indent: &Indent) -> Vec<Line<'static>> {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    render_markdown(source, &ctx, indent)
}

fn render(source: &str, width: u16) -> String {
    plain_text(&render_with(source, width, &Indent::default()))
}

fn style_of(lines: &[Line<'_>], text: &str) -> Style {
    lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.contains(text))
        .map(|span| span.style)
        .unwrap_or_else(|| panic!("no span holds {text:?}"))
}

fn bullet_indent() -> Indent {
    Indent::new(vec![Span::raw("● ")], vec![Span::raw("  ")])
}

#[test]
fn inline_code_loses_its_backticks_and_takes_the_code_color() {
    let lines = render_with("Run `cargo build` now.", 80, &Indent::default());
    assert_eq!(plain_text(&lines), "Run cargo build now.");
    assert_eq!(style_of(&lines, "cargo build"), theme().style(Role::Code));
}

#[test]
fn code_blocks_drop_their_fences_and_sit_at_the_text_column() {
    let source = "To try it:\n\n```bash\ncd /tmp && echo \"hi\"\n```\n\nDone.";
    let lines = render_with(source, 80, &bullet_indent());
    assert_eq!(
        plain_text(&lines),
        "● To try it:\n\n  cd /tmp && echo \"hi\"\n\n  Done."
    );
    assert_eq!(style_of(&lines, "cd"), theme().style(Role::SyntaxCommand));
    assert_eq!(style_of(&lines, "hi"), theme().style(Role::SyntaxString));
}

#[test]
fn a_fence_without_a_language_takes_the_code_color() {
    let theme = theme();
    let lines = render_with("```\n  npm run build\n\n```", 80, &bullet_indent());
    assert_eq!(plain_text(&lines), "●   npm run build\n");
    let row = &lines[0];
    assert_eq!(
        row.spans.last().map(|s| s.content.as_ref()),
        Some("npm run build")
    );
    assert_eq!(
        row.spans.last().map(|s| s.style),
        Some(theme.style(Role::Code))
    );
    assert_eq!(style_of(&lines, "  ").fg, theme.style(Role::Text).fg);
    for source in ["```klingon\nnuqneH\n```", "    nuqneH"] {
        let lines = render_with(source, 80, &Indent::default());
        assert_eq!(
            style_of(&lines, "nuqneH"),
            theme.style(Role::Text),
            "{source}"
        );
    }
}

#[test]
fn nested_numbered_lists_count_with_letters_then_roman_numerals() {
    let source = "1. one\n   1. first\n   2. second\n      1. deep\n      2. deeper\n2. two\n   - bullet\n     1. under a bullet";
    assert_eq!(
        render(source, 40),
        "1. one\n   a. first\n   b. second\n      i. deep\n      ii. deeper\n2. two\n   - bullet\n     i. under a bullet"
    );
    assert_eq!(render("- item\n  > 1. quoted", 40), "- item\n  ▎ 1. quoted");
}

#[test]
fn long_code_lines_break_anywhere_under_the_same_indent() {
    let source = "```\nabcdefghijklmnopqrstuvwxyz\n```";
    assert_eq!(
        plain_text(&render_with(source, 12, &bullet_indent())),
        "● abcdefghij\n  klmnopqrst\n  uvwxyz"
    );
}

#[test]
fn lists_hang_under_their_text() {
    let source = "Still waiting on you:\n- The export follow-ups. The main ones are the missing headers.\n- The schema.\n\n1. First step, which is long enough to wrap.\n2. Second.";
    assert_eq!(
        render(source, 40),
        "Still waiting on you:\n\
         - The export follow-ups. The main ones\n  are the missing headers.\n\
         - The schema.\n\n\
         1. First step, which is long enough to\n   wrap.\n\
         2. Second."
    );
}

#[test]
fn nested_lists_indent_under_the_parent_text() {
    let source = "- outer item\n  - inner item that wraps around\n    - deepest\n- back out";
    assert_eq!(
        render(source, 24),
        "- outer item\n  - inner item that\n    wraps around\n    - deepest\n- back out"
    );
}

#[test]
fn loose_lists_keep_the_source_spacing() {
    assert_eq!(render("- a\n\n- b\n- c", 40), "- a\n\n- b\n- c");
    assert_eq!(
        render("- a\n\n  second paragraph\n- b", 40),
        "- a\n\n  second paragraph\n- b"
    );
}

#[test]
fn task_items_show_boxes() {
    let lines = render_with("- [ ] todo item\n- [x] done item", 40, &Indent::default());
    assert_eq!(plain_text(&lines), "- ☐ todo item\n- ☒ done item");
    assert_eq!(style_of(&lines, "☒"), theme().style(Role::Success));
    assert_eq!(
        render("- [ ] a task long enough to wrap", 16),
        "- ☐ a task long\n    enough to\n    wrap"
    );
}

#[test]
fn soft_breaks_are_line_breaks() {
    assert_eq!(render("one\ntwo\nthree", 80), "one\ntwo\nthree");
    assert_eq!(render("hard  \nbreak", 80), "hard\nbreak");
}

#[test]
fn blocks_are_separated_where_the_source_separates_them() {
    assert_eq!(
        render("para one\n\n\n\npara two", 80),
        "para one\n\npara two"
    );
    assert_eq!(render("text\n```\ncode\n```\nmore", 80), "text\ncode\nmore");
    assert_eq!(render("intro:\n- item", 80), "intro:\n- item");
}

#[test]
fn a_heading_always_has_a_blank_row_after_it() {
    assert_eq!(render("# Title\ntext", 80), "Title\n\ntext");
    assert_eq!(render("# Title\n\ntext", 80), "Title\n\ntext");
    assert_eq!(render("## One\n## Two\n- item", 80), "One\n\nTwo\n\n- item");
    assert_eq!(render("Setext\n---\ntext", 80), "Setext\n\ntext");
    assert_eq!(
        render("- # In an item\n- next", 80),
        "- In an item\n\n- next"
    );
    assert_eq!(render("text\n# Last", 80), "text\nLast");
}

#[test]
fn emphasis_and_headings_use_modifiers() {
    let lines = render_with(
        "## The **plan**\n\n*soft* and ~~gone~~",
        80,
        &Indent::default(),
    );
    assert_eq!(plain_text(&lines), "The plan\n\nsoft and gone");
    let heading = style_of(&lines, "The").add_modifier;
    assert!(heading.contains(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED));
    let top = render_with("# Top", 80, &Indent::default());
    let top = style_of(&top, "Top").add_modifier;
    assert!(top.contains(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED));
    assert!(
        style_of(&lines, "soft")
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert!(
        style_of(&lines, "gone")
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
}

#[test]
fn blockquotes_get_a_dim_bar_and_italics() {
    let lines = render_with("> quoted\n>\n> again", 80, &Indent::default());
    assert_eq!(plain_text(&lines), "▎ quoted\n▎\n▎ again");
    assert_eq!(lines[0].spans[0].style, theme().style(Role::Dim));
    assert!(
        style_of(&lines, "quoted")
            .add_modifier
            .contains(Modifier::ITALIC)
    );
}

#[test]
fn rules_fill_the_row() {
    assert_eq!(
        render("above\n\n---\n\nbelow", 10),
        "above\n\n──────────\n\nbelow"
    );
}

#[test]
fn links_show_their_url_only_when_it_adds_something() {
    let lines = render_with(
        "See [the docs](https://docs.rs) or <https://crates.io>.",
        80,
        &Indent::default(),
    );
    assert_eq!(
        plain_text(&lines),
        "See the docs (https://docs.rs) or https://crates.io."
    );
    assert!(
        style_of(&lines, "the docs")
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert_eq!(
        style_of(&lines, "(https://docs.rs)"),
        theme().style(Role::Dim)
    );
}

#[test]
fn tables_draw_in_a_box_and_stack_as_records_when_narrow() {
    let source = "| Stream | Status |\n|---|---:|\n| Contracts | Starting now |\n| Docs | 3 |";
    let lines = render_with(source, 60, &Indent::default());
    assert_eq!(
        plain_text(&lines),
        "┌───────────┬──────────────┐\n│ Stream    │       Status │\n├───────────┼──────────────┤\n│ Contracts │ Starting now │\n│ Docs      │            3 │\n└───────────┴──────────────┘"
    );
    assert!(
        style_of(&lines, "Stream")
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(style_of(&lines, "┌───"), theme().style(Role::Dim));
    let narrow = render(
        "| Name | Notes |\n|---|---|\n| supercalifragilistic | expialidocious |\n| agy | ok |",
        20,
    );
    assert_eq!(
        narrow,
        "Name:\n  supercalifragilist\n  ic\nNotes:\n  expialidocious\n\nName: agy\nNotes: ok"
    );
}

#[test]
fn tables_wrap_cells_before_giving_up() {
    let source = "| Name | Notes |\n|---|---|\n| agy | not installed on this machine yet |";
    assert_eq!(
        render(source, 28),
        "┌──────┬──────────────────┐\n│ Name │ Notes            │\n├──────┼──────────────────┤\n│ agy  │ not installed on │\n│      │ this machine yet │\n└──────┴──────────────────┘"
    );
}

#[test]
fn escapes_in_the_source_never_reach_a_line() {
    let lines = render_with(
        "evil \x1b[2J\x1b]0;title\x07text\n\n```\n\x1b[31mred\x1b[0m\n```",
        80,
        &Indent::default(),
    );
    let text = plain_text(&lines);
    assert_eq!(text, "evil text\n\nred");
    assert!(!text.contains('\x1b'));
}

#[test]
fn no_row_is_wider_than_the_width() {
    let source = "# A heading that is long\n\nSome **bold** text with `code` and a [link](https://example.com/a/very/long/path).\n\n- list item with words\n  - nested item with more words\n\n> quote that goes on and on\n\n```rust\nfn main() { println!(\"a long line of code that must hard wrap\"); }\n```\n\n| a | b |\n|---|---|\n| one two three | four five six |";
    for width in [1u16, 2, 3, 5, 8, 13, 21, 40, 80] {
        for line in render_with(source, width, &bullet_indent()) {
            assert!(
                spans_width(&line.spans) <= usize::from(width),
                "{width}: {line:?}"
            );
        }
    }
}

#[test]
fn a_typical_answer_renders_like_claude_code() {
    let source = "The flag is in and the tests pass:\n\n```console\n$ todo export --json\n{\"items\": 3, \"done\": 1}\n```\n\nStill waiting on you:\n- The 2 `--json` questions. \"Keep the CSV default\" works.\n- The review follow-ups. The main ones are the missing schema, the date format in the output, and whether empty fields should print as null in the JSON export.\n\n## Next\n\n1. Answer the questions.\n2. Merge the **branch**.";
    for width in [80u16, 40] {
        let text = plain_text(&render_with(source, width, &bullet_indent()));
        insta::assert_snapshot!(format!("typical_answer_{width}"), text);
    }
}
