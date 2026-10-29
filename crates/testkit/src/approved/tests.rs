use std::fs;

use super::*;
use crate::screen::Grid;

const MOON: Palette = Palette::TOKYONIGHT_MOON;

/// Dim's colour in the app's dark theme, which TokyoNight Moon also has as its colour 7.
const DIM: &str = "#828bb8";

const MOON_TERMINAL: &str = "terminal fg=#c8d3f5 bg=#222436";
const LIGHT_TERMINAL: &str = "terminal fg=#000000 bg=#ffffff";

fn parsed(name: &str, text: &str) -> Drawing {
    match Drawing::parse(name, text) {
        Ok(drawing) => drawing,
        Err(err) => panic!("{err:#}"),
    }
}

fn parse_error(name: &str, text: &str) -> String {
    match Drawing::parse(name, text) {
        Ok(_) => panic!("{text:?} reads"),
        Err(err) => format!("{err:#}"),
    }
}

fn found(drawing: &Drawing, drawn: &Grid) -> Vec<(u16, u16, Aspect)> {
    differences(drawing, drawn, &MOON, &|_, _| false)
        .into_iter()
        .map(|d| (d.row, d.col, d.aspect))
        .collect()
}

/// A one-row drawing of a whole screen that size, on the TokyoNight Moon terminal.
fn row_drawing(size: &str, text: &str, paint: &str, keys: &str) -> String {
    format!(
        "One row\nscreen {size}\nat 0,0\nsize {size}\n{MOON_TERMINAL}\ntext\n{text}\npaint\n\
         {paint}\nkey\n{keys}"
    )
}

/// Two rows of a 12 × 3 screen from its second: a header in Dim and bold text.
const TWO_ROWS: &str = "\
Limits: the head of the tab
screen 12x3
at 1,0
size 12x2
terminal fg=#c8d3f5 bg=#222436
text
Limits  ok
  Codex
paint
dddddd..bb
..bbbbb
key
. blank
d fg=#828bb8  ; Dim
b fg=#c8d3f5 bold  ; the terminal's own, Bright
";

/// The two rows as the app draws them, from the screen's row `top`.
fn two_rows_from(top: usize) -> String {
    "\r\n".repeat(top) + "\x1b[38;2;130;139;184mLimits\x1b[0m  \x1b[1mok\r\n  Codex\x1b[0m"
}

fn two_rows_screen() -> Grid {
    Grid::from_bytes(12, 3, two_rows_from(1).as_bytes())
}

#[test]
fn a_drawing_reads_its_header_rows_and_keys() {
    let drawing = parsed("limits@12x3.txt", TWO_ROWS);
    assert_eq!(drawing.title, "Limits: the head of the tab");
    assert_eq!((drawing.cols, drawing.rows), (12, 3));
    assert_eq!(
        drawing.at,
        At {
            row: Some(1),
            col: Some(0)
        }
    );
    assert_eq!((drawing.width, drawing.height), (12, 2));
    assert_eq!(drawing.terminal, MOON.terminal());
    let (cell, paint) = drawing.wanted(1, 2).unwrap();
    assert_eq!(cell.text, 'C');
    assert_eq!(paint.to_string(), "fg=#c8d3f5 bold");
    assert!(drawing.wanted(2, 0).is_none(), "it has two rows");
    assert_eq!(drawing.line(1), Some("  Codex"));
    let names: Vec<String> = drawing.names().iter().map(Named::to_string).collect();
    assert_eq!(names, ["#c8d3f5 Bright", "#828bb8 Dim"]);
}

#[test]
fn each_header_line_is_read_and_checked() {
    let reordered = TWO_ROWS.replace(
        "screen 12x3\nat 1,0\nsize 12x2\nterminal fg=#c8d3f5 bg=#222436\n",
        "terminal fg=#c8d3f5 bg=#222436\nsize 12x2\nat 1,0\nscreen 12x3\n",
    );
    assert_eq!(
        parsed("limits@12x3.txt", &reordered),
        parsed("limits@12x3.txt", TWO_ROWS)
    );
    let unknown = parsed("limits@12x3.txt", &TWO_ROWS.replace("at 1,0", "at ?,?"));
    assert_eq!(
        unknown.at,
        At {
            row: None,
            col: None
        }
    );
    let cases = [
        ("screen 12x3\n", "", "gives no screen line"),
        (
            "screen 12x3",
            "screen 12x4",
            "named for a 12x3 screen, but it says screen 12x4",
        ),
        ("at 1,0\n", "", "gives no at line"),
        ("at 1,0", "at 1", "\"1\" isn't <row>,<col>"),
        ("at 1,0", "at x,0", "\"x\" isn't a number or ?"),
        (
            "at 1,0",
            "at 2,0",
            "a 12x2 drawing at 2,0 doesn't fit the 12x3 screen",
        ),
        ("at 1,0\n", "at 1,0\nat 1,0\n", "at is given twice"),
        ("size 12x2\n", "", "gives no size line"),
        ("size 12x2", "size 0x2", "\"0x2\" isn't <cols>x<rows>"),
        ("size 12x2", "size 13x2", "doesn't fit the 12x3 screen"),
        (
            "terminal fg=#c8d3f5 bg=#222436\n",
            "",
            "gives no terminal line",
        ),
        (
            MOON_TERMINAL,
            "terminal fg=#c8d3f5",
            "expected fg=#rrggbb bg=#rrggbb",
        ),
        (
            MOON_TERMINAL,
            "terminal fg=#c8d3f5 bg=222436",
            "\"222436\" isn't a colour",
        ),
        (
            "size 12x2\n",
            "size 12x2\ncolours dark\n",
            "expected screen, at, size or terminal",
        ),
        ("\npaint\n", "\n", "where the rows end and \"paint\" starts"),
        ("\nkey\n", "\n", "where the rows end and \"key\" starts"),
    ];
    for (from, to, says) in cases {
        let text = TWO_ROWS.replacen(from, to, 1);
        let err = parse_error("limits@12x3.txt", &text);
        assert!(err.contains(says), "{from:?} as {to:?}: {err}");
    }
    let err = parse_error("limits.txt", TWO_ROWS);
    assert!(
        err.contains("isn't named <state>@<cols>x<rows>.txt"),
        "{err}"
    );
    let err = parse_error("limits@12x3.txt", "Limits\nscreen 12x3\n");
    assert!(err.contains("ends before its text"), "{err}");
}

