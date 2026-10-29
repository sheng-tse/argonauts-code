use std::path::{Path, PathBuf};

use argonauts_core::display_names::DisplayNames;
use argonauts_core::logos::logo_for;
use argonauts_core::names::{AgentId, ModelRef};
use ratatui::style::{Color, Style};

use super::*;
use crate::mascot::png;
use crate::text::marks::decoration;
use crate::text::spans_width;
use crate::theme::Glyphs;

fn logos_folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/logos")
}

fn logo(agent: &str, mark: Option<LogoMark>) -> Logo {
    Logo {
        agent: AgentId(agent.to_owned()),
        mark,
    }
}

const BOTH: [Brightness; 2] = [Brightness::Dark, Brightness::Light];

#[test]
fn every_logo_ships_at_every_size_and_reads_back() {
    for mark in LogoMark::ALL {
        for size in LogoSize::ALL {
            for on in BOTH {
                let image = png::decode(mark_png(mark, size, on))
                    .unwrap_or_else(|| panic!("{mark:?} at {size:?} on {on:?} doesn't read"));
                let (width, height) = size.pixels();
                assert_eq!(
                    (image.width, image.height),
                    (width as usize, height as usize),
                    "{mark:?} at {size:?}"
                );
            }
        }
    }
}

#[test]
fn each_logo_shows_the_files_named_by_its_stem() {
    let folder = logos_folder();
    let mut shown = 0;
    for mark in LogoMark::ALL {
        let inks: &[(Brightness, &str)] = if one_colour(mark) {
            &[(Brightness::Dark, ""), (Brightness::Light, "-on-light")]
        } else {
            &[(Brightness::Dark, "")]
        };
        for (on, ink) in inks {
            for (size, name) in [
                (LogoSize::Cells, "cells"),
                (LogoSize::Cells2x, "cells-2x"),
                (LogoSize::Px64, "64"),
                (LogoSize::Px128, "128"),
            ] {
                let file = folder.join(format!("{}-{name}{ink}.png", mark.stem()));
                let on_disk = std::fs::read(&file).expect("the picture is in the folder");
                assert_eq!(mark_png(mark, size, *on), on_disk, "{}", file.display());
                shown += 1;
            }
        }
    }
    let shipped = std::fs::read_dir(&folder)
        .expect("the folder reads")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .count();
    assert_eq!(shipped, shown, "a picture in the folder no logo shows");
}

#[test]
fn only_the_logos_drawn_in_the_text_colour_come_in_two_inks() {
    let sources = logos_folder().join("svg");
    for mark in LogoMark::ALL {
        let svg = std::fs::read_to_string(sources.join(format!("{}.svg", mark.stem())))
            .expect("each logo has its source");
        assert_eq!(one_colour(mark), svg.contains("currentColor"), "{mark:?}");
        let (dark, light) = (
            mark_png(mark, LogoSize::Cells, Brightness::Dark),
            mark_png(mark, LogoSize::Cells, Brightness::Light),
        );
        assert_eq!(dark == light, !one_colour(mark), "{mark:?}");
    }
}

#[test]
fn a_two_cell_picture_holds_its_logo_in_the_middle() {
    for (size, margin_x, margin_y) in [(LogoSize::Cells, 4, 7), (LogoSize::Cells2x, 8, 14)] {
        for mark in LogoMark::ALL {
            let image = png::decode(mark_png(mark, size, Brightness::Dark)).expect("reads");
            for y in 0..image.height {
                for x in 0..image.width {
                    let inside = (margin_x..image.width - margin_x).contains(&x)
                        && (margin_y..image.height - margin_y).contains(&y);
                    let alpha = image.pixels[(y * image.width + x) * 4 + 3];
                    assert!(
                        inside || alpha == 0,
                        "{mark:?} at {size:?} reaches {x}, {y}"
                    );
                }
            }
        }
    }
}

#[test]
fn larger_cells_take_the_double_picture_and_squares_round_up() {
    assert_eq!(LogoSize::for_cell((18, 42)), LogoSize::Cells);
    assert_eq!(LogoSize::for_cell((9, 21)), LogoSize::Cells);
    assert_eq!(LogoSize::for_cell((19, 42)), LogoSize::Cells2x);
    assert_eq!(LogoSize::for_cell((18, 44)), LogoSize::Cells2x);
    assert_eq!(LogoSize::square(1), LogoSize::Px64);
    assert_eq!(LogoSize::square(64), LogoSize::Px64);
    assert_eq!(LogoSize::square(65), LogoSize::Px128);
    assert_eq!(LogoSize::square(4000), LogoSize::Px128);
}

