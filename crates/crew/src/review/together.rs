//! Several fallbacks in one reviewer's place (`[fallback.reviewer]` with `together` of 2 or
//! more). When the reviewer is out as its review starts, that many fallbacks review at once
//! in its place. When it stops midway and one fallback takes its place, the rest join once
//! that one has answered. `pick` says how their reviews count: `merge` folds them into one
//! review that keeps every finding, and `judge` leaves them apart, so the judge settles
//! them when they disagree. A review passes no checks, so `first_passing` keeps
//! every finding too.

use argonauts_core::TaskId;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::task::ReviewEvent;
use argonauts_core::settings::fallback::Pick;
use argonauts_core::workflows::steps::judgment::{ReviewComment, ReviewOutput, ReviewVerdict};
use argonauts_core::{Note, NoteLevel};

use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::engine::job_fallback::{self, Job};
use crate::fallback::rules;
use crate::roles::display_name;
use crate::sessions::labels;

/// How many fallbacks take `reviewer`'s place at once, and how their reviews count. `None`
/// when its place goes to one fallback at a time.
pub(super) fn rules_for(host: &Host, reviewer: &ModelRef) -> Option<(usize, Pick)> {
    let applied = rules::applied(host, RoleName::Reviewer, &reviewer.agent_id());
    let together = usize::try_from(applied.together).unwrap_or(usize::MAX);
    let pick = applied
        .pick
        .unwrap_or(Pick::default_for(RoleName::Reviewer));
    (together >= 2).then_some((together, pick))
}

/// The fallbacks that review in `reviewer`'s place from the start, when it's out already and
/// its rules put several in its place. Empty when its own review is asked as ever.
pub(super) async fn from_the_start(host: &Host, job: &Job, reviewer: &ModelRef) -> Vec<ModelRef> {
    let Some((together, _)) = rules_for(host, reviewer) else {
        return Vec::new();
    };
    if !job_fallback::is_out(host, reviewer).await {
        return Vec::new();
    }
    job_fallback::together(host, job, reviewer, &[], together)
}

/// The fallbacks that join `first`, which took `reviewer`'s place when it stopped midway.
pub(super) fn joining(
    host: &Host,
    job: &Job,
    reviewer: &ModelRef,
    first: &ModelRef,
) -> Vec<ModelRef> {
    match rules_for(host, reviewer) {
        Some((together, _)) => job_fallback::together(host, job, reviewer, &[first], together - 1),
        None => Vec::new(),
    }
}

/// The review `by` already gave in this round, found when the step is entered again.
pub(super) fn given(host: &Host, task: &TaskId, round: u32, by: &ModelRef) -> Option<ReviewOutput> {
    host.read(|state| {
        let reviews = &state.tasks.get(task)?.reviews;
        reviews.iter().find_map(|event| match event {
            ReviewEvent::Reviewed {
                reviewer,
                round: r,
                review,
                ..
            } if *r == round && reviewer == by => Some(review.clone()),
            _ => None,
        })
    })
}