#[test]
fn keys_of_every_kind_are_read() {
    let keys = "\
. blank
? the character alone
f fg=#ff757f italic  ; Error
g fg=#c3e88d bg=#2f334d bold  ; Success, on Band
h bg=#2f334d  ; Band
L logo=codex
N logo=claudecode-2
P picture=map
";
    let covered = "\u{FFFC}".repeat(5);
    let text = row_drawing("10x1", &format!("ab {covered}z"), "fghLLNNP?", keys);
    let drawing = parsed("keys@10x1.txt", &text);
    let paints: Vec<String> = (0..10)
        .map(|col| drawing.wanted(0, col).unwrap().1.to_string())
        .collect();
    assert_eq!(
        paints,
        [
            "fg=#ff757f italic",
            "fg=#c3e88d bg=#2f334d bold",
            "bg=#2f334d",
            "logo=codex",
            "logo=codex",
            "logo=claudecode-2",
            "logo=claudecode-2",
            "picture=map",
            "the character alone",
            "blank",
        ]
    );
    assert_eq!(
        drawing.wanted(0, 5).unwrap().1,
        &Paint::Logo {
            stem: "claudecode".to_owned(),
            number: Some(2)
        }
    );
    let names: Vec<String> = drawing.names().iter().map(Named::to_string).collect();
    assert_eq!(
        names,
        [
            "#ff757f Error",
            "#c3e88d Success",
            "#2f334d Band",
            "#2f334d Band"
        ]
    );
}

#[test]
fn a_key_that_doesnt_fit_its_cells_is_refused() {
    let white = ". blank\nx fg=#ffffff\n";
    let cases = [
        (
            "ab",
            "xx",
            ". blank\nx Dim 828bb8\n",
            "\"Dim\" isn't fg=#rrggbb",
        ),
        (
            "ab",
            "xx",
            ". blank\nx fg=828bb8\n",
            "\"828bb8\" isn't a colour",
        ),
        (
            "ab",
            "xx",
            ". blank\nx fg=#ffffff fg=#000000\n",
            "\"fg=#000000\" is given twice",
        ),
        (
            "\u{FFFC}\u{FFFC}",
            "xx",
            ". blank\nx logo=nobody\n",
            "no logo is named \"nobody\"",
        ),
        (
            "",
            "x",
            ". blank\nx bg=#2f334d bold\n",
            "no weight or slant",
        ),
        (
            "",
            "x",
            ". blank\nx picture=\n",
            "picture= names one picture",
        ),
        ("ab", "xx", ". fg=#ffffff\n", "'.' is `. blank`"),
        (
            "ab",
            "xx",
            ". blank\n? fg=#ffffff\n",
            "'?' is `? the character alone`",
        ),
        ("ab", "xx", ". blank\nx blank\n", "only '.' is blank"),
        (
            "ab",
            "xx",
            ". blank\nx\n",
            "a key is one character, then a space",
        ),
        ("ab", "xx", ". blank\n\n", "a key line starts with its key"),
        (
            "ab",
            "xx",
            ". blank\nx fg=#ffffff\nx fg=#000000\n",
            "'x' is given twice",
        ),
        ("ab", "xy", white, "'y' is painted but never given"),
        (
            "ab",
            "xx",
            ". blank\nx fg=#ffffff\ny fg=#000000\n",
            "no cell is painted with the keys ['y']",
        ),
        ("ab", "xx", "x fg=#ffffff\n", "gives no `. blank`"),
        (" b", "xx", white, "' ' can't be painted fg=#ffffff"),
        (
            "ab",
            "LL",
            ". blank\nL logo=codex\n",
            "'a' can't be painted logo=codex",
        ),
        ("ab", "", ". blank\n", "'a' can't be painted blank"),
        (
            "\u{FFFC}b",
            "?x",
            ". blank\n? the character alone\nx fg=#ffffff\n",
            "can't be painted the character alone",
        ),
        ("abcde", "", ". blank\n", "5 characters for 4 cells"),
        ("ab", "xxxxx", white, "5 keys for 4 cells"),
        ("界", "x", white, "'界' isn't one cell wide"),
        ("e\u{301}", "xx", white, "isn't one cell wide"),
        ("a\u{7}", "x", white, "isn't a character a screen shows"),
    ];
    for (text, paint, keys, says) in cases {
        let file = row_drawing("4x1", text, paint, keys);
        let err = parse_error("small@4x1.txt", &file);
        assert!(err.contains(says), "{text:?} {paint:?} {keys:?}: {err}");
    }
}