#[test]
fn in_cells_a_logo_is_its_agents_colour_dot_two_cells_wide() {
    let theme = Theme::dark();
    let spans = logo_spans(&logo("codex", Some(LogoMark::Codex)), &theme);
    assert_eq!(spans_width(&spans), usize::from(LOGO_WIDTH));
    assert_eq!(spans[0].content, "●");
    assert_eq!(spans[0].style, decoration(theme.style(Role::CliCodex)));
    assert_eq!(spans[1].content, " ");
    assert_eq!(spans[1].style, Style::default());

    let unknown = logo_spans(&Logo::dot("lmstudio"), &theme);
    assert_eq!(unknown[0].style, decoration(theme.style(Role::CatOther)));

    let mut plain = Theme::dark();
    plain.glyphs = Glyphs::ASCII;
    let ascii = logo_spans(&logo("claude-code", Some(LogoMark::ClaudeCode)), &plain);
    assert_eq!(ascii[0].content, "*");
    assert_eq!(spans_width(&ascii), usize::from(LOGO_WIDTH));
}

#[test]
fn a_logo_without_a_file_has_no_picture() {
    assert_eq!(
        logo_png(&Logo::dot("lmstudio"), LogoSize::Cells, Brightness::Dark),
        None
    );
    let codex = logo("codex", Some(LogoMark::Codex));
    assert_eq!(
        logo_png(&codex, LogoSize::Px64, Brightness::Dark),
        Some(mark_png(LogoMark::Codex, LogoSize::Px64, Brightness::Dark))
    );
}

fn with_pictures(brightness: Brightness) -> Theme {
    let mut theme = Theme::for_brightness(brightness);
    theme.logo_pictures = true;
    theme
}

const CODEX_CELLS: &str = "\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}";

#[test]
fn where_the_terminal_holds_them_a_logo_is_its_pictures_placeholders() {
    let codex = logo("codex", Some(LogoMark::Codex));
    let spans = logo_spans(&codex, &with_pictures(Brightness::Dark));
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, CODEX_CELLS);
    assert_eq!(
        spans[0].style,
        decoration(Style::new().fg(Color::Rgb(0x4c, 0x4f, 0x02)))
    );
    assert_eq!(spans_width(&spans), usize::from(LOGO_WIDTH));
}

#[test]
fn a_one_colour_logo_takes_the_ink_for_the_screen_and_a_coloured_one_keeps_its_own() {
    let opencode = logo("opencode", Some(LogoMark::OpenCode));
    let on_dark = logo_spans(&opencode, &with_pictures(Brightness::Dark));
    let on_light = logo_spans(&opencode, &with_pictures(Brightness::Light));
    assert_eq!(on_dark[0].style.fg, Some(Color::Rgb(0x4c, 0x4f, 0x06)));
    assert_eq!(on_light[0].style.fg, Some(Color::Rgb(0x4c, 0x4f, 0x07)));

    let claude = logo("claude-code", Some(LogoMark::ClaudeCode));
    assert_eq!(
        logo_spans(&claude, &with_pictures(Brightness::Light)),
        logo_spans(&claude, &with_pictures(Brightness::Dark))
    );
}

#[test]
fn a_logo_without_a_file_stays_a_dot_where_pictures_draw() {
    let theme = with_pictures(Brightness::Dark);
    let spans = logo_spans(&Logo::dot("lmstudio"), &theme);
    assert_eq!(spans[0].content, "●");
    assert_eq!(spans_width(&spans), usize::from(LOGO_WIDTH));
}

#[test]
fn pictures_draw_only_once_sent_in_true_colour_with_narrow_ambiguous_glyphs() {
    let theme = Theme::dark();
    assert!(!draws_pictures(Logos::Auto, &theme, 1));
    set_takes_logos(true);
    assert!(draws_pictures(Logos::Auto, &theme, 1));
    assert!(!draws_pictures(Logos::Off, &theme, 1));
    assert!(!draws_pictures(Logos::Auto, &theme, 2));
    let fewer = Theme::dark().with_level(ColorLevel::Ansi256);
    assert!(!draws_pictures(Logos::Auto, &fewer, 1));
    set_takes_logos(false);
}

