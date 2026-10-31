use super::laws::{self, Law, Look};
use super::tokens::is_figure;
use super::{Area, Colour, Grid, REAL_SIZES, Rgb, Size};

fn laws_broken(grid: &Grid, look: &Look) -> Vec<Law> {
    laws::check(grid, look)
        .iter()
        .map(|breach| breach.law)
        .collect()
}

#[test]
fn the_real_sizes_start_with_a_laptop_and_the_wide_window() {
    assert_eq!(REAL_SIZES[0], Size::new(120, 38));
    assert_eq!(REAL_SIZES[1], Size::new(177, 40));
    assert_eq!(REAL_SIZES[0].to_string(), "120 × 38");
}

#[test]
fn text_takes_the_cells_a_terminal_gives_it() {
    let grid = Grid::from_text(8, &["修复 ok", "a long row cut at the edge"]);
    assert_eq!(grid.cell(0, 0).text, "修");
    assert!(grid.cell(0, 1).is_covered());
    assert_eq!(grid.cell(0, 5).text, "o");
    assert_eq!(grid.row_text(grid.area(), 1), "a long r");
    assert_eq!(grid.find_in_row(grid.area(), 0, "ok"), [5]);
}

#[test]
fn bytes_keep_their_colours() {
    let grid = Grid::from_bytes(10, 2, b"\x1b[38;2;10;20;30mA\x1b[0m\x1b[38;5;244mB\x1b[7mC");
    assert_eq!(grid.cell(0, 0).ink, Colour::Rgb(Rgb(10, 20, 30)));
    assert_eq!(grid.cell(0, 1).ink, Colour::Indexed(244));
    assert!(grid.cell(0, 1).ink.is_grey());
    assert!(grid.cell(0, 2).inverse);
    assert!(!Colour::Rgb(Rgb(120, 80, 200)).is_grey());
    assert!(Colour::Rgb(Rgb(128, 130, 140)).is_grey());
    assert!(
        Colour::Rgb(Rgb(0x8a, 0x8f, 0xa8)).is_grey(),
        "the theme's grey of the rest"
    );
    assert!(!Colour::Rgb(Rgb(0xb1, 0xb9, 0xf9)).is_grey(), "a pale blue");
}

#[test]
fn figures_are_what_a_column_of_numbers_holds() {
    for figure in [
        "12", "$5.35", "~$1.55", "68%", "<1%", "1.97M", "118.4b", "3h", "(33.3%)",
    ] {
        assert!(is_figure(figure), "{figure}");
    }
    for word in ["5.5", "Opus", "task-001", "$", "%", "v2"] {
        assert!(!is_figure(word), "{word}");
    }
}

#[test]
fn a_selection_shows_its_pointer_on_its_row() {
    let list = Grid::from_text(30, &["  Read a file", "❯ Ran the tests", "  Wrote a test"]);
    assert!(laws_broken(&list, &Look::new().selected("Ran the tests")).is_empty());
    assert_eq!(
        laws_broken(&list, &Look::new().selected("Wrote a test")),
        [Law::SelectedRowDrawn]
    );
    let scrolled_away = Grid::from_text(30, &["  Read a file", "  Ran the tests"]);
    assert_eq!(
        laws_broken(&scrolled_away, &Look::new().selection()),
        [Law::SelectedRowDrawn]
    );
    assert!(
        laws_broken(&scrolled_away, &Look::new()).is_empty(),
        "nothing selected"
    );
}

#[test]
fn columns_of_words_stand_four_blanks_apart_and_numbers_three() {
    let kept = Grid::from_text(
        60,
        &[
            "model         on             spend  share",
            "Opus 5.5      Claude Code    $5.35    68%",
            "GPT 6 Luna    Codex          $0.62     7%",
        ],
    );
    assert!(laws_broken(&kept, &Look::new()).is_empty());
    let crowded = Grid::from_text(
        60,
        &[
            "task-001  CSV writer   GPT 5.6 on Codex",
            "task-002  Endpoint     Opus 5.5 on Claude Code",
        ],
    );
    let breaches = laws::columns(&crowded, &Look::new());
    let what: Vec<&str> = breaches.iter().map(|breach| breach.what.as_str()).collect();
    assert_eq!(
        what,
        [
            "“CSV” has 2 blanks after “task-001”, and a column of words takes 4",
            "“GPT” has 3 blanks after “writer”, and a column of words takes 4",
            "“Endpoint” has 2 blanks after “task-002”, and a column of words takes 4",
        ]
    );
    let close = Grid::from_text(40, &["Opus 5.5  $5.35 68%", "Haiku 4.5 $0.20 10%"]);
    let what: Vec<String> = laws::columns(&close, &Look::new())
        .into_iter()
        .map(|breach| breach.what)
        .collect();
    assert_eq!(
        what,
        [
            "“$5.35” has 2 blanks after “5.5”, and a column of numbers takes 3",
            "“68%” has 1 blank after “$5.35”, and a share after its amount takes 2",
            "“$0.20” has 1 blank after “4.5”, and a column of numbers takes 3",
            "“10%” has 1 blank after “$0.20”, and a share after its amount takes 2",
        ]
    );
}