#[test]
fn short_rows_are_blanks_on_the_terminals_background() {
    let drawing = parsed("limits@12x3.txt", TWO_ROWS);
    let (cell, paint) = drawing.wanted(1, 10).unwrap();
    assert_eq!((cell.text, paint), (' ', &Paint::Blank));
    let mut screen = two_rows_screen();
    assert_eq!(found(&drawing, &screen), []);
    screen.cell_mut(2, 10).background = Colour::Rgb(Rgb(1, 2, 3));
    screen.cell_mut(2, 11).text = "x".to_owned();
    assert_eq!(
        found(&drawing, &screen),
        [(1, 10, Aspect::Background), (1, 11, Aspect::Text)]
    );
}

#[test]
fn a_known_region_is_compared_where_its_at_line_puts_it() {
    let drawing = parsed("limits@12x3.txt", TWO_ROWS);
    let mut screen = two_rows_screen();
    assert_eq!(
        drawing.place(&screen),
        Placement::At(Region {
            top: 1,
            left: 0,
            rows: 2,
            cols: 12
        })
    );
    assert_eq!(found(&drawing, &screen), []);
    screen.cell_mut(1, 2).text = "n".to_owned();
    screen.cell_mut(1, 4).ink = Colour::Indexed(4);
    screen.cell_mut(2, 3).bold = false;
    screen.cell_mut(2, 9).background = Colour::Rgb(Rgb(1, 2, 3));
    // The drawings draw no underlines, so none is compared.
    screen.cell_mut(2, 4).underline = true;
    assert_eq!(
        found(&drawing, &screen),
        [
            (0, 2, Aspect::Text),
            (0, 4, Aspect::TextColour),
            (1, 3, Aspect::Bold),
            (1, 9, Aspect::Background),
        ]
    );
    // A known place is held even where the screen shows the text elsewhere.
    let higher = Grid::from_bytes(12, 3, two_rows_from(0).as_bytes());
    assert!(found(&drawing, &higher).len() > 10);
}

#[test]
fn a_drawing_at_an_unknown_row_is_found_by_its_text() {
    let drawing = parsed(
        "limits@12x5.txt",
        &TWO_ROWS
            .replace("screen 12x3", "screen 12x5")
            .replace("at 1,0", "at ?,0"),
    );
    let mut screen = Grid::from_bytes(12, 5, two_rows_from(2).as_bytes());
    let region = Region {
        top: 2,
        left: 0,
        rows: 2,
        cols: 12,
    };
    assert_eq!(drawing.place(&screen), Placement::At(region));
    assert_eq!(found(&drawing, &screen), []);
    // Found by its text, it is then held to its colours there.
    screen.cell_mut(3, 2).bold = false;
    assert_eq!(drawing.place(&screen), Placement::At(region));
    assert_eq!(found(&drawing, &screen), [(1, 2, Aspect::Bold)]);
}

/// "Codex" alone, in bold, anywhere on a 12 × 3 screen.
const CODEX_ANYWHERE: &str = "\
Codex alone
screen 12x3
at ?,?
size 5x1
terminal fg=#c8d3f5 bg=#222436
text
Codex
paint
bbbbb
key
. blank
b fg=#c8d3f5 bold
";

#[test]
fn a_drawing_at_an_unknown_row_and_column_is_found_by_its_text() {
    let drawing = parsed("codex@12x3.txt", CODEX_ANYWHERE);
    let screen = two_rows_screen();
    assert_eq!(
        drawing.place(&screen),
        Placement::At(Region {
            top: 2,
            left: 2,
            rows: 1,
            cols: 5
        })
    );
    assert_eq!(found(&drawing, &screen), []);
}