#[test]
fn every_logo_goes_out_once_in_each_ink_with_its_placement() {
    let sent = send_pictures(Some((18, 42)));
    let placements = String::from_utf8_lossy(&sent).matches("a=p,U=1,").count();
    let inks: usize = LogoMark::ALL
        .iter()
        .map(|mark| if one_colour(*mark) { 2 } else { 1 })
        .sum();
    assert_eq!(placements, inks);

    let codex = crate::mascot::placeholder::send(
        mark_png(LogoMark::Codex, LogoSize::Cells, Brightness::Dark),
        0x4c_4f02,
        (2, 1),
    );
    assert!(sent.windows(codex.len()).any(|part| part == codex));
    let kimi_light = crate::mascot::placeholder::send(
        mark_png(LogoMark::Kimi, LogoSize::Cells, Brightness::Light),
        0x4c_4f00 + LogoMark::Kimi as u32 * 2 + 1,
        (2, 1),
    );
    assert!(
        sent.windows(kimi_light.len())
            .any(|part| part == kimi_light)
    );
}

#[test]
fn larger_or_unknown_cells_take_the_double_pictures() {
    let double = mark_png(LogoMark::Codex, LogoSize::Cells2x, Brightness::Dark);
    for cell in [None, Some((20, 44))] {
        let sent = send_pictures(cell);
        let codex = crate::mascot::placeholder::send(double, 0x4c_4f02, (LOGO_WIDTH, 1));
        assert!(
            sent.windows(codex.len()).any(|part| part == codex),
            "{cell:?}"
        );
    }
}

/// What a row with a logo writes to the terminal: the picture's id as the foreground colour,
/// then its two placeholder cells, then the name in its own colour.
#[test]
fn a_row_writes_the_placeholders_in_the_pictures_colour() {
    use ratatui::layout::{Position, Size};
    use ratatui::text::Line;

    use crate::terminal::InlineTerminal;

    let theme = with_pictures(Brightness::Dark);
    let mut spans = logo_spans(&logo("codex", Some(LogoMark::Codex)), &theme);
    spans.push(Span::raw("Codex"));
    let mut terminal = InlineTerminal::new(Vec::new(), Size::new(12, 2), Position::new(0, 0));
    terminal
        .draw(1, |frame| frame.paint_lines(&[Line::from(spans)]))
        .unwrap();
    let written = String::from_utf8(terminal.writer().clone()).unwrap();
    let expected = format!("\x1b[38;2;76;79;2m{CODEX_CELLS}\x1b[39mCodex");
    assert!(written.contains(&expected), "{written:?}");

    let mut screen = vt100::Parser::new(2, 12, 0);
    screen.process(written.as_bytes());
    let first = screen.screen().cell(1, 0).unwrap();
    assert_eq!(first.contents(), "\u{10EEEE}\u{0305}\u{0305}");
    assert_eq!(first.fgcolor(), vt100::Color::Rgb(76, 79, 2));
    let second = screen.screen().cell(1, 1).unwrap();
    assert_eq!(second.contents(), "\u{10EEEE}\u{0305}\u{030D}");
    assert_eq!(screen.screen().cell(1, 2).unwrap().contents(), "C");
}

#[test]
fn a_row_with_its_own_mark_or_none_keeps_it_where_no_picture_draws() {
    let codex = logo("codex", Some(LogoMark::Codex));
    let ring = Span::raw("○ ");
    let dots = Theme::dark();
    assert_eq!(logo_or(&codex, &dots, ring.clone()), vec![ring.clone()]);
    assert!(logo_before(&codex, &dots).is_empty());

    assert!(logo_picture(&codex, &dots).is_empty());

    // A logo before a name keeps one blank before it, as every row's logo does.
    let pictures = with_pictures(Brightness::Dark);
    let picture = logo_spans(&codex, &pictures);
    assert_eq!(logo_picture(&codex, &pictures), picture);
    let mut before = picture.clone();
    before.push(Span::raw(" "));
    assert_eq!(logo_or(&codex, &pictures, ring.clone()), before);
    assert_eq!(logo_before(&codex, &pictures), before);

    let unknown = Logo::dot("lmstudio");
    assert_eq!(logo_or(&unknown, &pictures, ring.clone()), vec![ring]);
    assert!(logo_before(&unknown, &pictures).is_empty());
}

