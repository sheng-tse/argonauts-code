//! The summary under the map, in the app's own words: what the world is drawn from, then the six
//! behaviors used most with their counts, in two columns of three across the screen and in one
//! beside the map. It lists behaviors only: nature, the land's kinds and the wonders stay for the
//! pointer to find.

use argonauts_core::app::overview::OverviewPeriod;
use argonauts_core::behaviors::behavior_map::{BehaviorMap, Placing, Scope};
use ratatui::text::Span;

use super::hover::cap;
use crate::base::kit::Kit;
use crate::base::text::{At, cut, str_width};
use crate::base::theme::Role;

/// How many behaviors the summary lists.
pub const MOST_COMMON: usize = 6;
/// The blanks before a count, and between two columns, as between a table's columns.
const TO_COUNT: usize = 3;
const GAP: usize = 4;
/// The narrowest row that holds two columns: the screen's width under the map, where a column
/// beside the map is narrower.
const TWO_COLUMNS: usize = 115;

fn plural(count: usize, one: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        _ => format!("{count} {one}s"),
    }
}

/// What the map on screen is and what it's drawn from: `Label map · 562 actions in 19 runs across
/// 6 projects · last 30 days`, or `Summaries · 650 of 823 summarized by Qwen3 Coder 30B (64k) ·
/// 173 newer not yet`. With no action to show, where none were and what shows more. The
/// grouping shows on the selector at the right of the sub-tabs, so the line leaves it out.
pub(super) fn headline_words(map: &BehaviorMap) -> String {
    match Headline::of(map) {
        Some(headline) => headline.words(true, true),
        None => empty_words(map),
    }
}

/// The headline's parts, which give way in turn where the row is short: first what made the
/// map, then the counts. The map's name, what waits and the period always stay.
struct Headline {
    name: &'static str,
    /// `650 of 823`, or `562 actions`.
    counts: String,
    /// ` summarized by Qwen3 Coder 30B (64k)`, or ` in 19 runs across 6 projects`.
    made_by: String,
    /// ` · 173 newer not yet`.
    waiting: String,
    /// ` · last 30 days`.
    period: String,
}

impl Headline {
    fn of(map: &BehaviorMap) -> Option<Headline> {
        let request = map.request;
        if map.actions.is_empty() {
            return None;
        }
        let period = match request.period {
            OverviewPeriod::All => String::new(),
            other => format!(" · {}", other.title().to_lowercase()),
        };
        let actions = map.actions.len();
        if let Placing::Summaries { by } = &map.placing {
            let (counts, waiting) = match map.unsummarized {
                0 => (actions.to_string(), String::new()),
                waiting => (
                    format!("{actions} of {}", actions + waiting),
                    format!(" · {waiting} newer not yet"),
                ),
            };
            let words = if map.by_words {
                " · placed by word vectors"
            } else {
                ""
            };
            return Some(Headline {
                name: "Summaries",
                counts,
                made_by: format!(" summarized by {by}{words}"),
                waiting,
                period,
            });
        }
        let sources = match (map.runs, map.conversations) {
            (runs, 0) => plural(runs, "run"),
            (0, talks) => plural(talks, "conversation"),
            (runs, talks) => format!(
                "{} and {}",
                plural(runs, "run"),
                plural(talks, "conversation")
            ),
        };
        let place = match (request.scope, map.projects.as_slice()) {
            (Scope::ThisProject, _) => map.here.as_deref().map(|here| format!(" in {here}")),
            (Scope::Everywhere, [one]) => Some(format!(" in {one}")),
            (Scope::Everywhere, []) => None,
            (Scope::Everywhere, many) => Some(format!(" across {}", plural(many.len(), "project"))),
        };
        Some(Headline {
            name: "Label map",
            counts: plural(actions, "action"),
            made_by: format!(" in {sources}{}", place.unwrap_or_default()),
            waiting: String::new(),
            period,
        })
    }

