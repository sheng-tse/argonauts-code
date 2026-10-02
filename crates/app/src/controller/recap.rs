//! Recaps and titles, built by code.

use argonauts_core::api::StatusSummary;
use argonauts_core::app::TurnReply;
use argonauts_core::events::card::CardState;
use argonauts_core::events::run::{RunMode, RunOutcome};

/// Longest title, in characters. The terminal cuts the rest anyway.
const TITLE_MAX: usize = 60;

/// Such as "Export now takes --json. All 4 cards merged, and the checks pass. Ask for another
/// change, or /resume an older run."
pub(crate) fn recap_from_summary(summary: &StatusSummary, outcome: Option<RunOutcome>) -> String {
    let goal = goal_line(summary);
    let cards = Cards::count(summary);
    let run = summary
        .run
        .as_ref()
        .map_or_else(|| "/resume".to_owned(), |run| format!("/resume {run}"));
    let rest = match outcome {
        Some(RunOutcome::Merged) => match cards.merged_phrase() {
            Some(merged) => format!(
                "{}, and the checks pass. Ask for another change, or /resume an older run.",
                capitalized(&merged)
            ),
            None => "It merged, and the checks pass. Ask for another change, or /resume an older \
                     run."
                .to_owned(),
        },
        Some(RunOutcome::PullRequest) => with_cards(
            &cards,
            "The work waits in a pull request for review.",
            "and the work waits in a pull request for review.",
        ),
        Some(RunOutcome::BranchLeft) => with_cards(
            &cards,
            &format!("The branches are kept for later. {run} picks it up."),
            &format!("and the branches are kept for later. {run} picks it up."),
        ),
        Some(RunOutcome::Abandoned) => "You abandoned the run, so nothing merged.".to_owned(),
        Some(RunOutcome::Stopped) => match cards.merged_phrase() {
            Some(merged) => format!("The run stopped with {merged}. {run} goes on from there."),
            None => format!("The run stopped. {run} goes on from there."),
        },
        Some(RunOutcome::Failed) => match cards.merged_phrase() {
            Some(merged) => format!("The run failed with {merged}. /status shows where."),
            None => "The run failed. /status shows where.".to_owned(),
        },
        None => in_progress(summary, &cards),
    };
    format!("{goal}. {rest}")
}

/// After a long chat turn.
pub(crate) fn recap_from_turn(request: &str, reply: &TurnReply) -> String {
    let title = title_from_request(request);
    match reply {
        TurnReply::Answer { .. } => format!("Answered \"{title}\"."),
        TurnReply::Change { triage, .. } => {
            format!(
                "Started a {} run for \"{title}\": {}.",
                mode_name(triage.mode),
                triage.reason
            )
        }
        TurnReply::Unclear { .. } => {
            format!("Answered \"{title}\", and a card asks whether to start work on it.")
        }
    }
}

/// The terminal title for a chat: its first request, shortened.
pub(crate) fn title_from_request(request: &str) -> String {
    let line = request
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let words: Vec<&str> = line.split_whitespace().collect();
    let joined = words.join(" ");
    if joined.chars().count() <= TITLE_MAX {
        return joined;
    }
    // Cut at a word when one ends close enough, so the title doesn't end mid-word.
    let mut title = String::new();
    for word in &words {
        let next = title.chars().count() + usize::from(!title.is_empty()) + word.chars().count();
        if next > TITLE_MAX - 1 {
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
    }
    if title.chars().count() < TITLE_MAX / 2 {
        title = joined.chars().take(TITLE_MAX - 1).collect();
    }
    title.push('…');
    title
}

pub(crate) fn mode_name(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Solo => "solo",
        RunMode::Crew => "crew",
    }
}

fn goal_line(summary: &StatusSummary) -> String {
    let goal = summary
        .goal
        .iter()
        .chain(summary.plan.as_ref())
        .map(|line| line.trim().trim_end_matches('.'))
        .find(|line| !line.is_empty());
    match goal {
        Some(goal) => capitalized(goal),
        None => match &summary.run {
            Some(run) => format!("Run {run}"),
            None => "The run".to_owned(),
        },
    }
}

struct Cards {
    merged: usize,
    working: usize,
    /// Cancelled cards don't count.
    total: usize,
}

impl Cards {
    fn count(summary: &StatusSummary) -> Self {
        let counted = || {
            summary
                .cards
                .iter()
                .filter(|card| card.state != CardState::Cancelled)
        };
        Self {
            merged: counted()
                .filter(|card| card.state == CardState::Integrated)
                .count(),
            working: counted()
                .filter(|card| {
                    matches!(
                        card.state,
                        CardState::Running
                            | CardState::Gating
                            | CardState::InReview
                            | CardState::Integrating
                    )
                })
                .count(),
            total: counted().count(),
        }
    }

    /// "all 4 cards merged", "the card merged", "2 of 5 cards merged", or `None` with no cards.
    fn merged_phrase(&self) -> Option<String> {
        match (self.merged, self.total) {
            (_, 0) => None,
            (1, 1) => Some("the card merged".to_owned()),
            (0, 1) => Some("the card isn't merged".to_owned()),
            (merged, total) if merged == total => Some(format!("all {total} cards merged")),
            (merged, total) => Some(format!("{merged} of {total} cards merged")),
        }
    }
}