/// A name after a logo is worded for what the row shows: after Codex's picture a GPT model
/// drops its maker's word, and after the colour dot drawn where no picture does it stays whole.
#[test]
fn a_name_after_a_logo_follows_what_the_row_draws() {
    use argonauts_core::display_names::after_logo;

    let codex = logo("codex", Some(LogoMark::Codex));
    let mut theme = Theme::dark();
    assert_eq!(
        after_logo(&as_drawn(&codex, &theme), "GPT 5.6 Luna"),
        "GPT 5.6 Luna"
    );
    theme.logo_pictures = true;
    assert_eq!(as_drawn(&codex, &theme), codex);
    assert_eq!(
        after_logo(&as_drawn(&codex, &theme), "GPT 5.6 Luna"),
        "5.6 Luna"
    );
}

/// A label words its name by what its logo draws: after Codex's picture GPT 6 Luna reads
/// `6 Luna`, and beside the dot, or with nothing in the logo's place, it reads whole. The logo
/// keeps its cells either way, so the names line up whatever draws.
#[test]
fn a_label_drops_the_makers_word_only_where_its_logo_draws() {
    let codex = logo("codex", Some(LogoMark::Codex));
    let pictures = with_pictures(Brightness::Dark);
    let dots = Theme::dark();

    let drawn = ModelLabel::named(&codex, "GPT 6 Luna", &pictures, NoPicture::Dot);
    assert!(drawn.pictured);
    assert_eq!(drawn.name, "6 Luna");
    assert_eq!(drawn.logo, logo_before(&codex, &pictures));
    assert_eq!(spans_width(&drawn.logo), usize::from(LOGO_BEFORE));

    let dotted = ModelLabel::named(&codex, "GPT 6 Luna", &dots, NoPicture::Dot);
    assert!(!dotted.pictured);
    assert_eq!(dotted.name, "GPT 6 Luna");
    assert_eq!(spans_width(&dotted.logo), usize::from(LOGO_BEFORE));
    assert_eq!(
        dotted.width(),
        usize::from(LOGO_BEFORE) + "GPT 6 Luna".len()
    );

    let bare = ModelLabel::named(&codex, "GPT 6 Luna", &dots, NoPicture::Nothing);
    assert!(bare.logo.is_empty());
    assert_eq!(bare.name, "GPT 6 Luna");

    let ring = Span::raw("○ ");
    let marked = ModelLabel::named(&codex, "GPT 6 Luna", &dots, NoPicture::Mark(ring.clone()));
    assert_eq!(marked.logo, vec![ring]);
    assert_eq!(marked.name, "GPT 6 Luna");

    // A logo with no picture is a dot even where pictures draw, so its name stays whole.
    let unknown = Logo::dot("lmstudio");
    let local = ModelLabel::named(&unknown, "Qwen3 Coder 30B", &pictures, NoPicture::Dot);
    assert!(!local.pictured);
    assert_eq!(local.name, "Qwen3 Coder 30B");
}

/// A CLI's label keeps its whole name after its logo, where a model's would drop the maker's
/// word: `Claude Code`, never `Code`.
#[test]
fn a_clis_label_keeps_its_whole_name_where_its_logo_draws() {
    let names = DisplayNames::default();
    let pictures = with_pictures(Brightness::Dark);
    for cli in ["claude-code", "codex", "antigravity-cli", "opencode"] {
        let label = ModelLabel::cli(&names, cli, &pictures, NoPicture::Dot);
        assert_eq!(label.name, names.cli(cli), "{cli}");
        assert_eq!(
            label.logo,
            logo_before(&logo_for(cli, None), &pictures),
            "{cli}"
        );
        let dotted = ModelLabel::cli(&names, cli, &Theme::dark(), NoPicture::Nothing);
        assert!(dotted.logo.is_empty());
        assert_eq!(dotted.name, names.cli(cli));
    }
    let claude = logo_for("claude-code", None);
    let as_a_model = ModelLabel::named(&claude, "Claude Code", &pictures, NoPicture::Dot);
    assert_ne!(
        as_a_model.name, "Claude Code",
        "a model's form drops the maker's word"
    );
    let held = ModelLabel::cli_named("claude-code", "Claude Code", &pictures, NoPicture::Dot);
    assert_eq!(held.name, "Claude Code");
}