#[test]
fn a_drawing_found_twice_or_nowhere_says_so() {
    let drawing = parsed("codex@12x3.txt", CODEX_ANYWHERE);
    let twice = Grid::from_bytes(12, 3, b"\x1b[1mCodex Codex");
    let placement = drawing.place(&twice);
    let lefts = match &placement {
        Placement::Many { first, more } => {
            let mut lefts = vec![first.left];
            lefts.extend(more.iter().map(|region| region.left));
            lefts
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(lefts, [0, 6]);
    let differences = differences_at(&drawing, &placement, &twice, &MOON, &|_, _| false);
    let aspects: Vec<Aspect> = differences.iter().map(|d| d.aspect).collect();
    assert_eq!(aspects, [Aspect::Place]);
    let text = report("codex@12x3.txt", &drawing, &twice, &placement, &differences);
    assert!(
        text.contains(
            "the drawing's text is at 2 places on the 12 × 3 screen, where it must be at one: \
             rows 0-0, cols 0-4; rows 0-0, cols 6-10."
        ),
        "{text}"
    );

    let near = Grid::from_bytes(12, 3, b"\r\n\r\n   \x1b[1mCodez");
    let placement = drawing.place(&near);
    assert_eq!(
        placement,
        Placement::Nowhere(Region {
            top: 2,
            left: 3,
            rows: 1,
            cols: 5
        })
    );
    let differences = differences_at(&drawing, &placement, &near, &MOON, &|_, _| false);
    let cells: Vec<(u16, u16, Aspect)> = differences
        .iter()
        .map(|d| (d.row, d.col, d.aspect))
        .collect();
    assert_eq!(cells, [(0, 0, Aspect::Place), (0, 4, Aspect::Text)]);
    let text = report("codex@12x3.txt", &drawing, &near, &placement, &differences);
    assert!(
        text.contains(
            "the drawing's text is nowhere on the 12 × 3 screen. Where it comes closest, at \
             rows 2-2, cols 3-7, 1 cells are different."
        ),
        "{text}"
    );
    assert!(
        text.contains("row 0, col 4: text: the drawing has \"x\", the screen \"z\""),
        "{text}"
    );
}

#[test]
fn a_cell_painted_whole_in_one_colour_matches_a_blank_or_a_full_block_on_it() {
    let keys = ". blank\nb fg=#82aaff\nf bg=#82aaff\ns fg=#828bb8 bg=#2f334d\n";
    let drawing = parsed("bar@4x1.txt", &row_drawing("4x1", "█ ▓a", "bfsb", keys));
    let blue = "\x1b[38;2;130;170;255m█\x1b[0m";
    let on_blue = "\x1b[48;2;130;170;255m \x1b[0m";
    // ▓ paints three quarters of Dim over the band behind it.
    let shade = Rgb::parse(DIM).unwrap().over(Rgb(0x2f, 0x33, 0x4d), 0.75);
    let on_shade = format!("\x1b[48;2;{};{};{}m \x1b[0m", shade.0, shade.1, shade.2);
    let swapped = format!("{on_blue}{blue}{on_shade}\x1b[38;2;130;170;255ma");
    let screen = Grid::from_bytes(4, 1, swapped.as_bytes());
    assert_eq!(found(&drawing, &screen), []);
    let mut other = screen.clone();
    other.cell_mut(0, 0).background = Colour::Rgb(Rgb(0x82, 0xaa, 0xfe));
    other.cell_mut(0, 1).text = "▓".to_owned();
    assert_eq!(
        found(&drawing, &other),
        [(0, 0, Aspect::Background), (0, 1, Aspect::Background)]
    );
    // A glyph is never a whole cell of colour.
    let mut letter = screen.clone();
    letter.cell_mut(0, 0).text = "x".to_owned();
    assert_eq!(found(&drawing, &letter), [(0, 0, Aspect::Text)]);
}

#[test]
fn colours_compare_as_painted_not_as_written() {
    let keys = ". blank\nd fg=#828bb8\nf fg=#c8d3f5\n";
    let drawing = parsed("dim@4x1.txt", &row_drawing("4x1", "abcd", "ddff", keys));
    // Dim as colour 7 of the theme and spelled out; the text colour as the terminal's own.
    let mut screen = Grid::from_bytes(4, 1, b"\x1b[37ma\x1b[38;2;130;139;184mb\x1b[0mcd");
    assert_eq!(found(&drawing, &screen), []);
    // Faint text keeps half its colour over the background, which no longer paints Dim.
    screen.cell_mut(0, 0).dim = true;
    assert_eq!(found(&drawing, &screen), [(0, 0, Aspect::TextColour)]);
    // Inverse text paints its colour behind it.
    let inverse = Grid::from_bytes(1, 1, b"\x1b[7m \x1b[0m");
    let behind = parsed(
        "inverse@1x1.txt",
        &row_drawing("1x1", "", "b", ". blank\nb bg=#c8d3f5\n"),
    );
    assert_eq!(found(&behind, &inverse), []);
}

#[test]
fn the_terminal_line_gives_the_unstyled_cells_colours() {
    let dark_text = row_drawing("3x1", "ab", "ii", ". blank\ni fg=#000000\n");
    let light = parsed(
        "light@3x1.txt",
        &dark_text.replace(MOON_TERMINAL, LIGHT_TERMINAL),
    );
    let dark = parsed("dark@3x1.txt", &dark_text);
    let unstyled = Grid::from_bytes(3, 1, b"ab");
    assert_eq!(found(&light, &unstyled), []);
    assert_eq!(
        found(&dark, &unstyled),
        [(0, 0, Aspect::TextColour), (0, 1, Aspect::TextColour)]
    );
    let black_on_white = Grid::from_bytes(3, 1, b"\x1b[38;2;0;0;0;48;2;255;255;255mab ");
    assert_eq!(found(&light, &black_on_white), []);
    assert_eq!(
        found(&dark, &black_on_white),
        [
            (0, 0, Aspect::Background),
            (0, 1, Aspect::Background),
            (0, 2, Aspect::Background)
        ]
    );
}

#[test]
fn a_wide_glyph_on_the_screen_takes_two_drawn_cells() {
    let drawing = parsed(
        "wide@4x1.txt",
        &row_drawing("4x1", "xyab", "????", ". blank\n? the character alone\n"),
    );
    let screen = Grid::from_bytes(4, 1, "界ab".as_bytes());
    assert_eq!(
        found(&drawing, &screen),
        [(0, 0, Aspect::Text), (0, 1, Aspect::Text)]
    );
}

/// Codex's logo as the app draws its picture's two placeholders.
const CODEX_MARK: &str =
    "\x1b[38;2;76;79;2m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\x1b[39m";

/// The id of Codex's logo on a dark screen, which its placeholders' colour names.
const CODEX: u32 = 0x4c_4f02;

/// Picture `id` and a virtual placement for its placeholders, as the app sends a logo.
fn picture_sent(id: u32) -> String {
    format!(
        "\x1b_Ga=t,f=100,q=2,i={id},m=0;iVBORw0KGgo=\x1b\\\
         \x1b_Ga=p,U=1,i={id},c=2,r=1,q=2\x1b\\"
    )
}

/// Codex's logo, sent and then drawn, then its name.
fn codex_logo() -> String {
    format!("{}{CODEX_MARK} Codex", picture_sent(CODEX))
}

/// A logo, then its name in characters alone.
fn logo_drawing(logo: &str) -> String {
    let keys = format!(". blank\n? the character alone\nL logo={logo}\n");
    row_drawing("8x1", "\u{FFFC}\u{FFFC} Codex", "LL.?????", &keys)
}

#[test]
fn a_logo_is_its_pictures_two_halves() {
    let screen = Grid::from_bytes(8, 1, codex_logo().as_bytes());
    let drawing = parsed("logo@8x1.txt", &logo_drawing("codex"));
    assert_eq!(found(&drawing, &screen), []);
    let claude = parsed("logo@8x1.txt", &logo_drawing("claudecode"));
    let changes = differences(&claude, &screen, &MOON, &|_, _| false);
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].drawing, "the claudecode logo's left half");
    assert_eq!(changes[0].screen, "the codex logo's left half");
    let dot = Grid::from_bytes(8, 1, "● Codex".as_bytes());
    let changes = differences(&drawing, &dot, &MOON, &|_, _| false);
    assert_eq!(changes[0].screen, "\"●\"");
    let halves_swapped = CODEX_MARK.replace(
        "\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}",
        "\u{0305}\u{030D}\u{10EEEE}\u{0305}\u{0305}",
    );
    let swapped = format!("{}{halves_swapped} Codex", picture_sent(CODEX));
    let swapped = Grid::from_bytes(8, 1, swapped.as_bytes());
    assert_eq!(
        found(&drawing, &swapped),
        [(0, 0, Aspect::Picture), (0, 1, Aspect::Picture)]
    );
}

