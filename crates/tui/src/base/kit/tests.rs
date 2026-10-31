use ratatui::layout::Rect;

use super::*;
use crate::base::stability;
use crate::base::theme::{ColorLevel, Glyphs};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn rows(drawn: &Drawn) -> Vec<String> {
    drawn
        .lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn meter_body(kit: &Kit<'_>, cells: usize) -> Drawn {
    let mut body = Drawn::new();
    body.push(vec![kit.strong("plan_usage")]);
    let meter = bars::block_meter(kit, 0.5, cells, Role::BarFill, Role::BarTrack);
    body.push_meter(meter, 0, cells, 1);
    body
}

#[test]
fn a_frame_carries_its_meters_to_screen_columns() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let framed = Frame::band(40).draw(&kit, meter_body(&kit, 30));
    assert_eq!(framed.meters, [Rect::new(3, 2, 30, 1)]);
    assert_eq!(rows(&framed)[0], format!(" ╭{}╮ [-]", "─".repeat(32)));
    assert_eq!(framed.width(), 39);
    stability::assert_drawn(&framed, 40);
}

#[test]
fn block_meters_stacked_without_a_row_between_fail_the_checker() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let mut slab = Drawn::new();
    for _ in 0..2 {
        let meter = bars::block_meter(&kit, 0.5, 20, Role::BarFill, Role::BarTrack);
        slab.push_meter(meter, 0, 20, 1);
    }
    assert!(!stability::check_drawn(&slab, 40).is_empty());
    // A title row between them, as Claude Code's /usage puts one, keeps them apart.
    let mut apart = meter_body(&kit, 20);
    apart.append(meter_body(&kit, 20));
    assert!(stability::check_drawn(&apart, 40).is_empty());
    // One meter drawn two rows high on purpose passes.
    let mut tall = Drawn::new();
    let meter = bars::block_meter(&kit, 0.5, 20, Role::BarFill, Role::BarTrack);
    tall.push_meter(meter, 0, 20, 2);
    assert!(stability::check_drawn(&tall, 40).is_empty());
}

#[test]
fn cards_stand_one_blank_row_apart() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let card = || Frame::view(40).draw(&kit, meter_body(&kit, 20));
    let stacked = stack_cards([card(), Drawn::new(), card()]);
    assert_eq!(stacked.height(), 4 + 1 + 4);
    assert_eq!(rows(&stacked)[4], "");
    assert_eq!(stacked.meters[1].y, 7);
    stability::assert_drawn(&stacked, 40);
}

#[test]
fn ascii_glyphs_draw_frames_and_marks_in_plain_characters() {
    let mut theme = plain();
    theme.glyphs = Glyphs::ASCII;
    let kit = Kit::new(&theme);
    let header = Header::new("limit", Role::Success);
    let mut body = Drawn::new();
    body.push(header.draw(&kit, 20));
    let framed = Frame::printed(28).draw(&kit, body);
    let text = rows(&framed).join("\n");
    assert!(text.is_ascii(), "{text}");
    assert!(text.contains("* limit"));
}

#[test]
fn spread_and_fit_keep_rows_their_width() {
    let left = vec![Span::raw("left")];
    let right = vec![Span::raw("right")];
    assert_eq!(spans_width(&spread(left, right, 20)), 20);
    assert_eq!(spans_width(&fit(vec![Span::raw("x".repeat(30))], 10)), 10);
    assert_eq!(spans_width(&fit(vec![Span::raw("x")], 10)), 10);
}
