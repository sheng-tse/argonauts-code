use std::time::Instant;

use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

use super::*;
use crate::render::{Clock, RenderCtx};
use crate::text::{plain_text, spans_width};
use crate::theme::{ColorLevel, Glyphs, Role, Theme};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

const STATUS: &str = "\
diff --git a/STATUS.md b/STATUS.md
index 3f2a..9c1b 100644
--- a/STATUS.md
+++ b/STATUS.md
@@ -93,6 +158,8 @@ context
+
 ## Status

-| Part | Status |
+| Area | Status |
 |---|---|
-| Parser | In progress |
-| Docs | Not started |
+| Export, CLI, Tests | Starting now |
+| Everything else | Waiting on review |
";

#[test]
fn unified_diffs_parse_with_their_numbers() {
    let view = DiffView::from_unified(STATUS).expect("parses");
    assert_eq!(view.hunks.len(), 1);
    let rows: Vec<(DiffKind, Option<u32>)> = view.hunks[0]
        .lines
        .iter()
        .map(|line| (line.kind, line.number()))
        .collect();
    use DiffKind::*;
    assert_eq!(
        rows,
        [
            (Added, Some(158)),
            (Context, Some(159)),
            (Context, Some(160)),
            (Removed, Some(95)),
            (Added, Some(161)),
            (Context, Some(162)),
            (Removed, Some(97)),
            (Removed, Some(98)),
            (Added, Some(163)),
            (Added, Some(164)),
        ]
    );
    assert_eq!(view.stats(), (4, 3));
    assert!(!view.is_empty());
}

#[test]
fn bad_diffs_say_where() {
    let error = DiffView::from_unified("@@ -1,2 +1,2 @@\n a\n?b\n").expect_err("bad line");
    assert_eq!(error.line, 3);
    let error = DiffView::from_unified("@@ nonsense @@\n").expect_err("bad header");
    assert_eq!(error.line, 1);
    let error = DiffView::from_unified("just words\n").expect_err("no hunk");
    assert_eq!(error.line, 1);
    assert_eq!(error.to_string(), "diff line 1: no @@ hunk header found");
    assert_eq!(DiffView::from_unified(""), Ok(DiffView::default()));
}

#[test]
fn a_hunk_that_outruns_its_header_keeps_its_lines() {
    let view = DiffView::from_unified("@@ -1 +1 @@\n-old\n+new\n+extra\n").expect("lenient");
    assert_eq!(view.stats(), (2, 1));
    let two = DiffView::from_unified(
        "@@ -1 +1 @@\n-a\n+b\n@@ -10,1 +10,1 @@\n-c\n+d\n\\ No newline at end of file\n",
    )
    .expect("two hunks");
    assert_eq!(two.hunks.len(), 2);
}

#[test]
fn between_numbers_both_sides() {
    let view = DiffView::between("a\nb\nc\nd\n", "a\nB\nc\nd\ne\n", 1);
    let rows: Vec<String> = view
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .map(|line| format!("{:?}{:?}{}", line.kind, line.number(), line.text))
        .collect();
    assert_eq!(
        rows,
        [
            "ContextSome(1)a",
            "RemovedSome(2)b",
            "AddedSome(2)B",
            "ContextSome(3)c",
            "ContextSome(4)d",
            "AddedSome(5)e",
        ]
    );
}

fn render(view: &DiffView, theme: &Theme, width: u16, indent: u16) -> Vec<Line<'static>> {
    let ctx = RenderCtx::new(width, theme, Clock::still(Instant::now()));
    view.render(&ctx, indent)
}

#[test]
fn rows_have_a_gutter_a_marker_and_a_fill() {
    let theme = theme();
    let view = DiffView::from_unified(STATUS).expect("parses");
    let lines = render(&view, &theme, 80, 5);
    assert_eq!(
        plain_text(&lines),
        "      158 +\n      159  ## Status\n      160\n       95 -| Part | Status |\n      161 +| Area | Status |\n      162  |---|---|\n       97 -| Parser | In progress |\n       98 -| Docs | Not started |\n      163 +| Export, CLI, Tests | Starting now |\n      164 +| Everything else | Waiting on review |"
    );
    let removed = &lines[3];
    let removed_bg = theme.style(Role::RemovedRow).bg;
    assert_eq!(removed.style.bg, None);
    assert_eq!(removed.spans[0].content, "     ");
    assert_eq!(removed.spans[0].style.bg, Some(Color::Reset));
    assert_eq!(
        removed.spans[1].style.fg,
        theme.style(Role::RemovedMarker).fg
    );
    assert_eq!(removed.spans[1].style.bg, removed_bg);
    assert!(
        removed.spans[1..]
            .iter()
            .all(|span| span.style.bg.is_some())
    );
    assert_eq!(spans_width(&removed.spans), 73);
    assert_eq!(removed.spans.last().unwrap().style.bg, removed_bg);
    assert_eq!(
        lines[4].spans[1].style.fg,
        theme.style(Role::AddedMarker).fg
    );
    assert_eq!(lines[4].spans[1].style.bg, theme.style(Role::AddedRow).bg);
    assert_eq!(lines[4].spans[4].style.fg, theme.style(Role::DiffText).fg);
    assert_eq!(lines[1].style.bg, None);
    assert_eq!(lines[1].spans[1].style, theme.style(Role::Dim));
}