#[test]
fn prose_rows_and_lone_rows_have_no_columns() {
    let prose = Grid::from_text(
        70,
        &[
            "What your agents did, from 28 actions in 19 runs across 6 projects",
            "Lit: read a source file · 17 actions, 12 of them Haiku 5.5's",
        ],
    );
    assert!(laws_broken(&prose, &Look::new()).is_empty());
    let tabs = Grid::from_text(40, &["Overview  Status  Config  Usage"]);
    assert!(laws_broken(&tabs, &Look::new()).is_empty());
    let durations = Grid::from_text(40, &["Opus 5.5     2h 20m", "GPT 5.6      1h 22m"]);
    assert!(laws_broken(&durations, &Look::new()).is_empty());
    let resets = Grid::from_text(
        60,
        &[
            "5-hour   12%   resets in 2h 45m",
            "week     44%   resets in 2d 6h",
        ],
    );
    assert!(laws_broken(&resets, &Look::new()).is_empty());
    let numbered = Grid::from_text(40, &["❯ 1.  Council", "  2.  Orchestrator", "  10. Ablate"]);
    assert!(laws_broken(&numbered, &Look::new().selected("Council")).is_empty());
}

/// Rows of model swatches, each a `█` in its colour before its name.
fn swatches(rows: &[(&str, (u8, u8, u8), &str)]) -> Grid {
    let mut bytes = Vec::new();
    for (at, (glyph, (r, g, b), name)) in rows.iter().enumerate() {
        bytes.extend(
            format!(
                "\x1b[{};1H\x1b[38;2;{r};{g};{b}m{glyph}\x1b[0m {name}",
                at + 1
            )
            .bytes(),
        );
    }
    let height = u16::try_from(rows.len()).unwrap();
    Grid::from_bytes(30, height, &bytes)
}

#[test]
fn two_models_never_share_a_swatch_but_may_share_a_dot_or_the_grey() {
    let look = Look::new().models(&["Opus 5.5", "Haiku 5.5", "Sonnet 5.5", "GPT 6 Luna"]);
    let apart = swatches(&[
        ("█", (215, 119, 87), "Opus 5.5"),
        ("▚", (215, 119, 87), "Haiku 5.5"),
        ("█", (16, 163, 127), "GPT 6 Luna"),
        ("█", (215, 119, 87), "Opus 5.5"),
    ]);
    assert!(laws_broken(&apart, &look).is_empty());
    let alike = swatches(&[
        ("█", (215, 119, 87), "Opus 5.5"),
        ("█", (215, 119, 87), "Haiku 5.5"),
    ]);
    let breaches = laws::models(&alike, &look);
    assert_eq!(breaches.len(), 1, "{breaches:#?}");
    assert!(
        breaches[0]
            .what
            .starts_with("Haiku 5.5 draws █ in #d77757, as Opus 5.5 does")
    );
    let shared = swatches(&[
        ("●", (215, 119, 87), "Opus 5.5"),
        ("●", (215, 119, 87), "Haiku 5.5"),
        ("█", (128, 128, 136), "Sonnet 5.5"),
        ("█", (128, 128, 136), "GPT 6 Luna"),
    ]);
    assert!(laws_broken(&shared, &look).is_empty());
}