#[test]
fn a_logos_ink_for_light_screens_shows_only_on_a_light_terminal() {
    let light_ink = format!(
        "{}{} Codex",
        picture_sent(CODEX + 1),
        CODEX_MARK.replace("76;79;2m", "76;79;3m")
    );
    let screen = Grid::from_bytes(8, 1, light_ink.as_bytes());
    let dark = logo_drawing("codex");
    let changes = differences(&parsed("logo@8x1.txt", &dark), &screen, &MOON, &|_, _| {
        false
    });
    assert_eq!(changes.len(), 2);
    assert_eq!(
        changes[0].screen,
        "the codex logo's left half, in its ink for light screens"
    );
    let light = dark.replace(MOON_TERMINAL, LIGHT_TERMINAL);
    assert_eq!(found(&parsed("logo@8x1.txt", &light), &screen), []);
}

#[test]
fn a_logo_sent_before_the_alternate_screen_is_blank_on_it() {
    let drawing = parsed("logo@8x1.txt", &logo_drawing("codex"));
    let before = format!("{}\x1b[?1049h\x1b[H{CODEX_MARK} Codex", picture_sent(CODEX));
    let screen = Grid::from_bytes(8, 1, before.as_bytes());
    let changes = differences(&drawing, &screen, &MOON, &|_, _| false);
    assert_eq!(
        found(&drawing, &screen),
        [(0, 0, Aspect::Picture), (0, 1, Aspect::Picture)]
    );
    assert_eq!(
        changes[0].screen,
        "the codex logo's left half, blank since its screen doesn't hold the picture"
    );
    let written = drawing_text(&screen, Region::whole(8, 1), &MOON, &[]).unwrap();
    assert!(!written.contains("logo="), "{written}");
    assert!(written.contains("\ntext\n   Codex\n"), "{written}");

    let sent_again = format!(
        "{}\x1b[?1049h{}\x1b[H{CODEX_MARK} Codex",
        picture_sent(CODEX),
        picture_sent(CODEX)
    );
    assert_eq!(
        found(&drawing, &Grid::from_bytes(8, 1, sent_again.as_bytes())),
        []
    );
}

#[test]
fn a_picture_cell_holds_a_picture_that_isnt_a_logo() {
    let drawing = parsed(
        "map@2x1.txt",
        &row_drawing("2x1", "\u{FFFC}\u{FFFC}", "PP", ". blank\nP picture=map\n"),
    );
    let map = format!(
        "{}\x1b[38;2;18;52;86m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}",
        picture_sent(0x12_3456)
    );
    assert_eq!(found(&drawing, &Grid::from_bytes(2, 1, map.as_bytes())), []);
    let logo = format!("{}{CODEX_MARK}", picture_sent(CODEX));
    let logo = Grid::from_bytes(2, 1, logo.as_bytes());
    assert_eq!(
        found(&drawing, &logo),
        [(0, 0, Aspect::Picture), (0, 1, Aspect::Picture)]
    );
    let words = Grid::from_bytes(2, 1, b"ab");
    let changes = differences(&drawing, &words, &MOON, &|_, _| false);
    assert_eq!(changes[0].drawing, "the picture map");
    assert_eq!(changes[0].screen, "\"a\"");
}

