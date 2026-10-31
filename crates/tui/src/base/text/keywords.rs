//! The main agent CLI's keywords, drawn the way that CLI draws them. Claude Code colors each
//! letter of `ultrathink` in its rainbow, and in the input a lighter band sweeps across.

use std::cmp::Ordering;
use std::ops::Range;
use std::time::{Duration, Instant};

use argonauts_core::look::keywords::{self, Found, Keyword, KeywordLook};
use ratatui::style::Style;
use ratatui::text::Span;

use crate::base::draw::render::Clock;
use crate::base::theme::{Role, Theme};

/// Claude Code's shimmer takes a step every 50 ms of a clock that only ticks in its 16 ms
/// frames, so the band redraws every 64 ms and now and then moves two letters at once.
const STEP: Duration = Duration::from_millis(50);
const FRAME: Duration = Duration::from_millis(64);

/// The sweep runs this many characters before the first keyword and as many after the last.
const RUN_UP: usize = 10;

/// One letter's bytes in the text, and its style.
pub type Letter = (Range<usize>, Style);

/// The letters of every keyword in `text`, in order. With `sweep`'s clock the shimmer passes
/// over them, as in the input; without it they hold still, as the transcript draws them.
pub fn letters(
    text: &str,
    keywords: &[Keyword],
    theme: &Theme,
    sweep: Option<&Clock>,
) -> Vec<Letter> {
    if keywords.is_empty() || !theme.has_color() {
        return Vec::new();
    }
    let found = keywords::find(text, keywords);
    let glimmer = sweep
        .filter(|_| !theme.reduced_motion)
        .and_then(|clock| glimmer(text, &found, clock));
    let mut letters = Vec::new();
    for found in &found {
        match found.keyword.look {
            KeywordLook::Rainbow => {
                let first = chars_before(text, found.range.start);
                let word = &text[found.range.clone()];
                for (index, (offset, letter)) in word.char_indices().enumerate() {
                    let start = found.range.start + offset;
                    let lit =
                        glimmer.is_some_and(|at| ((first + index) as isize).abs_diff(at) <= 1);
                    letters.push((start..start + letter.len_utf8(), theme.rainbow(index, lit)));
                }
            }
            // The whole word is one run in one color, so a row draws it as one span.
            KeywordLook::Flat => letters.push((found.range.clone(), theme.style(Role::Ultracode))),
        }
    }
    letters
}

/// When the shimmer in `text` next moves, while there's one to move.
pub fn next_change(
    text: &str,
    keywords: &[Keyword],
    theme: &Theme,
    clock: &Clock,
) -> Option<Instant> {
    if keywords.is_empty() || !theme.has_color() || theme.reduced_motion {
        return None;
    }
    let shimmers = keywords::find(text, keywords)
        .iter()
        .any(|found| found.keyword.look == KeywordLook::Rainbow);
    shimmers.then(|| clock.next_frame_at(FRAME))
}