#[test]
fn a_name_is_cut_only_when_its_row_has_no_room_for_the_rest() {
    let name = "Gemini 3.8 Flash (Low)";
    let look = Look::new().whole(&[name]);
    let roomy = Grid::from_text(60, &["█ Gemini 3.8 Fla…  agy"]);
    let breaches = laws::cut(&roomy, &look);
    assert_eq!(breaches.len(), 1, "{breaches:#?}");
    assert!(
        breaches[0].what.contains("cut to “Gemini 3.8 Fla…”"),
        "{breaches:#?}"
    );
    let tight = Grid::from_text(24, &["█ Gemini 3.8 Fla…  agy"]);
    assert!(laws::cut(&tight, &look).is_empty());
    let framed = Grid::from_text(60, &["│ Gemini 3.8 Fla… agy │      "]);
    assert!(
        laws::cut(&framed, &look).is_empty(),
        "the room past the frame isn't the panel's"
    );
    let inside = Area::new(0, 0, 24, 1);
    assert!(laws::cut(&roomy, &look.clone().within(inside)).is_empty());
}

/// A sentence cut at the panel's edge in the middle of a name is cut for want of room; the
/// same name as a column's value, cut while its row has room, is not.
#[test]
fn a_sentence_cut_at_the_edge_mid_name_keeps_the_law() {
    let look = Look::new().whole(&["OpenCode"]);
    let sentence = " Through Argonauts Code: OpenCod…";
    let edge = Grid::from_text(sentence.chars().count() as u16, &[sentence]);
    assert!(
        laws::cut(&edge, &look).is_empty(),
        "{:#?}",
        laws::cut(&edge, &look)
    );
    let column = Grid::from_text(60, &["  Claude Code    OpenCo…      12"]);
    assert_eq!(laws::cut(&column, &look).len(), 1);
}

fn what(breaches: Vec<laws::Breach>) -> Vec<String> {
    breaches.into_iter().map(|breach| breach.what).collect()
}

#[test]
fn a_mark_keeps_to_its_name_by_its_own_room() {
    let kept = Grid::from_text(
        40,
        &[
            "▌ Claude Code",
            "██  Opus 5.5",
            "■  Cache read",
            "● Codex",
            "▁▂▃▅ busy",
            "↓ 3 more",
            "model   ▾tokens",
            "    ▗█▖ ▗▘ Argonauts Code",
        ],
    );
    assert!(laws_broken(&kept, &Look::new()).is_empty());
    let close = Grid::from_text(40, &["██ Opus 5.5", "■ Cache read", "●Codex"]);
    assert_eq!(
        what(laws::columns(&close, &Look::new())),
        [
            "“Opus” has 1 blank after its ██, and a name after a table's swatch takes 2",
            "“Cache” has 0 blanks after its ■, and a name after its mark takes 1 or 2",
            "“Codex” has 0 blanks after its ●, and a name after its mark takes 1 or 2",
        ]
    );
    let logo = "\u{10EEEE}\u{10EEEE}";
    let kept = Grid::from_text(40, &[format!("{logo} Claude Code")]);
    assert!(laws_broken(&kept, &Look::new()).is_empty());
    let touching = Grid::from_text(40, &[format!("{logo}Claude Code")]);
    assert_eq!(
        what(laws::columns(&touching, &Look::new())),
        ["“Claude” has 0 blanks after its logo, and a logo's name takes 1"]
    );
}

#[test]
fn a_legend_on_a_row_of_its_own_keeps_its_entries_apart() {
    let kept = Grid::from_text(60, &["▌ Claude Code 1.2M 40%      ▌ Codex 300k 10%"]);
    assert!(laws_broken(&kept, &Look::new()).is_empty());
    let crowded = Grid::from_text(60, &["▌ Cache read 97%  ▌ Write 2%"]);
    assert_eq!(
        what(laws::columns(&crowded, &Look::new())),
        ["“▌” has 2 blanks after “97%”, and a legend's next entry takes 6"]
    );
    let meters = Grid::from_text(80, &["● Claude 5h ▄▄▄▄▖▁  71%   ○ Codex 5h ▄▄▖▁▁▁  38%"]);
    assert!(laws_broken(&meters, &Look::new()).is_empty());
    let table = Grid::from_text(
        60,
        &["Opus 5.5    12   ▁▂▃▅▆ busy", "Haiku 5.5    3   ▁▁▂ idle"],
    );
    assert!(laws_broken(&table, &Look::new()).is_empty());
}

#[test]
fn a_mark_stands_no_further_from_its_name_than_its_room() {
    let logo = "\u{10EEEE}\u{10EEEE}";
    let off = Grid::from_text(
        40,
        &[
            format!("{logo}  Claude Code"),
            "●   Codex".to_owned(),
            "██   Opus 5.5".to_owned(),
        ],
    );
    assert_eq!(
        what(laws::columns(&off, &Look::new())),
        [
            "“Claude” has 2 blanks after its logo, and a logo's name takes 1",
            "“Codex” has 3 blanks after its ●, and a name after its mark takes 1 or 2",
            "“Opus” has 3 blanks after its ██, and a name after a table's swatch takes 2",
        ]
    );
    let columns = Grid::from_text(40, &["●    running", "○    waiting"]);
    assert!(laws_broken(&columns, &Look::new()).is_empty());
}