#[test]
fn a_character_alone_is_compared_whatever_its_colours() {
    let drawing = parsed(
        "typed@3x1.txt",
        &row_drawing("3x1", "a█", "???", ". blank\n? the character alone\n"),
    );
    let coloured = Grid::from_bytes(3, 1, "\x1b[1;31;44ma█\x1b[42m ".as_bytes());
    assert_eq!(found(&drawing, &coloured), []);
    let other = Grid::from_bytes(3, 1, "b \x1b[44m█".as_bytes());
    assert_eq!(
        found(&drawing, &other),
        [
            (0, 0, Aspect::Text),
            (0, 1, Aspect::Text),
            (0, 2, Aspect::Text)
        ]
    );
}

#[test]
fn a_screen_written_as_a_drawing_reads_back_as_itself() {
    let screen = Grid::from_bytes(
        10,
        2,
        format!(
            "{}\r\n\x1b[1;38;2;130;139;184mab ▓\x1b[0m \x1b[4;48;5;24mx\x1b[0m",
            codex_logo()
        )
        .as_bytes(),
    );
    let names = [Named {
        name: Some("Dim".to_owned()),
        rgb: Rgb::parse(DIM).unwrap(),
    }];
    let text = drawing_text(&screen, Region::whole(10, 2), &MOON, &names).unwrap();
    assert!(text.contains("\nat 0,0\nsize 10x2\n"), "{text}");
    assert!(text.contains(" fg=#828bb8 bold  ; Dim\n"), "{text}");
    assert!(
        text.contains(" fg=#c8d3f5 bg=#005f87  ; the terminal's own\n"),
        "{text}"
    );
    assert!(text.contains(" logo=codex\n"), "{text}");
    assert!(text.contains("\n\u{FFFC}\u{FFFC} Codex\n"), "{text}");
    let back = parsed("screen@10x2.txt", &text);
    assert_eq!(found(&back, &screen), [], "{text}");

    let part = Region {
        top: 1,
        left: 1,
        rows: 1,
        cols: 4,
    };
    let text = drawing_text(&screen, part, &MOON, &names).unwrap();
    assert!(text.contains("\nat 1,1\nsize 4x1\n"), "{text}");
    assert_eq!(
        found(&parsed("part@10x2.txt", &text), &screen),
        [],
        "{text}"
    );
}

#[test]
fn the_report_lists_each_run_and_marks_the_row() {
    let drawing = parsed("limits@12x3.txt", TWO_ROWS);
    let mut screen = two_rows_screen();
    for col in 2..=4 {
        screen.cell_mut(2, col).ink = Colour::Indexed(1);
    }
    let placement = drawing.place(&screen);
    let found = differences_at(&drawing, &placement, &screen, &MOON, &|_, _| false);
    let text = report("limits@12x3.txt", &drawing, &screen, &placement, &found);
    assert!(
        text.starts_with(
            "limits@12x3.txt: 3 cells are different from the drawing, at rows 1-2, cols 0-11 \
             of the 12 × 3 screen.\n  Rows and columns count from the drawing's first cell.\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "row 1, cols 2-4: text colour: the drawing has #c8d3f5 Bright, the terminal's own \
             text colour, the screen #ff757f"
        ),
        "{text}"
    );
    assert!(
        text.contains("  row 1\n    drawing │  Codex\n    screen  │  Codex\n            │  ^^^\n"),
        "{text}"
    );
}

#[test]
fn the_256_colours_are_xterms_past_the_themes_sixteen() {
    assert_eq!(MOON.indexed(4).to_string(), "#82aaff");
    assert_eq!(MOON.indexed(16).to_string(), "#000000");
    assert_eq!(MOON.indexed(24).to_string(), "#005f87");
    assert_eq!(MOON.indexed(231).to_string(), "#ffffff");
    assert_eq!(MOON.indexed(232).to_string(), "#080808");
}

#[test]
fn keys_are_named_as_people_write_them() {
    assert_eq!(key_named("Shift+Tab"), Some(crate::Key::ShiftTab));
    assert_eq!(key_named("Ctrl+c"), Some(crate::Key::Ctrl('c')));
    assert_eq!(key_named("j"), Some(crate::Key::Char('j')));
    assert_eq!(key_named("Ctrl+"), None);
    assert_eq!(key_named("Space bar"), None);
}

const NOW: &str = "limits/now@12x3.txt";

/// A folder with the drawing `TWO_ROWS` at [`NOW`], listed in a manifest with `extra` in its
/// row, then `rows`.
fn approved_folder(extra: &str, rows: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("limits")).unwrap();
    fs::write(dir.path().join(NOW), TWO_ROWS).unwrap();
    let hash = hash_of(TWO_ROWS.as_bytes());
    let manifest = format!(
        "[[drawing]]\nfile = \"{NOW}\"\nrelease = \"0.2.0\"\napproval = 360\n\
         hash = \"{hash}\"\n{extra}{rows}"
    );
    fs::write(dir.path().join(MANIFEST), manifest).unwrap();
    dir
}