    fn words(&self, made_by: bool, counts: bool) -> String {
        let mut out = self.name.to_owned();
        if counts {
            out.push_str(" · ");
            out.push_str(&self.counts);
            if made_by {
                out.push_str(&self.made_by);
            }
        }
        out.push_str(&self.waiting);
        out.push_str(&self.period);
        out
    }
}

/// `No actions in the last 7 days · r changes the period`.
fn empty_words(map: &BehaviorMap) -> String {
    let request = map.request;
    let place = match (request.scope, map.here.as_deref()) {
        (Scope::ThisProject, Some(here)) => format!(" in {here}"),
        _ => String::new(),
    };
    match (request.period, request.scope) {
        (OverviewPeriod::All, Scope::Everywhere) => {
            "No actions recorded yet: the map fills in as your agents work".to_owned()
        }
        (OverviewPeriod::All, Scope::ThisProject) => {
            format!("No actions recorded{place} yet · s shows every project")
        }
        (period, _) => format!(
            "No actions{place} in the {} · r changes the period",
            period.title().to_lowercase()
        ),
    }
}

/// The behaviors used most, most first, a tie in the order of their words.
pub(super) fn most_common(map: &BehaviorMap) -> Vec<(String, usize)> {
    let mut counted: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for action in &map.actions {
        *counted.entry(action.behavior.as_str()).or_default() += 1;
    }
    let mut counts: Vec<(&str, usize)> = counted.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    counts
        .into_iter()
        .take(MOST_COMMON)
        .map(|(behavior, count)| (behavior.to_owned(), count))
        .collect()
}

/// The headline as its row, one cell in, dim, in `width` cells: what made the map gives way
/// first, then the counts, so the period and what waits always show. Only a row too short for
/// what stays is cut, after a whole word.
pub(super) fn headline(kit: &Kit<'_>, map: &BehaviorMap, width: usize) -> Vec<Span<'static>> {
    vec![kit.dim(headline_in(map, width))]
}

fn headline_in(map: &BehaviorMap, width: usize) -> String {
    let Some(headline) = Headline::of(map) else {
        return cut(&format!(" {}", empty_words(map)), width, At::Word);
    };
    let fewest = format!(" {}", headline.words(false, false));
    [(true, true), (false, true)]
        .into_iter()
        .map(|(made_by, counts)| format!(" {}", headline.words(made_by, counts)))
        .find(|words| str_width(words) <= width)
        .unwrap_or_else(|| cut(&fewest, width, At::Word))
}

/// `Most common` and the behaviors `top` holds, from [`most_common`], in `room` rows, its
/// heading's included: down the first column and then the second where the row holds two, else
/// in one. A column is as wide as its longest
/// behavior, its counts right-aligned three blanks after it; a behavior is cut where the screen is
/// too narrow for it.
pub(super) fn most_common_rows(
    kit: &Kit<'_>,
    top: &[(String, usize)],
    width: usize,
    room: usize,
) -> Vec<Vec<Span<'static>>> {
    let rows = room.saturating_sub(1);
    let top: Vec<(String, String)> = top
        .iter()
        .map(|(behavior, count)| (cap(behavior), count.to_string()))
        .collect();
    if top.is_empty() || rows == 0 {
        return Vec::new();
    }
    let halves = top.split_at(top.len().div_ceil(2));
    let two = width >= TWO_COLUMNS && 1 + cells(halves.0) + GAP + cells(halves.1) <= width;
    let down = match two {
        true => halves.0.len().min(rows),
        false => top.len().min(rows),
    };
    let columns: Vec<Vec<(String, String)>> = top
        .chunks(down)
        .take(if two { 2 } else { 1 })
        .map(|column| fitted(column, width.saturating_sub(1)))
        .collect();
    let mut out = vec![vec![Span::styled(" Most common", kit.bold(Role::Text))]];
    for row in 0..down {
        let mut line = vec![Span::raw(" ")];
        for (at, column) in columns.iter().enumerate() {
            let Some((words, count)) = column.get(row) else {
                continue;
            };
            if at > 0 {
                line.push(Span::raw(" ".repeat(GAP)));
            }
            let (names, counts) = widths(column);
            let pad = names - str_width(words) + TO_COUNT + counts - str_width(count);
            line.push(Span::styled(
                format!("{words}{}", " ".repeat(pad)),
                kit.style(Role::Text),
            ));
            line.push(Span::styled(count.clone(), kit.style(Role::Text)));
        }
        out.push(line);
    }
    out
}