#[test]
fn a_state_stands_two_blanks_before_its_name() {
    // Rows of `approved/council/plan@120x38.txt` and `work-starts@120x38.txt`: the question
    // keeps its mark a blank from its words, a run's row and a step its state two.
    let logo = "\u{10EEEE}\u{10EEEE}";
    let rows = [
        "◆ Approve this plan?".to_owned(),
        String::new(),
        format!(
            "     ✔  Build {logo} Opus 5.5 ── ○  Review {logo} 5.6 Luna, new session ── until it holds"
        ),
        String::new(),
        "  ◆  Rate limit the API                    ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰  4/4 · 5m10s · ↓  90.8k tokens"
            .to_owned(),
        String::new(),
        "  ⣾  Rate limit the API                    ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  0/4 · 11m02s · ↓  1.2M tokens"
            .to_owned(),
    ];
    let kept = Grid::from_text(120, &rows);
    assert!(laws_broken(&kept, &Look::new()).is_empty());
    // A third blank stands a state off its name, and a logo keeps to its name with one.
    let off = Grid::from_text(
        120,
        &[
            "  ⣾   Rate limit the API".to_owned(),
            String::new(),
            format!("  ○  {logo}  Opus 5.5"),
        ],
    );
    assert_eq!(
        what(laws::columns(&off, &Look::new())),
        [
            "“Rate” has 3 blanks after its ⣾, and a name after its mark takes 1 or 2",
            "“Opus” has 2 blanks after its logo, and a logo's name takes 1",
        ]
    );
}

#[test]
fn a_list_that_hides_rows_says_how_many() {
    let rows = [
        "read a source file",
        "ran the tests",
        "searched the code",
        "wrote a new test",
    ];
    let look = Look::new().list(&rows);
    assert!(laws_broken(&Grid::from_text(40, &rows), &look).is_empty());
    let counted = Grid::from_text(40, &["read a source file", "ran the tests", "↓ 2 more"]);
    assert!(laws_broken(&counted, &look).is_empty());
    let scrolled = Grid::from_text(
        40,
        &["↑ 1 more", "ran the tests", "searched the c…", "↓ 1 more"],
    );
    assert!(laws_broken(&scrolled, &look).is_empty());
    let silent = Grid::from_text(40, &["read a source file", "ran the tests"]);
    assert_eq!(
        what(laws::hidden_rows(&silent, &look)),
        ["the list shows 2 of its 4 rows and hides 2, and nothing says so"]
    );
    let miscounted = Grid::from_text(40, &["read a source file", "↓ 2 more"]);
    let breaches = laws::hidden_rows(&miscounted, &look);
    assert_eq!(
        what(breaches.clone()),
        ["the list shows 1 of its 4 rows and hides 3, and it says “2 more”"]
    );
    assert_eq!(
        (breaches[0].law, breaches[0].row),
        (Law::HiddenRowsCounted, 1)
    );
    let ranged = Grid::from_text(40, &["ran the tests", "searched the code", "↑ 2–3 of 4 ↓"]);
    assert!(laws_broken(&ranged, &look).is_empty());
    let misranged = Grid::from_text(40, &["ran the tests", "↑ 2–3 of 4 ↓"]);
    assert_eq!(
        what(laws::hidden_rows(&misranged, &look)),
        ["the list shows 1 of its 4 rows and hides 3, and it says “2–3 of 4”"]
    );
    let inside = Grid::from_text(40, &["ran the tests again", "reran the tests"]);
    assert_eq!(laws::hidden_rows(&inside, &look).len(), 1);
}

