//! Summaries' own rows in the Behaviors tab while an update runs: the line under the
//! sub-tabs saying who summarizes, how far the update got, and the note under the map. The map
//! that's made is drawn by the map's own view, as every map is.

use argonauts_core::behaviors::deep_map::{DeepDone, DeepProgress};
use ratatui::text::{Line, Span};

use crate::base::kit::Kit;
use crate::base::text::{At, cut, cut_spans, sanitize_line, str_width};
use crate::base::theme::Role;

/// The tab's margin, as the rows stand alone; the panel's inset is one of its cells.
const MARGIN: &str = "  ";

/// `Summaries · 562 actions · summarizing with Qwen3 Coder 30B on Ollama`.
pub fn summarizing(kit: &Kit<'_>, actions: usize, summarizer: &str) -> Line<'static> {
    Line::from(vec![kit.dim(format!(
        "{MARGIN}Summaries · {actions} actions · summarizing with {summarizer}"
    ))])
}

/// `Summaries · downloading Qwen3.5 27B on Ollama first`.
pub fn downloading(kit: &Kit<'_>, summarizer: &str) -> Line<'static> {
    Line::from(vec![kit.dim(format!(
        "{MARGIN}Summaries · downloading {summarizer} first"
    ))])
}

/// `✻ Summarizing… 312 of 562 · about 5 min left · esc keeps it going in the background · x stops it`,
/// or `✻ Downloading… 4.1 of 17 GB · …` while the summarizer downloads.
pub fn progress(kit: &Kit<'_>, progress: &DeepProgress) -> Line<'static> {
    let warn = kit.style(Role::Warn);
    let left = progress
        .seconds_left
        .map(|seconds| format!("about {} left · ", left_words(seconds)))
        .unwrap_or_default();
    // Fonts such as Iosevka draw ✻ and the ellipsis two cells wide, over the cell after
    // them, so each has two blanks after it and one always shows, as the arrows do.
    Line::from(vec![
        Span::styled(format!("{MARGIN}✻  "), warn),
        Span::styled(
            match progress.downloading {
                true => "Downloading…  ",
                false => "Summarizing…  ",
            },
            warn,
        ),
        kit.dim(match progress.downloading {
            true => format!(
                "{:.1} of {:.0} GB · ",
                progress.done as f64 / 1000.0,
                progress.total as f64 / 1000.0
            ),
            false => format!("{} of {} · {left}", progress.done, progress.total),
        }),
        kit.span("esc", Role::Question),
        kit.dim(" keeps it going in the background · "),
        kit.span("x", Role::Question),
        kit.dim(" stops it"),
    ])
}

/// `50 s`, to the nearest ten, then whole minutes, so 55 seconds reads `1 min`.
fn left_words(seconds: u64) -> String {
    let tens = (seconds + 5) / 10 * 10;
    if tens < 60 {
        format!("{tens} s")
    } else {
        format!("{} min", (seconds + 30) / 60)
    }
}

/// How an update ended, in a sentence.
pub fn ended(done: &DeepDone) -> String {
    let made = format!(
        "{} summarized now, {} like an identical action, {} already done",
        done.written, done.reused, done.already
    );
    if done.stopped {
        return format!("Stopped: {made}. Every summary written is kept.");
    }
    if let Some(fewer) = done.fewer {
        return format!(
            "The map is as it was, since the new one had {} of its {} dots: {made}. Every \
             summary written is kept.",
            fewer.made, fewer.shown
        );
    }
    if let Some(why) = &done.failed {
        return format!(
            "The update stopped: {}. {made}. Every summary written is kept.",
            sanitize_line(why.trim().trim_end_matches('.'))
        );
    }
    let words = if done.by_words {
        ", placed by their words"
    } else {
        ""
    };
    let waits = match &done.embedder_failed {
        Some(why) if !done.by_words => format!(
            " The embedder didn't answer ({why}), so the new summaries wait for the next update."
        ),
        _ => String::new(),
    };
    format!(
        "Made: {made}. {} dots, {} of them new{words}.{waits}",
        done.placed, done.new_places
    )
}

/// The two lines under the headline after an update that left the map as it was, in `width`:
/// why it stopped, such as `Update stopped after 0 of 88 summaries: Haiku 5.5 couldn't start,
/// since a CLI session needs a sandbox.`, then what was kept. `total` is the summaries the
/// update was making, `None` while none was asked for yet, as while the summarizer downloads.
pub fn kept(
    kit: &Kit<'_>,
    done: &DeepDone,
    summarizer: &str,
    total: Option<usize>,
    redo: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let kind = if redo { "Redo" } else { "Update" };
    let who = if summarizer.is_empty() {
        "the summarizer"
    } else {
        summarizer
    };
    let after = total
        .map(|total| format!(" after {} of {total} summaries", done.written))
        .unwrap_or_default();
    let why = done
        .failed
        .as_deref()
        .map(|why| sanitize_line(why.trim().trim_end_matches('.')).into_owned())
        .filter(|why| !why.is_empty());
    let (stopped, how) = if done.stopped {
        (
            format!(" {kind} stopped:"),
            format!(" you stopped it{after}."),
        )
    } else if let Some(fewer) = done.fewer {
        (
            format!(" {kind} stopped:"),
            format!(
                " its new map had {} dots, fewer than this map's {}.",
                fewer.made, fewer.shown
            ),
        )
    } else if let Some(why) = why {
        match after.is_empty() {
            true => (format!(" {kind} stopped:"), format!(" {why}.")),
            false => (format!(" {kind} stopped"), format!("{after}: {why}.")),
        }
    } else {
        (
            format!(" {kind} stopped:"),
            format!(" {who} didn't answer{after}."),
        )
    };
    let kept = match done.written {
        0 => "The map is as it was.".to_owned(),
        written => format!("The map is as it was, and the {written} are kept for the next try."),
    };
    let how = whole_sentences(&how, width.saturating_sub(str_width(&stopped)));
    let first = [Span::styled(stopped, kit.style(Role::Warn)), kit.dim(how)];
    vec![
        Line::from(cut_spans(&first, width, At::Word)),
        Line::from(vec![kit.dim(cut(&format!(" {kept}"), width, At::Word))]),
    ]
}

/// `words` whole where they fit in `cells`, else as many of their first sentences as fit, so
/// a reason too long for the row ends where one of its sentences does. The first sentence
/// stays even where it doesn't fit, for the cut to shorten.
fn whole_sentences(words: &str, cells: usize) -> String {
    if str_width(words) <= cells {
        return words.to_owned();
    }
    let ends = words.match_indices(". ").map(|(at, _)| at + 1);
    let fits = ends
        .take_while(|&end| str_width(&words[..end]) <= cells)
        .last();
    match fits {
        Some(end) => words[..end].to_owned(),
        None => words.to_owned(),
    }
}

/// Under the map while it fills in.
pub fn arriving(kit: &Kit<'_>) -> Line<'static> {
    Line::from(vec![kit.dim(format!(
        "{MARGIN}The dots arrive as their summaries do; the list fills in when it's done."
    ))])
}

// The running rows are compared with their picture through the whole panel, in the map's
// goldens.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_left_reads_in_seconds_then_minutes() {
        assert_eq!(left_words(50), "50 s");
        assert_eq!(left_words(47), "50 s");
        assert_eq!(left_words(130), "2 min");
    }

    #[test]
    fn an_update_says_how_it_ended() {
        let done = DeepDone {
            written: 312,
            reused: 9,
            already: 241,
            placed: 562,
            new_places: 321,
            ..DeepDone::default()
        };
        assert_eq!(
            ended(&done),
            "Made: 312 summarized now, 9 like an identical action, 241 already done. 562 dots, \
             321 of them new."
        );
        let stopped = DeepDone {
            stopped: true,
            ..done.clone()
        };
        assert!(ended(&stopped).starts_with("Stopped: 312 summarized now"));
        let waiting = DeepDone {
            new_places: 0,
            embedder_failed: Some("Ollama isn't running".to_owned()),
            ..done
        };
        assert!(
            ended(&waiting).ends_with(
                "0 of them new. The embedder didn't answer (Ollama isn't running), so the new \
                 summaries wait for the next update."
            ),
            "{}",
            ended(&waiting)
        );
    }

    #[test]
    fn a_new_map_with_fewer_dots_says_the_map_was_kept_and_why() {
        let theme = crate::base::theme::Theme::default();
        let kit = Kit::new(&theme);
        let done = DeepDone {
            written: 120,
            placed: 823,
            fewer: Some(argonauts_core::behaviors::deep_map::FewerDots {
                made: 120,
                shown: 823,
            }),
            ..DeepDone::default()
        };
        let lines: Vec<String> = kept(&kit, &done, "Haiku 5.5", Some(120), true, 117)
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert_eq!(
            lines,
            [
                " Redo stopped: its new map had 120 dots, fewer than this map's 823.",
                " The map is as it was, and the 120 are kept for the next try.",
            ]
        );
        assert!(ended(&done).starts_with("The map is as it was, since the new one had 120"));
    }

    fn kept_rows(done: &DeepDone, total: Option<usize>, width: usize) -> Vec<String> {
        let theme = crate::base::theme::Theme::default();
        let kit = Kit::new(&theme);
        kept(&kit, done, "Haiku 5.5", total, false, width)
            .iter()
            .map(|line| line.to_string())
            .collect()
    }

    /// What a refused start holds: a reason of two sentences.
    const REFUSED: &str = "Haiku 5.5 couldn't start, since a CLI session needs a sandbox. Only \
        sandbox.mode = \"tool\" or \"none\", which you set yourself, lets one run without ours";

    #[test]
    fn a_failed_update_says_why_after_how_far_it_got() {
        let refused = DeepDone {
            failed: Some(REFUSED.to_owned()),
            ..DeepDone::default()
        };
        assert_eq!(
            kept_rows(&refused, Some(88), 300),
            [
                format!(" Update stopped after 0 of 88 summaries: {REFUSED}."),
                " The map is as it was.".to_owned(),
            ]
        );
        let unanswered = DeepDone {
            written: 40,
            failed: Some("Haiku 5.5 stopped without an answer.".to_owned()),
            ..DeepDone::default()
        };
        assert_eq!(
            kept_rows(&unanswered, Some(823), 117),
            [
                " Update stopped after 40 of 823 summaries: Haiku 5.5 stopped without an answer.",
                " The map is as it was, and the 40 are kept for the next try.",
            ]
        );
        let stopped = DeepDone {
            stopped: true,
            ..unanswered
        };
        assert_eq!(
            kept_rows(&stopped, Some(823), 117)[0],
            " Update stopped: you stopped it after 40 of 823 summaries."
        );
    }

    #[test]
    fn a_summarizer_that_didnt_download_asked_for_no_summary() {
        let done = DeepDone {
            failed: Some(
                "Qwen3.5 27B didn't download (pull model manifest: file does not exist)".into(),
            ),
            ..DeepDone::default()
        };
        assert_eq!(
            kept_rows(&done, None, 117),
            [
                " Update stopped: Qwen3.5 27B didn't download (pull model manifest: file does not \
                 exist).",
                " The map is as it was.",
            ]
        );
    }

    /// The panel's own rows at the real sizes are held in its drawings' tests.
    #[test]
    fn a_long_reason_gives_way_a_sentence_at_a_time_then_at_a_word() {
        let refused = DeepDone {
            failed: Some(REFUSED.to_owned()),
            ..DeepDone::default()
        };
        let first = " Update stopped after 0 of 88 summaries: Haiku 5.5 couldn't start, since a CLI \
            session needs a sandbox.";
        for (width, said) in [
            (
                262,
                format!(" Update stopped after 0 of 88 summaries: {REFUSED}."),
            ),
            (117, first.to_owned()),
            (
                77,
                " Update stopped after 0 of 88 summaries: Haiku 5.5 couldn't start, since a…"
                    .to_owned(),
            ),
        ] {
            let rows = kept_rows(&refused, Some(88), width);
            assert_eq!(rows[0], said, "{width}");
            assert!(str_width(&rows[0]) < width, "{width}: {}", rows[0]);
        }
    }

    #[test]
    fn the_time_left_reads_in_tens_of_seconds_then_minutes() {
        assert_eq!(left_words(50), "50 s");
        assert_eq!(left_words(54), "50 s");
        assert_eq!(left_words(55), "1 min");
        assert_eq!(left_words(300), "5 min");
    }
}