fn with_cards(cards: &Cards, alone: &str, after: &str) -> String {
    match cards.merged_phrase() {
        Some(merged) => format!("{}, {after}", capitalized(&merged)),
        None => alone.to_owned(),
    }
}

fn in_progress(summary: &StatusSummary, cards: &Cards) -> String {
    let mut parts = Vec::new();
    if let Some(merged) = cards.merged_phrase() {
        parts.push(capitalized(&merged));
    }
    if cards.working > 0 {
        parts.push(format!("{} working", cards.working));
    }
    let mut text = if parts.is_empty() {
        "It's under way.".to_owned()
    } else {
        format!("{}.", parts.join(", "))
    };
    if !summary.waiting.is_empty() {
        // A card's title can end a sentence of its own, and one stop is enough.
        let waiting = summary.waiting.join(", ");
        text.push_str(&format!(
            " Waiting on {}.",
            waiting.trim().trim_end_matches('.')
        ));
    } else if let Some(last) = summary.recent.last() {
        text.push_str(&format!(" Last: {}.", last.trim().trim_end_matches('.')));
    }
    text
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::api::CardBrief;
    use argonauts_core::events::run::TriageDecision;
    use argonauts_core::{CardId, RunId};

    fn card(n: u32, state: CardState) -> CardBrief {
        CardBrief {
            card: CardId::numbered(n),
            title: format!("card {n}"),
            state,
            owner: None,
            note: String::new(),
        }
    }

    fn summary(cards: Vec<CardBrief>) -> StatusSummary {
        StatusSummary {
            run: Some(RunId::new("run-007").unwrap()),
            goal: vec!["export now takes --json.".to_owned(), "criteria".to_owned()],
            cards,
            ..StatusSummary::default()
        }
    }

    #[test]
    fn recap_from_status_summary() {
        let merged = summary((1..=4).map(|n| card(n, CardState::Integrated)).collect());
        assert_eq!(
            recap_from_summary(&merged, Some(RunOutcome::Merged)),
            "Export now takes --json. All 4 cards merged, and the checks pass. Ask for another \
             change, or /resume an older run."
        );
        let stopped = summary(vec![
            card(1, CardState::Integrated),
            card(2, CardState::Running),
            card(3, CardState::Cancelled),
        ]);
        assert_eq!(
            recap_from_summary(&stopped, Some(RunOutcome::Stopped)),
            "Export now takes --json. The run stopped with 1 of 2 cards merged. /resume run-007 \
             goes on from there."
        );
        let mut going = stopped.clone();
        going.waiting = vec!["the merge checkpoint".to_owned()];
        assert_eq!(
            recap_from_summary(&going, None),
            "Export now takes --json. 1 of 2 cards merged, 1 working. Waiting on the merge \
             checkpoint."
        );
        let mut escalated = summary(vec![card(1, CardState::Escalated)]);
        escalated.waiting = vec!["card-001 escalated · Fix add so it returns a + b.".to_owned()];
        assert_eq!(
            recap_from_summary(&escalated, None),
            "Export now takes --json. The card isn't merged. Waiting on card-001 escalated · Fix \
             add so it returns a + b."
        );
        let empty = StatusSummary::default();
        assert_eq!(
            recap_from_summary(&empty, Some(RunOutcome::Abandoned)),
            "The run. You abandoned the run, so nothing merged."
        );
        let one = summary(vec![card(1, CardState::Integrated)]);
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::PullRequest)),
            "Export now takes --json. The card merged, and the work waits in a pull request \
             for review."
        );
    }

    #[test]
    fn titles_are_the_first_line_shortened_at_a_word() {
        assert_eq!(title_from_request("  hi  "), "hi");
        assert_eq!(
            title_from_request("\n add   rate limiting\nto the API"),
            "add rate limiting"
        );
        let long = "how should we add rate limiting to the public API without breaking the \
                    clients that retry";
        let title = title_from_request(long);
        assert!(title.chars().count() <= TITLE_MAX, "{title}");
        assert!(title.ends_with("the…") || title.ends_with('…'), "{title}");
        assert!(long.starts_with(title.trim_end_matches('…')), "{title}");
        let one_word = "x".repeat(200);
        assert_eq!(title_from_request(&one_word).chars().count(), TITLE_MAX);
    }

    #[test]
    fn a_turn_recap_says_what_the_turn_became() {
        let triage = TriageDecision {
            mode: RunMode::Crew,
            escalate_to_crew: false,
            rule: "recipe:api-feature".to_owned(),
            reason: "the recipe for api-feature says crew".to_owned(),
        };
        let reply = TurnReply::Change {
            markdown: String::new(),
            estimate: serde_json::from_value(serde_json::json!({
                "task_type": "api-feature", "summary": "s", "files_likely": [], "file_count": 9,
                "risk_labels": [], "tests_exist": true, "unknowns": []
            }))
            .unwrap(),
            triage,
        };
        assert_eq!(
            recap_from_turn("add rate limiting", &reply),
            "Started a crew run for \"add rate limiting\": the recipe for api-feature says crew."
        );
    }
}
