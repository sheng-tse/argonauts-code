//! The deeper map's own rows in the Behaviors tab while an update runs: the line under the
//! sub-tabs saying who summarizes, how far the update got, and the note under the map. The map
//! that's made is drawn by the map's own view, as every map is.

use argonauts_core::deep_map::{DeepDone, DeepProgress};
use ratatui::text::{Line, Span};

use crate::kit::Kit;
use crate::theme::Role;

/// The tab's margin, as the rows stand alone; the panel's inset is one of its cells.
const MARGIN: &str = "  ";

/// `Deeper map · 562 actions · summarizing with Qwen3 Coder 30B on Ollama`.
pub fn summarizing(kit: &Kit<'_>, actions: usize, summarizer: &str) -> Line<'static> {
    Line::from(vec![kit.dim(format!(
        "{MARGIN}Deeper map · {actions} actions · summarizing with {summarizer}"
    ))])
}

/// `✻ Summarizing… 312 of 562 · about 5 min left · esc keeps it going in the background · x stops it`.
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
        Span::styled("Summarizing…  ", warn),
        kit.dim(format!("{} of {} · {left}", progress.done, progress.total)),
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
    if let Some(why) = &done.failed {
        return format!(
            "The summarizer stopped answering ({why}): {made}. Every summary written is kept."
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
    fn the_time_left_reads_in_tens_of_seconds_then_minutes() {
        assert_eq!(left_words(50), "50 s");
        assert_eq!(left_words(54), "50 s");
        assert_eq!(left_words(55), "1 min");
        assert_eq!(left_words(300), "5 min");
    }
}