/// `Sonnet 5.5 on Claude Code and GPT 5.6 on Codex both review build:add-due in place of
/// Gemini 3.6 Pro on agy`, and how their reviews count. `more` are about to be
/// asked beside `already`, who have the job. Says nothing when the log has the review of
/// each of `more`, since it was said then.
pub(super) fn say_who_reviews(
    host: &Host,
    task: &TaskId,
    round: u32,
    reviewer: &ModelRef,
    more: &[ModelRef],
    already: &[&ModelRef],
) -> EngineResult<()> {
    if more.iter().all(|by| given(host, task, round, by).is_some()) {
        return Ok(());
    }
    let label = host.read(|state| labels::task_label(state, task));
    let names: Vec<String> = already
        .iter()
        .copied()
        .chain(more)
        .map(display_name)
        .collect();
    let who = match names.as_slice() {
        [] | [_] => return Ok(()),
        [one, other] => format!("{one} and {other} both review"),
        [rest @ .., last] => format!("{} and {last} all review", rest.join(", ")),
    };
    let pick = rules_for(host, reviewer).map_or(Pick::Merge, |(_, pick)| pick);
    let how = match pick {
        Pick::Judge => "the judge settles them if they disagree",
        Pick::Merge | Pick::FirstPassing => "every finding from each is kept",
    };
    let text = format!(
        "{who} {label} in place of {} · {how} · /fallback changes this",
        display_name(reviewer)
    );
    let level = NoteLevel::Info;
    host.record(Some(task), None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// Every reviewer's reviews as they count toward the verdict, reviewer by reviewer in the
/// order each was first reviewed, those of the fallbacks that took a reviewer's place with it.
pub(super) fn counted_by_reviewer(
    host: &Host,
    reviews: Vec<(ModelRef, ModelRef, ReviewOutput)>,
) -> Vec<ReviewOutput> {
    let mut by_reviewer: Vec<(ModelRef, Vec<ReviewOutput>)> = Vec::new();
    for (asked, _, review) in reviews {
        match by_reviewer.iter_mut().find(|(known, _)| *known == asked) {
            Some((_, theirs)) => theirs.push(review),
            None => by_reviewer.push((asked, vec![review])),
        }
    }
    by_reviewer
        .into_iter()
        .flat_map(|(asked, theirs)| match rules_for(host, &asked) {
            Some((_, pick)) => counted(theirs, pick),
            None => theirs,
        })
        .collect()
}

/// What one reviewer's reviews count as: one review with every finding, or each as it is for the
/// judge to settle.
pub(super) fn counted(reviews: Vec<ReviewOutput>, pick: Pick) -> Vec<ReviewOutput> {
    match pick {
        Pick::Judge => reviews,
        Pick::Merge | Pick::FirstPassing if reviews.len() < 2 => reviews,
        Pick::Merge | Pick::FirstPassing => vec![merged(&reviews)],
    }
}

/// One review out of several: the most severe verdict, every summary, and every comment, a
/// repeat folded into the first that made it.
fn merged(reviews: &[ReviewOutput]) -> ReviewOutput {
    let severity = |verdict: ReviewVerdict| match verdict {
        ReviewVerdict::Approve => 0,
        ReviewVerdict::RequestChanges => 1,
        ReviewVerdict::Reject => 2,
    };
    let verdict = reviews
        .iter()
        .map(|review| review.verdict)
        .max_by_key(|verdict| severity(*verdict))
        .unwrap_or(ReviewVerdict::Approve);
    let mut summaries: Vec<&str> = Vec::new();
    let mut comments: Vec<ReviewComment> = Vec::new();
    for review in reviews {
        let summary = review.summary.trim();
        if !summary.is_empty() && !summaries.iter().any(|kept| same_words(kept, summary)) {
            summaries.push(summary);
        }
        for comment in &review.comments {
            let repeat = comments.iter().any(|kept| {
                kept.path == comment.path
                    && kept.line == comment.line
                    && same_words(&kept.text, &comment.text)
            });
            if !repeat {
                comments.push(comment.clone());
            }
        }
    }
    ReviewOutput {
        verdict,
        summary: summaries.join(" "),
        comments,
        tamper_suspected: reviews.iter().any(|review| review.tamper_suspected),
    }
}

/// The same finding, whatever its case and spacing.
fn same_words(a: &str, b: &str) -> bool {
    let words =
        |text: &str| -> Vec<String> { text.split_whitespace().map(str::to_lowercase).collect() };
    words(a) == words(b)
}

#[cfg(test)]
mod tests {
    use argonauts_core::workflows::steps::plan::Severity;

    use super::*;

    fn comment(path: &str, line: u32, text: &str) -> ReviewComment {
        ReviewComment {
            path: path.to_owned(),
            line: Some(line),
            severity: Severity::High,
            text: text.to_owned(),
        }
    }

    fn review(verdict: ReviewVerdict, summary: &str, comments: Vec<ReviewComment>) -> ReviewOutput {
        ReviewOutput {
            verdict,
            summary: summary.to_owned(),
            comments,
            tamper_suspected: false,
        }
    }

    #[test]
    fn merge_keeps_every_finding_once_and_the_most_severe_verdict() {
        let missing_test = comment("src/due.rs", 12, "The due date needs a test.");
        let first = review(
            ReviewVerdict::Approve,
            "Reads well.",
            vec![missing_test.clone()],
        );
        let second = review(
            ReviewVerdict::RequestChanges,
            "A case is missing.",
            vec![
                comment("src/due.rs", 12, "the due date  needs a test."),
                comment("src/due.rs", 30, "An empty date panics."),
            ],
        );
        let reviews = vec![first, second];
        let counted_as = counted(reviews.clone(), Pick::Merge);
        assert_eq!(counted_as.len(), 1);
        let one = &counted_as[0];
        assert_eq!(one.verdict, ReviewVerdict::RequestChanges);
        assert_eq!(one.summary, "Reads well. A case is missing.");
        assert_eq!(
            one.comments,
            [
                missing_test,
                comment("src/due.rs", 30, "An empty date panics.")
            ]
        );
        // A review passes no checks, so none is first.
        assert_eq!(counted(reviews.clone(), Pick::FirstPassing), counted_as);
        // The judge's pick leaves them apart, for the judge to settle.
        assert_eq!(counted(reviews.clone(), Pick::Judge), reviews);
    }
}