/// A worker's label takes the logo of whoever serves its model and the name the names give
/// it, drawn as one row: the logo's cells, then the name in its colour.
#[test]
fn a_workers_label_is_its_serving_logo_and_its_models_name() {
    let names = DisplayNames::default();
    let luna = ModelRef::new("codex", Some("gpt-6-luna".to_owned()));
    let pictures = with_pictures(Brightness::Dark);
    let whole = names.model("codex", Some("gpt-6-luna")).unwrap();

    let dotted = ModelLabel::of(&names, &luna, &Theme::dark(), NoPicture::Dot);
    assert_eq!(dotted.name, whole);
    let drawn = ModelLabel::of(&names, &luna, &pictures, NoPicture::Dot);
    assert_eq!(
        drawn.name,
        argonauts_core::display_names::after_logo(&logo("codex", Some(LogoMark::Codex)), &whole)
    );
    assert_ne!(drawn.name, whole);

    let style = Style::new().fg(Color::Rgb(0x6a, 0x9e, 0xe6));
    let spans = drawn.spans(style);
    assert_eq!(spans.last(), Some(&Span::styled(drawn.name.clone(), style)));
    assert_eq!(spans_width(&spans), drawn.width());

    // A worker that names no model reads as its CLI.
    let codex = ModelRef::new("codex", None);
    let cli = ModelLabel::of(&names, &codex, &Theme::dark(), NoPicture::Nothing);
    assert_eq!(cli.name, names.cli("codex"));
}

/// A session that names no model runs the one its CLI's settings give, so its logo is that
/// model's from the start, as its name is, and stays when the session reports it.
#[test]
fn a_workers_logo_is_the_model_it_runs_where_it_names_none() {
    let names = DisplayNames {
        configured: std::collections::BTreeMap::from([(
            "opencode".to_owned(),
            "ollama/qwen3-coder:30b-64k".to_owned(),
        )]),
        ..DisplayNames::default()
    };
    let pictures = with_pictures(Brightness::Dark);
    let unnamed = ModelRef::new("opencode", None);
    let reported = ModelRef::new("opencode", Some("ollama/qwen3-coder:30b-64k".to_owned()));
    let from_start = ModelLabel::of(&names, &unnamed, &pictures, NoPicture::Nothing);
    let once_reported = ModelLabel::of(&names, &reported, &pictures, NoPicture::Nothing);
    assert_eq!(from_start.whose.mark, Some(LogoMark::Ollama));
    assert_eq!(from_start, once_reported);
    assert_eq!(from_start.name, "Qwen3 Coder 30B 64K");
    assert_eq!(
        from_start.clone().beside_picture().as_deref(),
        Some("Qwen3 Coder 30B 64K")
    );
    let dotted = ModelLabel::of(&names, &unnamed, &Theme::dark(), NoPicture::Nothing);
    assert_eq!(dotted.beside_picture(), None);
}

/// A numbered logo is its logo's picture with a disc at the lower right that the number is cut
/// out of: the logo stays where the disc isn't, the disc takes the screen's text colour, and
/// the digit shows the screen through it. Each number makes its own picture.
#[test]
fn a_numbered_logo_cuts_its_number_out_of_a_disc_on_its_logo() {
    let size = LogoSize::Cells;
    let plain = png::decode(mark_png(LogoMark::Codex, size, Brightness::Dark)).unwrap();
    let two = numbered::picture(LogoMark::Codex, 2, size, Brightness::Dark).unwrap();
    let two = png::decode(&two).unwrap();
    assert_eq!((two.width, two.height), (plain.width, plain.height));
    let pixel = |image: &png::Rgba, x: usize, y: usize| {
        let at = (y * image.width + x) * 4;
        image.pixels[at..at + 4].to_vec()
    };
    // Far from the disc, the logo as it was.
    assert_eq!(pixel(&two, 10, 12), pixel(&plain, 10, 12));
    // The disc's centre is (28, 33); the digit's top row spans x 25 to 30 at y 28 and 29.
    assert_eq!(pixel(&two, 25, 28)[3], 0, "the digit's top row is cut out");
    assert_eq!(
        pixel(&two, 33, 33),
        vec![0xf2, 0xf2, 0xf2, 0xff],
        "the disc"
    );
    let light = numbered::picture(LogoMark::Codex, 2, size, Brightness::Light).unwrap();
    assert_eq!(
        pixel(&png::decode(&light).unwrap(), 33, 33),
        vec![0x24, 0x24, 0x24, 0xff],
        "the disc on a light screen"
    );
    let three = numbered::picture(LogoMark::Codex, 3, size, Brightness::Dark).unwrap();
    assert_ne!(png::decode(&three).unwrap(), two);
    assert!(numbered::picture(LogoMark::Codex, 10, size, Brightness::Dark).is_none());
}