/// The `/context` panel's rows as `approved/context/claude-code@120x38.txt` draws them.
const CONTEXT_ROWS: [&str; 14] = [
    "     Context Usage",
    "     ⛁ ⛁ ⛁ ⛁ ⛁ ⛀ ⛀ ⛀ ⛀ ⛀ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶   ￼￼ Opus 5.5",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶   claude-opus-5-5",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶   28.8k/1m tokens (3%)",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶   Estimated usage by category",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶   ⛁ System prompt: 4.4k tokens (0.4%)",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶   ⛁ System tools: 19.4k tokens (1.9%)",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶   ⛁ MCP tools: 2.1k tokens (0.2%)",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶   ⛁ MCP server instructions: 318 tokens (<0.1%)",
    "     ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛶ ⛝ ⛝ ⛝ ⛝ ⛝ ⛝ ⛝   ⛁ Memory files: 503 tokens (0.1%)",
    "                                               ⛁ Skills: ~2.1k tokens (0.2%)",
    "                                               ⛁ Messages: 10 tokens (<0.1%)",
    "                                               ⛶ Free space: 938.2k (93.8%)",
];

#[test]
fn claude_codes_context_lines_are_prose() {
    let context = Grid::from_text(120, &CONTEXT_ROWS);
    assert!(laws_broken(&context, &Look::new()).is_empty());
    // The same figures as a table's columns still keep their room.
    let table = Grid::from_text(
        60,
        &["System prompt 4.4k (0.4%)", "Skills        2.1k (0.2%)"],
    );
    assert_eq!(
        what(laws::columns(&table, &Look::new())),
        [
            "“4.4k” has 1 blank after “prompt”, and a column of numbers takes 3",
            "“(0.4%)” has 1 blank after “4.4k”, and a share after its amount takes 2",
            "“(0.2%)” has 1 blank after “2.1k”, and a share after its amount takes 2",
        ]
    );
}

/// `text` on a background of its own, as a badge is drawn.
fn badge(text: &str) -> String {
    format!("\x1b[48;2;255;193;7m \x1b[1;38;2;31;32;47m{text}\x1b[22;39m \x1b[0m")
}

#[test]
fn a_badge_keeps_to_the_words_after_it_as_a_mark_does() {
    // Rows of `approved/behaviors/deeper-list-down-1@120x38.txt`.
    let rows = [
        "  ❯       Broad search for cache invalidation".to_owned(),
        format!("    {} No change made", badge("new")),
        "          Listing project files to understand structure".to_owned(),
        format!("    {} Responded with a greeting", badge("new")),
    ];
    let bytes = rows.join("\r\n");
    let list = Grid::from_bytes(70, 4, bytes.as_bytes());
    assert_eq!(list.cell(1, 4).background, list.cell(1, 8).background);
    assert!(laws_broken(&list, &Look::new()).is_empty());
    // Without its background the word is a column of its own, 2 blanks from the next.
    let plain = Grid::from_text(70, &rows.map(|row| row.replace(&badge("new"), " new ")));
    assert_eq!(
        what(laws::columns(&plain, &Look::new())),
        [
            "“No” has 2 blanks after “new”, and a column of words takes 4",
            "“Responded” has 2 blanks after “new”, and a column of words takes 4",
        ]
    );
}

#[test]
fn a_label_and_the_count_right_aligned_after_it_are_one_entry() {
    // Rows of `approved/behaviors/label-list-down-1@120x38.txt`.
    let rows = [
        "  ❯ Presented a plan                                        3   (4.8%)   others   3   (0.4%)",
        "    Wrote a workflow script                                 4   (6.3%)   others   6   (0.8%)",
        "    Read a doc                                              3   (4.8%)   others   7   (0.9%)",
        "    Read git history                                        4   (6.3%)   others  10   (1.3%)",
        "    Edited a source file                                    8  (12.7%)   others  26   (3.4%)",
    ];
    let list = Grid::from_text(120, &rows);
    assert!(laws_broken(&list, &Look::new().selection()).is_empty());
    // Counts after different words are a column of their own.
    let mixed = rows.map(|row| row.replacen("others  10", "rest    10", 1));
    let mixed = Grid::from_text(120, &mixed);
    assert_eq!(
        what(laws::columns(&mixed, &Look::new())),
        ["“26” has 2 blanks after “others”, and a column of numbers takes 3"]
    );
}

#[test]
fn a_clis_chips_are_no_legend_with_or_without_meters() {
    // The bottom line as the Mods page previews it at 80 × 24, and with its meters.
    let chips = Grid::from_text(
        80,
        &[
            "  ⏵⏵ auto     ● Claude 56%   ○ CX",
            "  ⏵⏵ auto      ● Claude ██████  93% resets 4:40pm   ○ Codex █░░░░░  11%   ○ agy out",
        ],
    );
    assert!(laws_broken(&chips, &Look::new()).is_empty());
    let logo = "\u{10EEEE}\u{10EEEE}";
    let logos = Grid::from_text(80, &[format!("{logo} 5h █▁▁▁▁▁ 15%   {logo} 5h ▁▁▁▁▁▁ 0%")]);
    assert!(laws_broken(&logos, &Look::new()).is_empty());
    // A legend's swatches still stand their room apart.
    let legend = Grid::from_text(80, &["▌ Claude Code 2.9M 58%   ▌ Codex 1.55M 31%"]);
    assert_eq!(
        what(laws::columns(&legend, &Look::new())),
        ["“▌” has 3 blanks after “58%”, and a legend's next entry takes 6"]
    );
}

