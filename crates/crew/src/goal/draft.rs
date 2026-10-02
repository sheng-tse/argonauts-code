//! Building the goal file's content: from your prompt, from the restatements and what the
//! materiality rules settled, and with your answers written in.

use std::collections::BTreeSet;

use argonauts_core::RunId;
use argonauts_core::events::goal::{Assumption, Criterion, GoalFile, GoalSource};
use argonauts_core::names::WorkerRef;
use argonauts_core::steps::goal::Restatement;

use super::file::hashed;
use super::materiality::one_line;

/// Your prompt as the goal, for solo runs and for too few restatements.
pub fn from_prompt(run: &RunId, request: &str) -> GoalFile {
    hashed(GoalFile {
        run: run.clone(),
        version: 1,
        summary: request.trim().to_owned(),
        criteria: Vec::new(),
        in_scope: Vec::new(),
        out_of_scope: Vec::new(),
        assumptions: Vec::new(),
        answers: Vec::new(),
        source: GoalSource::Prompt,
        path: Default::default(),
        sha256: String::new(),
    })
}

/// The goal from the restatement closest to all the others, with the settled assumptions.
/// `None` without restatements.
pub fn from_restatements(
    run: &RunId,
    restated: &[(String, WorkerRef, Restatement)],
    assumptions: &[Assumption],
) -> Option<GoalFile> {
    let base = &restated.get(central(restated)?)?.2;
    let criteria = distinct(&base.success_criteria)
        .into_iter()
        .enumerate()
        .map(|(index, text)| Criterion {
            id: format!("G{}", index + 1),
            text,
        })
        .collect();
    Some(hashed(GoalFile {
        run: run.clone(),
        version: 1,
        summary: one_line(&base.summary),
        criteria,
        in_scope: distinct(&base.in_scope),
        out_of_scope: distinct(&base.out_of_scope),
        assumptions: assumptions.to_vec(),
        answers: Vec::new(),
        source: GoalSource::Restatements,
        path: Default::default(),
        sha256: String::new(),
    }))
}

/// Writes answers into the goal, skipping ones it already holds, so writing them again
/// changes nothing.
pub fn with_answers(goal: &GoalFile, answers: &[String]) -> GoalFile {
    let mut answered = goal.clone();
    for answer in answers {
        let answer = one_line(answer);
        if !answer.is_empty() && !answered.answers.contains(&answer) {
            answered.answers.push(answer);
        }
    }
    hashed(answered)
}

/// The index of the restatement that shares the most words with all the others, so the goal
/// starts from the consensus rather than from whichever agent happened to be first. Ties go to
/// the earlier label.
pub fn central(restated: &[(String, WorkerRef, Restatement)]) -> Option<usize> {
    let words: Vec<BTreeSet<String>> = restated
        .iter()
        .map(|(_, _, restatement)| word_set(restatement))
        .collect();
    let score = |index: usize| -> f64 {
        words
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, other)| jaccard(&words[index], other))
            .sum()
    };
    (0..restated.len()).reduce(|best, index| {
        if score(index) > score(best) {
            index
        } else {
            best
        }
    })
}

fn word_set(restatement: &Restatement) -> BTreeSet<String> {
    std::iter::once(&restatement.summary)
        .chain(&restatement.success_criteria)
        .chain(&restatement.in_scope)
        .chain(&restatement.out_of_scope)
        .flat_map(|text| text.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| word.len() > 2)
        .map(str::to_lowercase)
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

fn distinct(items: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    items
        .iter()
        .map(|item| one_line(item))
        .filter(|item| !item.is_empty() && seen.insert(item.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restatement(summary: &str, criteria: &[&str]) -> Restatement {
        Restatement {
            summary: summary.to_owned(),
            success_criteria: criteria.iter().map(|c| (*c).to_owned()).collect(),
            in_scope: vec!["the API".to_owned(), "The API".to_owned()],
            out_of_scope: Vec::new(),
            assumptions: Vec::new(),
            open_questions: Vec::new(),
        }
    }

    fn labeled(items: Vec<Restatement>) -> Vec<(String, WorkerRef, Restatement)> {
        items
            .into_iter()
            .enumerate()
            .map(|(index, r)| {
                (
                    super::super::restate::label(index),
                    WorkerRef::new(format!("w{index}"), None),
                    r,
                )
            })
            .collect()
    }

    #[test]
    fn the_prompt_goal_keeps_your_words() {
        let run = RunId::new("run-01").unwrap();
        let goal = from_prompt(&run, "  Fix the flaky login test.\nKeep the API.  ");
        assert_eq!(goal.source, GoalSource::Prompt);
        assert_eq!(goal.summary, "Fix the flaky login test.\nKeep the API.");
        assert!(goal.criteria.is_empty());
        assert_eq!(goal.sha256.len(), 64);
    }

    #[test]
    fn the_goal_starts_from_the_consensus_restatement() {
        let run = RunId::new("run-01").unwrap();
        let restated = labeled(vec![
            restatement("Rewrite the limiter in Go", &["limiter runs in Go"]),
            restatement(
                "Add a per user rate limiter to the API",
                &["requests over the limit get 429", "limit is configurable"],
            ),
            restatement(
                "Add a per user rate limit to the API",
                &["requests over the limit get 429"],
            ),
        ]);
        assert_eq!(central(&restated), Some(1));
        let assumptions = vec![Assumption {
            text: "t: r".to_owned(),
            basis: "b".to_owned(),
        }];
        let goal = from_restatements(&run, &restated, &assumptions).unwrap();
        assert_eq!(goal.summary, "Add a per user rate limiter to the API");
        let ids: Vec<&str> = goal.criteria.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["G1", "G2"]);
        assert_eq!(goal.in_scope, ["the API"]);
        assert_eq!(goal.assumptions, assumptions);
        assert_eq!(goal.source, GoalSource::Restatements);
        assert!(from_restatements(&run, &[], &[]).is_none());
    }

    #[test]
    fn answers_are_written_once() {
        let run = RunId::new("run-01").unwrap();
        let goal = from_prompt(&run, "x");
        let answers = vec!["Q1. Per key?\nYes".to_owned()];
        let once = with_answers(&goal, &answers);
        assert_eq!(once.answers, ["Q1. Per key? Yes"]);
        assert_eq!(with_answers(&once, &answers), once);
    }
}