#[test]
fn changed_words_get_the_brighter_color() {
    let theme = theme();
    let view = DiffView::from_unified(STATUS).expect("parses");
    let lines = render(&view, &theme, 80, 0);
    let highlighted = |line: &Line<'_>, role: Role| -> Vec<String> {
        line.spans
            .iter()
            .filter(|span| span.style.bg == theme.style(role).bg)
            .map(|span| span.content.to_string())
            .collect()
    };
    assert_eq!(highlighted(&lines[3], Role::RemovedWord), ["Part"]);
    assert_eq!(highlighted(&lines[4], Role::AddedWord), ["Area"]);
    assert!(highlighted(&lines[6], Role::RemovedWord).is_empty());
    assert!(highlighted(&lines[8], Role::AddedWord).is_empty());
}

#[test]
fn sixteen_colors_and_no_color_use_text_instead_of_fills() {
    for level in [ColorLevel::Ansi16, ColorLevel::None] {
        let mut theme = theme();
        theme.level = level;
        let view = DiffView::between("the old line\n", "the new line\n", 0);
        let lines = render(&view, &theme, 40, 0);
        assert!(
            lines.iter().all(|line| line.style.bg.is_none()),
            "{level:?}"
        );
        let word = lines[0]
            .spans
            .iter()
            .find(|span| span.content == "old")
            .expect("changed word");
        assert!(word.style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(word.style.fg, theme.style(Role::RemovedMarker).fg);
    }
}

#[test]
fn long_rows_wrap_under_their_text_and_keep_the_fill() {
    let theme = theme();
    let view = DiffView::between("", "abcdefghijklmnopqrstuvwxyz\n", 0);
    let lines = render(&view, &theme, 16, 2);
    assert_eq!(
        plain_text(&lines),
        "   1 +abcdefghi\n      jklmnopqr\n      stuvwxyz"
    );
    for line in &lines {
        assert_eq!(line.spans[1].style.bg, theme.style(Role::AddedRow).bg);
        assert_eq!(spans_width(&line.spans), 15);
    }
}

#[test]
fn code_wraps_where_a_token_ends() {
    let theme = theme();
    let line = "    fn write(&self, out: &mut dyn Write) -> Result<()> {\n";
    let view = DiffView::between("", line, 0);
    let lines = render(&view, &theme, 44, 0);
    let text = plain_text(&lines);
    assert!(!text.contains("Res\n"), "{text}");
    assert!(!text.contains("&m\n"), "{text}");
    assert_eq!(
        text,
        " 1 +    fn write(&self, out: &mut dyn\n    Write) -> Result<()> {"
    );
}

#[test]
fn a_live_preview_cuts_long_rows() {
    let theme = theme();
    let view = DiffView::between("", "abcdefghijklmnopqrstuvwxyz\n", 0);
    let ctx = RenderCtx::new(16, &theme, Clock::still(Instant::now())).with_live(true);
    let lines = view.render(&ctx, 2);
    assert_eq!(plain_text(&lines), "   1 +abcdefgh…");
}

#[test]
fn hunks_are_separated_by_a_dim_gap() {
    let theme = theme();
    let old: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let new = old
        .replace("line 2\n", "line two\n")
        .replace("line 18\n", "line eighteen\n");
    let view = DiffView::between(&old, &new, 1);
    assert_eq!(view.hunks.len(), 2);
    let lines = render(&view, &theme, 40, 0);
    let gap = lines
        .iter()
        .position(|line| plain_text(std::slice::from_ref(line)).trim() == "⋮");
    let gap = gap.expect("a gap row");
    assert_eq!(lines[gap].spans[1].style, theme.style(Role::Dim));
    assert_eq!(plain_text(&lines[gap..=gap]), "  ⋮");
}

#[test]
fn a_diff_cell_hangs_its_rows_under_the_hook() {
    let theme = theme();
    let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
    let cell = DiffCell {
        title: "Update(src/app.rs)".into(),
        diff: DiffView::between("a\n", "b\n", 0),
    };
    let lines = cell.lines(&ctx);
    assert_eq!(
        plain_text(&lines),
        "● Update(src/app.rs)\n  ⎿  Added 1 line, removed 1 line\n      1 -a\n      1 +b"
    );
    assert!(
        lines[1].spans[4]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(lines[1].spans[1].style.bg, Some(Color::Reset));
    let empty = DiffCell {
        title: "Update(x)".into(),
        diff: DiffView::default(),
    };
    assert_eq!(
        plain_text(&empty.lines(&ctx)),
        "● Update(x)\n  ⎿  (No changes)"
    );
}

#[test]
fn a_caption_says_what_changed() {
    let theme = theme();
    let view = DiffView::between("a\nb\n", "a\nc\nd\n", 1).updated("src/app.rs", 2, 1);
    let text: String = view
        .summary(&theme)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(text, "Added 2 lines, removed 1 line");
    let spans = view.summary(&theme);
    let bold: Vec<&str> = spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(bold, ["2", "1"]);
    let only = DiffView::default().updated("a.rs", 0, 3);
    let text: String = only
        .summary(&theme)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(text, "Removed 3 lines");
    let unknown = DiffView::default().updated("a.rs", 0, 0);
    let text: String = unknown
        .summary(&theme)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(text, "Updated a.rs");
    let wrote = DiffView::between("", "hi\n", 0).wrote("hello.txt", 1);
    let text: String = wrote
        .summary(&theme)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(text, "Wrote 1 line to hello.txt");
}

#[test]
fn snippets_lose_their_numbers() {
    let view = DiffView::between("x\n", "y\n", 0).without_numbers();
    assert!(
        view.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .all(|line| line.number().is_none())
    );
}