#[test]
fn a_progress_bar_is_a_column_of_bars() {
    // Rows of `approved/council/work-starts@120x38.txt`, the panel under the input.
    let logo = "\u{10EEEE}\u{10EEEE}";
    let rows = [
        format!(
            "      ⣾  {logo} Plan limits      Own session   ▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  1/5 ·  3m36s"
        ),
        format!(
            "      ⣾  {logo} Route tests      3 agents      ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  0/3 ·  1m04s"
        ),
    ];
    let panel = Grid::from_text(100, &rows);
    assert!(laws_broken(&panel, &Look::new()).is_empty());
    // A bar two blanks after the words before it still lacks its room.
    let crowded = [
        rows[0].replace("Own session   ", "Own sessions  "),
        rows[1].clone(),
    ];
    assert_eq!(
        what(laws::columns(&Grid::from_text(100, &crowded), &Look::new())),
        ["“▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱” has 2 blanks after “sessions”, and a column of bars takes 3"]
    );
}

#[test]
fn a_typing_bubbles_dots_and_a_logo_in_the_gutter_name_no_word() {
    // Rows of `approved/council/talk@120x38.txt`: a message's last line beside its writer's
    // logo, then the typing bubble.
    let logo = "\u{10EEEE}\u{10EEEE}";
    let rows = [
        "     interface for the tests. And the balancer's probe calls /health with no key, so a"
            .to_owned(),
        format!("{logo}   plain 401 takes a node out."),
        String::new(),
        format!("{logo}  • • •   Qwen3 Coder 30B is writing · 3.8 Flash replied to its message"),
    ];
    let talk = Grid::from_text(100, &rows);
    assert!(laws_broken(&talk, &Look::new()).is_empty());
    // A logo before a name of its own, and one dot, still keep to the name.
    let off = Grid::from_text(
        100,
        &[format!("{logo}   Claude Code"), "•   Qwen3".to_owned()],
    );
    assert_eq!(
        what(laws::columns(&off, &Look::new())),
        [
            "“Claude” has 3 blanks after its logo, and a logo's name takes 1",
            "“Qwen3” has 3 blanks after its •, and a name after its mark takes 1 or 2",
        ]
    );
}

#[test]
fn a_swatchs_glyph_alone_in_a_column_of_bars_is_a_days_bar() {
    // Rows of the Limits page's spend for the week, as its drawing at 120 × 38 has them: each
    // day's spend after the week's.
    let logo = "\u{10EEEE}\u{10EEEE}";
    let rows = [
        format!(
            "│ ▚▚  Opus 5.5        {logo} Claude Code    $5.35   68%    1.97M   ▚   ▚▚▚ ▁▁▁   │"
        ),
        format!(
            "│ ⣿⣿  GPT 6 Astra     {logo} Codex         ~$1.55   20%    0.39M   ⣿⣿  ▁▁▁ ▁▁▁   │"
        ),
        format!(
            "│ ██  Haiku 5.5       {logo} Claude Code   ~$0.38    5%    6.50M   ▌   █▌  ▁▁▁   │"
        ),
        format!(
            "│ ▚▚  GPT 6.1 Sol     {logo} Codex         ~$0.37    5%    0.63M   ▁▁▁ ▚   ▁▁▁   │"
        ),
    ];
    let week = Grid::from_text(100, &rows);
    assert!(laws_broken(&week, &Look::new()).is_empty());
    // With no bar in their column the same glyphs start a legend's entries.
    let legend = Grid::from_text(100, &rows[..3]);
    assert_eq!(
        what(laws::columns(&legend, &Look::new())),
        [
            "“▚” has 3 blanks after “1.97M”, and a legend's next entry takes 6",
            "“⣿⣿” has 3 blanks after “0.39M”, and a legend's next entry takes 6",
            "“▌” has 3 blanks after “6.50M”, and a legend's next entry takes 6",
        ]
    );
}