/// Every numbered logo's picture has an id of its own, apart from every logo's, and says its
/// number back.
#[test]
fn numbered_logos_have_ids_of_their_own() {
    let mut ids = std::collections::BTreeSet::new();
    for mark in LogoMark::ALL {
        for on in BOTH {
            ids.insert(picture_id(mark, on));
        }
    }
    let plain = ids.len();
    let mut numbered_ids = 0;
    for mark in LogoMark::ALL {
        for number in 1..=numbered::MOST {
            for on in BOTH {
                let id = numbered::id(mark, number, on);
                assert_eq!(numbered::number_of(id), Some(number));
                assert!(id < 1 << 24, "{id:#x} fits a placeholder's colour");
                if ids.insert(id) {
                    numbered_ids += 1;
                }
            }
        }
    }
    let inks: usize = LogoMark::ALL
        .iter()
        .map(|mark| if one_colour(*mark) { 2 } else { 1 })
        .sum();
    assert_eq!(numbered_ids, inks * usize::from(numbered::MOST));
    assert!(ids.len() > plain);
    for mark in LogoMark::ALL {
        assert_eq!(
            numbered::number_of(picture_id(mark, Brightness::Dark)),
            None
        );
    }
}

/// Where logos draw as pictures, a numbered seat's badge is its numbered logo, which goes out
/// once, with the frame that first draws it. Elsewhere it is the number in a circle.
#[test]
fn a_numbered_logo_goes_out_once_after_a_screen_draws_it() {
    set_takes_logos(true);
    let theme = golden::pictured(&Theme::dark());
    let codex = logo("codex", Some(LogoMark::Codex));
    let style = Style::new();
    let badge = numbered_spans(&codex, 2, style, &theme);
    let id = numbered::id(LogoMark::Codex, 2, Brightness::Dark);
    assert_eq!(
        golden::council_text(&ratatui::text::Line::from(badge.clone())),
        "▐2"
    );
    assert_eq!(spans_width(&badge), usize::from(LOGO_WIDTH));
    let sent = send_numbered(Some((18, 42)));
    let picture = numbered::picture(LogoMark::Codex, 2, LogoSize::Cells, Brightness::Dark).unwrap();
    let wanted = crate::mascot::placeholder::send(&picture, id, (LOGO_WIDTH, 1));
    assert!(sent.windows(wanted.len()).any(|part| part == wanted));
    numbered_spans(&codex, 2, style, &theme);
    assert!(send_numbered(Some((18, 42))).is_empty(), "sent once");

    let plain = numbered_spans(&codex, 2, style, &Theme::dark());
    let text: String = plain.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "② ");
    set_takes_logos(false);
}

/// Logos held back while `ui.logos` was off go out once, before the frame after they're turned
/// on, and from then the terminal holds them; nothing held, nothing goes.
#[test]
fn logos_held_while_off_go_out_once_when_turned_on() {
    set_takes_logos(false);
    assert!(!send_held(), "nothing held");
    assert_eq!(held_pictures(None), None);
    hold_for_later();
    assert_eq!(held_pictures(None), None, "not before logos are turned on");
    assert!(send_held());
    let sent = held_pictures(Some((18, 42))).expect("the pictures");
    assert_eq!(sent, send_pictures(Some((18, 42))));
    assert!(takes_logos());
    assert_eq!(held_pictures(None), None, "once");
    assert!(!send_held());
    set_takes_logos(false);
}