/// The widest behavior and the widest count of a column.
fn widths(column: &[(String, String)]) -> (usize, usize) {
    column
        .iter()
        .fold((0, 0), |(names, counts), (words, count)| {
            (names.max(str_width(words)), counts.max(str_width(count)))
        })
}

/// A column's cells, its counts included.
fn cells(column: &[(String, String)]) -> usize {
    let (names, counts) = widths(column);
    names + TO_COUNT + counts
}

/// A column with each behavior cut where the column would run past `width`.
fn fitted(column: &[(String, String)], width: usize) -> Vec<(String, String)> {
    let (_, counts) = widths(column);
    let room = width.saturating_sub(TO_COUNT + counts);
    column
        .iter()
        .map(|(words, count)| (cut(words, room, At::Word), count.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::overview::OverviewPeriod;
    use argonauts_core::behaviors::behavior_map::{BehaviorMap, MapRequest, Placing};

    use super::super::sample::Sample;
    use super::headline_in;

    /// The left column beside the map at 177 × 40.
    const BESIDE: usize = 89;

    fn map(period: OverviewPeriod, summaries: bool) -> BehaviorMap {
        let request = MapRequest {
            period,
            ..MapRequest::default()
        };
        let mut sample = Sample::new(request).model("Opus 5.5", "claude-code", "opus");
        for at in 0..650 {
            sample = sample.at(0, "read a source file", (0.1 + at as f32 / 1000.0, 0.5));
        }
        let mut map = sample.map;
        map.runs = 19;
        map.conversations = 4;
        map.projects = (0..6).map(|n| format!("project-{n}")).collect();
        if summaries {
            map.placing = Placing::Summaries {
                by: "Qwen3 Coder 30B (64k)".to_owned(),
            };
            map.unsummarized = 173;
        }
        map
    }

    #[test]
    fn summaries_name_their_summarizer_as_the_legend_does() {
        assert_eq!(
            headline_in(&map(OverviewPeriod::All, true), BESIDE),
            " Summaries · 650 of 823 summarized by Qwen3 Coder 30B (64k) · 173 newer not yet"
        );
    }

    /// What made the map gives way first, then the counts; the period and what waits stay, and
    /// nothing is cut.
    #[test]
    fn a_short_row_drops_what_made_the_map_then_the_counts_and_keeps_the_period() {
        for period in [
            OverviewPeriod::All,
            OverviewPeriod::Week,
            OverviewPeriod::Month,
        ] {
            let words = match period {
                OverviewPeriod::All => String::new(),
                other => format!(" · {}", other.title().to_lowercase()),
            };
            for summaries in [true, false] {
                let line = headline_in(&map(period, summaries), BESIDE);
                assert!(line.chars().count() <= BESIDE, "{line}");
                assert!(!line.contains('…'), "{line}");
                assert!(line.ends_with(&words), "{line}");
                if summaries {
                    assert!(line.contains(" · 173 newer not yet"), "{line}");
                }
            }
        }
        assert_eq!(
            headline_in(&map(OverviewPeriod::Month, true), BESIDE),
            " Summaries · 650 of 823 · 173 newer not yet · last 30 days"
        );
        assert_eq!(
            headline_in(&map(OverviewPeriod::Month, true), 45),
            " Summaries · 173 newer not yet · last 30 days"
        );
        assert_eq!(
            headline_in(&map(OverviewPeriod::Month, false), BESIDE),
            " Label map · 650 actions in 19 runs and 4 conversations across 6 projects · last 30 days"
        );
        assert_eq!(
            headline_in(&map(OverviewPeriod::Month, false), 60),
            " Label map · 650 actions · last 30 days"
        );
        assert_eq!(
            headline_in(&map(OverviewPeriod::Month, false), 30),
            " Label map · last 30 days"
        );
    }
}