const STEPS: &str =
    "steps = [{ wait = \"Limits\" }, { press = \"Down\" }, { send = \"/usage\\r\" }]\n";

#[test]
fn a_manifest_lists_drawings_with_their_steps_and_departures() {
    let departure = "[[departure]]\nfile = \"limits/now@12x3.txt\"\nrow = 1\ncols = [2, 6]\n\
                     instead = \"the name in the CLI's colour\"\napproval = 361\n";
    let dir = approved_folder(STEPS, departure);
    let manifest = Manifest::load(dir.path()).unwrap();
    assert_eq!(manifest.problems(dir.path()), Vec::<String>::new());
    assert_eq!(manifest.drawings[0].folder(), DRAWN_FOLDER);
    assert_eq!(
        manifest.drawings[0].steps,
        [
            Step::Wait("Limits".to_owned()),
            Step::Press("Down".to_owned()),
            Step::Send("/usage\r".to_owned()),
        ]
    );
    let mut screen = two_rows_screen();
    for col in 2..=6 {
        screen.cell_mut(2, col).ink = Colour::Indexed(3);
    }
    assert_eq!(check(&manifest, dir.path(), NOW, &screen, &MOON), Ok(()));
    screen.cell_mut(2, 7).text = "s".to_owned();
    let err = check(&manifest, dir.path(), NOW, &screen, &MOON).unwrap_err();
    assert!(err.contains("row 1, col 7: text"), "{err}");
    let small = Grid::blank(12, 2);
    let err = check(&manifest, dir.path(), NOW, &small, &MOON).unwrap_err();
    assert!(
        err.contains("drawn for 12 × 3, but the screen is 12 × 2"),
        "{err}"
    );
}

#[test]
fn a_listed_drawing_without_steps_is_valid() {
    let dir = approved_folder("", "");
    let manifest = Manifest::load(dir.path()).unwrap();
    assert_eq!(manifest.problems(dir.path()), Vec::<String>::new());
    assert_eq!(manifest.drawings[0].steps, []);
    assert_eq!(
        check(&manifest, dir.path(), NOW, &two_rows_screen(), &MOON),
        Ok(())
    );
}

#[test]
fn a_drawing_changed_since_its_hash_is_found() {
    let dir = approved_folder("", "");
    let changed = TWO_ROWS.replace("; Dim", "; Faint");
    fs::write(dir.path().join(NOW), &changed).unwrap();
    let problems = Manifest::load(dir.path()).unwrap().problems(dir.path());
    assert_eq!(
        problems,
        [format!(
            "{NOW}: changed since it was approved: its hash is now {}",
            hash_of(changed.as_bytes())
        )]
    );
}

#[test]
fn a_manifest_finds_unlisted_unreadable_and_misplaced_rows() {
    let departure = "[[departure]]\nfile = \"limits/now@12x3.txt\"\nrow = 2\ncols = [2, 6]\n\
                     instead = \"\"\napproval = 0\n";
    let dir = approved_folder("folder = \"../elsewhere\"\n", departure);
    fs::write(dir.path().join("limits/later@12x3.txt"), TWO_ROWS).unwrap();
    let problems = Manifest::load(dir.path()).unwrap().problems(dir.path());
    let says = |words: &str| problems.iter().any(|problem| problem.contains(words));
    assert!(says("limits/later@12x3.txt isn't listed"), "{problems:#?}");
    assert!(says("needs the number of its approval"), "{problems:#?}");
    assert!(says("says what the screen draws instead"), "{problems:#?}");
    assert!(says("these cells aren't in the drawing"), "{problems:#?}");
    assert!(
        says("../elsewhere, isn't a path inside the home"),
        "{problems:#?}"
    );
    fs::write(dir.path().join(NOW), TWO_ROWS.replace("bold", "heavy")).unwrap();
    let problems = Manifest::load(dir.path()).unwrap().problems(dir.path());
    let says = |words: &str| problems.iter().any(|problem| problem.contains(words));
    assert!(says("\"heavy\" isn't fg=#rrggbb"), "{problems:#?}");
}