/// The style of the letter at byte `offset`, if a keyword has one there.
pub fn style_at(letters: &[Letter], offset: usize) -> Option<Style> {
    letters
        .binary_search_by(|(range, _)| {
            if range.end <= offset {
                Ordering::Less
            } else if range.start > offset {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .ok()
        .map(|index| letters[index].1)
}

/// Draws `letters` over spans whose text, end to end, is the text the letters were found in.
pub fn overlay(spans: Vec<Span<'static>>, letters: &[Letter]) -> Vec<Span<'static>> {
    if letters.is_empty() {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len() + letters.len());
    let mut offset = 0;
    for span in spans {
        let text = span.content.as_ref();
        let end = offset + text.len();
        let mut from = 0;
        for (range, style) in letters
            .iter()
            .filter(|(range, _)| range.start >= offset && range.end <= end)
        {
            let start = range.start - offset;
            if start > from {
                out.push(Span::styled(text[from..start].to_owned(), span.style));
            }
            let stop = range.end - offset;
            out.push(Span::styled(
                text[start..stop].to_owned(),
                span.style.patch(*style),
            ));
            from = stop;
        }
        if from == 0 {
            out.push(span);
        } else if from < text.len() {
            out.push(Span::styled(text[from..].to_owned(), span.style));
        }
        offset = end;
    }
    out
}

fn chars_before(text: &str, offset: usize) -> usize {
    text[..offset].chars().count()
}

/// The character position the band is centered on now. It lights that letter and one on each
/// side, sweeping from before the first rainbow keyword to past the last, then starting over.
/// Before the text starts it lights nothing, or only the first letter.
fn glimmer(text: &str, found: &[Found<'_>], clock: &Clock) -> Option<isize> {
    let mut rainbow = found
        .iter()
        .filter(|found| found.keyword.look == KeywordLook::Rainbow);
    let first = rainbow.next()?;
    let last = rainbow.next_back().unwrap_or(first);
    let first = chars_before(text, first.range.start);
    let last = chars_before(text, last.range.end);
    let cycle = (last - first + 2 * RUN_UP) as u128;
    let frames = clock.since(clock.start).as_millis() / FRAME.as_millis();
    let steps = frames * FRAME.as_millis() / STEP.as_millis();
    let step = isize::try_from(steps % cycle).ok()?;
    Some(isize::try_from(first).ok()? - RUN_UP as isize + step)
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::base::theme::ColorLevel;

    const ULTRATHINK: Keyword = Keyword::new("ultrathink", KeywordLook::Rainbow, true);
    const PROMPT: &str = "please ultrathink about this";

    fn clock_at(millis: u64) -> Clock {
        let start = Instant::now();
        Clock::new(start, start + Duration::from_millis(millis))
    }

    fn lit(letters: &[Letter], theme: &Theme) -> Vec<usize> {
        letters
            .iter()
            .enumerate()
            .filter(|(index, (_, style))| *style == theme.rainbow(*index, true))
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn each_letter_takes_the_next_color() {
        let theme = Theme::dark();
        let letters = letters(PROMPT, &[ULTRATHINK], &theme, None);
        assert_eq!(letters.len(), 10);
        assert_eq!(letters[0], (7..8, theme.rainbow(0, false)));
        assert_eq!(letters[7], (14..15, theme.rainbow(0, false)));
        assert_eq!(letters[9], (16..17, theme.rainbow(2, false)));
    }

    #[test]
    fn the_band_sweeps_three_letters_wide_from_ten_before_to_ten_after() {
        let theme = Theme::dark();
        // The word starts at character 7, so the sweep starts at -3 and lights nothing yet.
        let at = |millis| {
            lit(
                &letters(PROMPT, &[ULTRATHINK], &theme, Some(&clock_at(millis))),
                &theme,
            )
        };
        assert!(at(0).is_empty());
        // 640 ms is ten frames, twelve steps: the band's middle is on character 9, the `t`.
        assert_eq!(at(640), [1, 2, 3]);
        // A cycle is thirty steps, the word's ten letters and twenty more, and 24 s is a whole
        // number of both cycles and frames.
        assert_eq!(at(640 + 24_000), at(640));
        assert!(at(64 * 30).is_empty());
    }

    #[test]
    fn a_band_centered_just_before_the_text_lights_its_first_letter() {
        let theme = Theme::dark();
        // 31 frames are 39 steps, nine into the second cycle: the band's middle is at -1.
        let letters = letters(
            "ultrathink",
            &[ULTRATHINK],
            &theme,
            Some(&clock_at(64 * 31)),
        );
        assert_eq!(lit(&letters, &theme), [0]);
    }

    #[test]
    fn still_letters_never_shimmer() {
        let theme = Theme::dark();
        let mut still = theme.clone();
        still.reduced_motion = true;
        assert!(lit(&letters(PROMPT, &[ULTRATHINK], &theme, None), &theme).is_empty());
        let clock = clock_at(640);
        assert!(
            lit(
                &letters(PROMPT, &[ULTRATHINK], &still, Some(&clock)),
                &still
            )
            .is_empty()
        );
        assert_eq!(next_change(PROMPT, &[ULTRATHINK], &still, &clock), None);
    }

    #[test]
    fn the_shimmer_asks_for_frames_only_while_a_keyword_shows() {
        let theme = Theme::dark();
        let clock = clock_at(100);
        assert_eq!(
            next_change(PROMPT, &[ULTRATHINK], &theme, &clock),
            Some(clock.start + FRAME * 2)
        );
        assert_eq!(
            next_change("please think", &[ULTRATHINK], &theme, &clock),
            None
        );
        assert_eq!(next_change(PROMPT, &[], &theme, &clock), None);
    }

    #[test]
    fn without_color_nothing_is_drawn() {
        let plain = Theme::dark().with_level(ColorLevel::None);
        assert!(letters(PROMPT, &[ULTRATHINK], &plain, None).is_empty());
    }

    #[test]
    fn overlay_splits_spans_and_keeps_their_other_styles() {
        let theme = Theme::dark();
        let base = Style::new().fg(Color::White);
        let spans = vec![
            Span::styled("please ultra", base),
            Span::styled("think about this", base),
        ];
        let letters = letters(PROMPT, &[ULTRATHINK], &theme, None);
        let out = overlay(spans, &letters);
        let text: String = out.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, PROMPT);
        assert_eq!(out[0], Span::styled("please ", base));
        assert_eq!(out[1].style, base.patch(theme.rainbow(0, false)));
        assert_eq!(out.last().unwrap(), &Span::styled(" about this", base));
        assert_eq!(style_at(&letters, 16), Some(theme.rainbow(9, false)));
        assert_eq!(style_at(&letters, 17), None);
    }
}