#[test]
fn the_approved_drawings_agree_with_their_manifest() {
    let dir = dir();
    let manifest = Manifest::load(&dir).unwrap_or_else(|err| panic!("{err:#}"));
    let problems = manifest.problems(&dir);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn no_approved_drawing_is_found_on_a_blank_screen() {
    let dir = dir();
    let manifest = Manifest::load(&dir).unwrap_or_else(|err| panic!("{err:#}"));
    for entry in &manifest.drawings {
        let drawing = Drawing::load(&dir.join(&entry.file)).unwrap_or_else(|err| panic!("{err:#}"));
        let blank = Grid::blank(drawing.cols, drawing.rows);
        let placement = drawing.place(&blank);
        let known = drawing.at.row.is_some() && drawing.at.col.is_some();
        assert_eq!(
            known,
            matches!(placement, Placement::At(_)),
            "{}: {placement:?}",
            entry.file
        );
        assert!(
            !differences_at(&drawing, &placement, &blank, &MOON, &|_, _| false).is_empty(),
            "{}",
            entry.file
        );
    }
}

#[test]
fn every_approved_drawing_keeps_the_screen_laws() {
    let dir = dir();
    let manifest = Manifest::load(&dir).unwrap_or_else(|err| panic!("{err:#}"));
    let mut breaches = Vec::new();
    for entry in &manifest.drawings {
        let drawing = Drawing::load(&dir.join(&entry.file)).unwrap_or_else(|err| panic!("{err:#}"));
        for breach in crate::screen::laws::check(&drawing.grid(), &crate::screen::Look::new()) {
            breaches.push(format!("{}: {breach}", entry.file));
        }
    }
    assert!(breaches.is_empty(), "{}", breaches.join("\n"));
}

#[test]
fn a_logos_picture_id_reads_back_as_its_logo_in_either_ink() {
    use super::paint::{light_ink, logo_id, logo_of};

    assert_eq!(logo_id("codex", None), Some(0x4c_4f02));
    for (stem, number) in [
        ("codex", None),
        ("claudecode", Some(1)),
        ("ollama", Some(9)),
    ] {
        let id = logo_id(stem, number).unwrap();
        assert_eq!(logo_of(id), Some((stem, number)), "{id:#x}");
        assert_eq!(logo_of(id + 1), Some((stem, number)), "{id:#x}");
        assert!(!light_ink(id) && light_ink(id + 1), "{id:#x}");
    }
    assert_eq!(logo_id("codex", Some(10)), None);
    assert_eq!(logo_of(0x12_3456), None);
}

#[test]
fn the_headers_version_reads_as_the_binarys_own() {
    let own = super::version::OWN;
    let drawing = |version: &str| {
        let text = format!(" Argonauts Code v{version}  ~/project");
        let paint: String = text
            .chars()
            .map(|c| if c == ' ' { '.' } else { 'b' })
            .collect();
        let keys = ". blank\nb fg=#c8d3f5 bold\n";
        parsed("header@40x1.txt", &row_drawing("40x1", &text, &paint, keys))
    };
    let screen = |version: &str| {
        let text = format!(" \x1b[1mArgonauts Code v{version}  ~/project");
        Grid::from_bytes(40, 1, text.as_bytes())
    };
    let header = format!("Argonauts Code v{own}  ~/project");
    for drawn in ["0.2.0", own, "12.40.7"] {
        let drawing = drawing(drawn);
        assert_eq!(found(&drawing, &screen(own)), [], "drawn at {drawn}");
        assert!(drawing.line(0).unwrap_or_default().contains(&header));
    }
    let elsewhere = found(&drawing("0.2.0"), &screen("0.2.0"));
    if own != "0.2.0" {
        assert!(
            !elsewhere.is_empty(),
            "the screen's own version is the binary's"
        );
    }
}

#[test]
fn a_new_version_moves_the_rest_of_its_row_into_the_blanks_at_its_end() {
    use super::drawing::{BLANK, Wanted};
    use super::version::as_own;

    let row = |text: &str| -> Vec<Wanted> {
        text.chars()
            .map(|text| Wanted {
                text,
                key: if text == ' ' { BLANK } else { 'b' },
            })
            .collect()
    };
    let text = |cells: &[Wanted]| cells.iter().map(|cell| cell.text).collect::<String>();
    let mut lines = vec!["Argonauts Code v0.2.0 ok".to_owned()];
    let mut cells = row("Argonauts Code v0.2.0 ok    ");
    as_own(&mut lines, &mut cells, 28, "0.10.12");
    assert_eq!(text(&cells), "Argonauts Code v0.10.12 ok  ");
    assert_eq!(lines[0], "Argonauts Code v0.10.12 ok");
    assert_eq!(cells[22].key, 'b');

    as_own(&mut lines, &mut cells, 28, "1.0");
    assert_eq!(text(&cells), "Argonauts Code v1.0 ok      ");
    assert_eq!(cells[23].key, BLANK);

    let mut lines = vec!["Argonauts Code v1.0 ok".to_owned()];
    let mut cells = row(&lines[0]);
    as_own(&mut lines, &mut cells, 22, "1.0.10");
    assert_eq!(
        text(&cells),
        "Argonauts Code v1.0 ok",
        "no room to grow into"
    );
}

#[test]
fn a_drawing_reads_as_the_grid_it_draws() {
    let keys = ". blank\nL logo=codex\nb fg=#828bb8 bg=#123456 bold\nf bg=#123456\n";
    let drawing = parsed(
        "row@8x1.txt",
        &row_drawing("8x1", "\u{FFFC}\u{FFFC} ab", "LL.bbf", keys),
    );
    let grid = drawing.grid();
    assert_eq!((grid.cols(), grid.rows()), (8, 1));
    let logo = grid.cell(0, 1);
    assert_eq!(
        logo.picture,
        Some(crate::screen::Placeholder {
            id: CODEX,
            shows: true
        })
    );
    assert_eq!(found(&drawing, &grid), []);
    let b = grid.cell(0, 4);
    assert_eq!((b.text.as_str(), b.bold), ("b", true));
    assert_eq!(
        grid.cell(0, 5).background,
        Colour::Rgb(Rgb(0x12, 0x34, 0x56))
    );
}
