//! Cross-judging, aggregated in code. Each drafter scores every plan but its own, anonymized,
//! in both orders when `both_orders`. A score from a judge of the plan author's family is
//! dropped. A plan's mean is over counted judges (each judge's mean of its rubric criteria,
//! averaged over its two orders); spread is the largest gap between one judge's two orders. A
//! clear winner leads by `judging.clear_winner_margin`; anything closer is a near tie for you.
//! With exactly two judges, the choice stands only if both rank the same plan first. Two
//! drafters judging each other also score their own plans for that check, and those scores
//! never enter a plan's mean.
//!
//! The rubric and this scoring rule are fixed: no setting changes them.

use std::collections::BTreeMap;

use argonauts_core::events::plan::{JudgingResult, PlanTally};
use argonauts_core::names::{PlanLabel, WorkerRef};
use argonauts_core::settings::protocol::JudgingSettings;
use argonauts_core::steps::plan::RubricCriterion;

use crate::prompts::plan::RUBRIC;

/// Scores differ by less than this only through float noise.
const EPSILON: f64 = 1e-9;

/// One judge's scores for one plan in one order.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub judge: WorkerRef,
    pub judge_family: String,
    pub plan: PlanLabel,
    /// 0 for the first presentation order, 1 for the reversed one.
    pub order: u8,
    /// Per rubric criterion, in rubric order, 0 to 10, where 10 is always best
    /// (`correctness_risk` 10 means the lowest risk).
    pub criteria: Vec<f64>,
}

/// The plans' authors with their families.
pub type Authors = BTreeMap<PlanLabel, (WorkerRef, String)>;

/// The built-in rubric.
pub fn rubric() -> Vec<RubricCriterion> {
    rubric_in(RUBRIC)
}

/// A rubric parsed from its prompt file, the built-in or your `prompts/plan/rubric.md`: one
/// `- \`name\`: what a 10 means` line each. A file with no such line is no rubric, so the
/// built-in is used.
pub fn rubric_in(text: &str) -> Vec<RubricCriterion> {
    let criteria: Vec<RubricCriterion> = text
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("- `")?;
            let (name, description) = rest.split_once("`:")?;
            Some(RubricCriterion {
                name: name.to_owned(),
                description: description.trim().to_owned(),
            })
        })
        .collect();
    if criteria.is_empty() && text != RUBRIC {
        tracing::warn!("prompts: your plan/rubric.md lists no criterion, so the built-in is used");
        return rubric();
    }
    criteria
}

/// plan -> judge -> value per order
type Table = BTreeMap<PlanLabel, BTreeMap<WorkerRef, BTreeMap<u8, f64>>>;

/// Every counted judge's value for each plan, with its orders.
struct Counted {
    /// What the means and votes are made of.
    values: Table,
    /// `values` plus each judge's scores of its own family's plans. Only the two-judge
    /// agreement check reads it, since a judge's ranking needs every plan it scored.
    rankings: Table,
    dropped: BTreeMap<PlanLabel, u32>,
}

fn count(scores: &[Score], authors: &Authors, criteria: usize) -> Counted {
    let mut counted = Counted {
        values: authors
            .keys()
            .map(|label| (*label, BTreeMap::new()))
            .collect(),
        rankings: Table::new(),
        dropped: authors.keys().map(|label| (*label, 0)).collect(),
    };
    for score in scores {
        let Some((author, family)) = authors.get(&score.plan) else {
            continue;
        };
        let own = &score.judge == author || &score.judge_family == family;
        let malformed = score.criteria.len() != criteria
            || score.criteria.iter().any(|value| !value.is_finite());
        if own || malformed {
            *counted.dropped.entry(score.plan).or_default() += 1;
        }
        if malformed {
            continue;
        }
        let value = mean(score.criteria.iter().map(|v| v.clamp(0.0, 10.0)));
        insert(&mut counted.rankings, score, value);
        if !own {
            insert(&mut counted.values, score, value);
        }
    }
    counted
}

fn insert(table: &mut Table, score: &Score, value: f64) {
    table
        .entry(score.plan)
        .or_default()
        .entry(score.judge.clone())
        .or_default()
        .insert(score.order, value);
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = values.fold((0.0, 0u32), |(sum, n), v| (sum + v, n + 1));
    if n == 0 { 0.0 } else { sum / f64::from(n) }
}

impl Counted {
    fn tallies(&self, authors: &Authors) -> Vec<PlanTally> {
        authors
            .iter()
            .map(|(label, (author, _))| {
                let judges = self.values.get(label);
                let per_judge = judges
                    .into_iter()
                    .flat_map(|judges| judges.values())
                    .map(|orders| mean(orders.values().copied()));
                let spread = judges
                    .into_iter()
                    .flat_map(|judges| judges.values())
                    .filter_map(|orders| match (orders.get(&0), orders.get(&1)) {
                        (Some(a), Some(b)) => Some((a - b).abs()),
                        _ => None,
                    })
                    .fold(0.0, f64::max);
                PlanTally {
                    label: *label,
                    author: author.clone(),
                    mean: mean(per_judge),
                    spread,
                    counted: judges.map_or(0, |judges| judges.len() as u32),
                    dropped: self.dropped.get(label).copied().unwrap_or(0),
                }
            })
            .collect()
    }

    fn judges(&self) -> Vec<&WorkerRef> {
        let mut judges: Vec<&WorkerRef> = self.values.values().flat_map(|j| j.keys()).collect();
        judges.sort();
        judges.dedup();
        judges
    }
}

fn orders<'a>(
    table: &'a Table,
    judge: &WorkerRef,
    plan: PlanLabel,
) -> Option<&'a BTreeMap<u8, f64>> {
    table.get(&plan).and_then(|judges| judges.get(judge))
}

/// Whether `judge` ranks `a` above `b` in every order it scored both in. A preference that
/// flips with the order counts as a tie, which is not a preference.
fn prefers(table: &Table, judge: &WorkerRef, a: PlanLabel, b: PlanLabel) -> bool {
    let (Some(a), Some(b)) = (orders(table, judge, a), orders(table, judge, b)) else {
        return false;
    };
    let shared: Vec<u8> = a
        .keys()
        .filter(|order| b.contains_key(order))
        .copied()
        .collect();
    !shared.is_empty() && shared.iter().all(|order| a[order] > b[order] + EPSILON)
}

/// The plan a judge scored highest, `None` on a tie or a flip between orders.
fn favorite(table: &Table, judge: &WorkerRef) -> Option<PlanLabel> {
    let plans: Vec<PlanLabel> = table
        .iter()
        .filter(|(_, judges)| judges.contains_key(judge))
        .map(|(label, _)| *label)
        .collect();
    plans.iter().copied().find(|best| {
        plans
            .iter()
            .filter(|other| *other != best)
            .all(|other| prefers(table, judge, *best, *other))
    })
}

/// How far `judge` puts `best` ahead of the next plan it scored, each plan's value averaged
/// over its orders.
fn lead(table: &Table, judge: &WorkerRef, best: PlanLabel) -> f64 {
    let value = |plan: PlanLabel| orders(table, judge, plan).map(|o| mean(o.values().copied()));
    let Some(top) = value(best) else {
        return 0.0;
    };
    let next = table
        .keys()
        .filter(|plan| **plan != best)
        .filter_map(|plan| value(*plan))
        .fold(f64::NEG_INFINITY, f64::max);
    if next.is_finite() { top - next } else { 0.0 }
}

/// Plans in ranking order: highest mean first, ties by label.
fn ranked(tallies: &[PlanTally]) -> Vec<&PlanTally> {
    let mut ranked: Vec<&PlanTally> = tallies.iter().collect();
    ranked.sort_by(|a, b| b.mean.total_cmp(&a.mean).then(a.label.cmp(&b.label)));
    ranked
}

pub fn aggregate(scores: &[Score], authors: &Authors, settings: &JudgingSettings) -> JudgingResult {
    aggregate_on(scores, authors, settings, rubric().len())
}

/// [`aggregate`] for a rubric of `criteria` criteria, which a rubric of yours may change.
pub fn aggregate_on(
    scores: &[Score],
    authors: &Authors,
    settings: &JudgingSettings,
    criteria: usize,
) -> JudgingResult {
    let counted = count(scores, authors, criteria);
    let plans = counted.tallies(authors);
    let order = ranked(&plans);
    let (winner, margin) = match order.as_slice() {
        [] => (None, 0.0),
        [only] => ((only.counted > 0).then_some(only.label), only.mean),
        [top, second, ..] => {
            // A plan no judge could score can't be compared, so the choice is yours.
            let all_judged = plans.iter().all(|plan| plan.counted > 0);
            let judges = counted.judges();
            let lead = top.mean - second.mean;
            let (choice, margin) = match judges.as_slice() {
                [a, b] => match agreed(&counted.rankings, a, b) {
                    Some((plan, margin)) => (Some(plan), margin),
                    None => (None, lead),
                },
                _ => (Some(top.label), lead),
            };
            let clear = margin + EPSILON >= settings.clear_winner_margin;
            (choice.filter(|_| all_judged && clear), margin)
        }
    };
    JudgingResult {
        method: settings.method,
        plans,
        winner,
        margin,
        near_tie: winner.is_none(),
    }
}

/// Two judges: the plan both rank first, and the mean of their leads for it. When they judged
/// each other's plans, each lead is measured on one judge's own scale, and the two authors'
/// preference for their own plans cancels in the mean.
fn agreed(rankings: &Table, a: &WorkerRef, b: &WorkerRef) -> Option<(PlanLabel, f64)> {
    let first = favorite(rankings, a)?;
    (favorite(rankings, b)? == first).then(|| {
        let margin = (lead(rankings, a, first) + lead(rankings, b, first)) / 2.0;
        (first, margin)
    })
}

/// `vote`: each judge votes for the plan it scored highest, and a majority of the judges wins.
/// A split, or a judge with no clear favorite, leaves the choice to you.
pub fn vote(scores: &[Score], authors: &Authors, settings: &JudgingSettings) -> JudgingResult {
    vote_on(scores, authors, settings, rubric().len())
}

/// [`vote`] for a rubric of `criteria` criteria.
pub fn vote_on(
    scores: &[Score],
    authors: &Authors,
    settings: &JudgingSettings,
    criteria: usize,
) -> JudgingResult {
    let counted = count(scores, authors, criteria);
    let plans = counted.tallies(authors);
    let judges = counted.judges();
    let mut votes: BTreeMap<PlanLabel, u32> = BTreeMap::new();
    for judge in &judges {
        if let Some(favorite) = favorite(&counted.values, judge) {
            *votes.entry(favorite).or_default() += 1;
        }
    }
    let mut tally: Vec<(PlanLabel, u32)> = votes.into_iter().collect();
    tally.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let top = tally.first().copied();
    let second = tally.get(1).map_or(0, |(_, n)| *n);
    let winner = top
        .filter(|(_, n)| 2 * n > judges.len() as u32)
        .map(|(label, _)| label);
    JudgingResult {
        method: settings.method,
        plans,
        winner,
        margin: f64::from(top.map_or(0, |(_, n)| n) - second),
        near_tie: winner.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::protocol::JudgingMethod;

    use super::*;

    fn worker(name: &str) -> WorkerRef {
        name.parse().unwrap()
    }

    fn authors(list: &[(PlanLabel, &str, &str)]) -> Authors {
        list.iter()
            .map(|(label, author, family)| (*label, (worker(author), (*family).to_owned())))
            .collect()
    }

    fn score(judge: &str, family: &str, plan: PlanLabel, order: u8, value: f64) -> Score {
        Score {
            judge: worker(judge),
            judge_family: family.to_owned(),
            plan,
            order,
            criteria: vec![value; 5],
        }
    }

    /// Both orders with the same value.
    fn both(judge: &str, family: &str, plan: PlanLabel, value: f64) -> [Score; 2] {
        [
            score(judge, family, plan, 0, value),
            score(judge, family, plan, 1, value),
        ]
    }

    fn three_families() -> Authors {
        authors(&[
            (1, "claude-code", "claude"),
            (2, "codex", "gpt"),
            (3, "antigravity-cli", "gemini"),
        ])
    }

    #[test]
    fn the_rubric_is_fixed_and_puts_risk_first() {
        let names: Vec<String> = rubric().into_iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "correctness_risk",
                "completeness",
                "testability",
                "simplicity",
                "cost"
            ]
        );
        assert!(rubric()[0].description.contains("lowest risk"));
        // A rubric of yours is read the same way, and one with no criterion is no rubric.
        let yours = rubric_in("Score on these:\n\n- `speed`: 10 means the fastest to build.\n");
        assert_eq!(yours.len(), 1);
        assert_eq!(yours[0].name, "speed");
        assert_eq!(rubric_in("Just be fair.\n"), rubric());
    }

    #[test]
    fn a_clear_winner_leads_by_the_margin() {
        let scores: Vec<Score> = [
            both("claude-code", "claude", 2, 8.0),
            both("claude-code", "claude", 3, 5.0),
            both("codex", "gpt", 1, 6.0),
            both("codex", "gpt", 3, 5.0),
            both("antigravity-cli", "gemini", 1, 6.0),
            both("antigravity-cli", "gemini", 2, 9.0),
        ]
        .concat();
        let result = aggregate(&scores, &three_families(), &JudgingSettings::default());
        assert_eq!(result.winner, Some(2));
        assert!(!result.near_tie);
        assert!((result.margin - 2.5).abs() < 1e-9);
        let plan2 = &result.plans[1];
        assert_eq!((plan2.counted, plan2.dropped), (2, 0));
        assert!((plan2.mean - 8.5).abs() < 1e-9);
        assert_eq!(result.method, JudgingMethod::Cross);
    }

    #[test]
    fn scores_from_the_author_or_its_family_are_dropped() {
        let authors = authors(&[
            (1, "claude-code/claude-opus", "claude"),
            (2, "codex", "gpt"),
        ]);
        let scores: Vec<Score> = [
            // Its own plan, scored high: dropped.
            both("claude-code/claude-opus", "claude", 1, 10.0),
            // Same family, another model: dropped.
            both("claude-code/claude-sonnet", "claude", 1, 10.0),
            both("claude-code/claude-sonnet", "claude", 2, 7.0),
            both("codex", "gpt", 1, 4.0),
            both("antigravity-cli", "gemini", 1, 5.0),
            both("antigravity-cli", "gemini", 2, 8.0),
        ]
        .concat();
        let result = aggregate(&scores, &authors, &JudgingSettings::default());
        let plan1 = &result.plans[0];
        assert_eq!((plan1.counted, plan1.dropped), (2, 4));
        assert!((plan1.mean - 4.5).abs() < 1e-9);
        assert_eq!(result.winner, Some(2));
    }

    #[test]
    fn spread_is_the_largest_gap_between_orders() {
        let scores = vec![
            score("codex", "gpt", 1, 0, 8.0),
            score("codex", "gpt", 1, 1, 5.0),
            score("antigravity-cli", "gemini", 1, 0, 7.0),
            score("antigravity-cli", "gemini", 1, 1, 6.0),
        ];
        let result = aggregate(&scores, &three_families(), &JudgingSettings::default());
        let plan1 = &result.plans[0];
        assert!((plan1.spread - 3.0).abs() < 1e-9);
        // Each judge's value is the mean of its two orders: 6.5 and 6.5.
        assert!((plan1.mean - 6.5).abs() < 1e-9);
    }

    #[test]
    fn a_lead_under_the_margin_is_a_near_tie() {
        let scores: Vec<Score> = [
            both("claude-code", "claude", 2, 7.5),
            both("claude-code", "claude", 3, 5.0),
            both("codex", "gpt", 1, 7.0),
            both("codex", "gpt", 3, 5.0),
            both("antigravity-cli", "gemini", 1, 7.0),
            both("antigravity-cli", "gemini", 2, 7.5),
        ]
        .concat();
        let result = aggregate(&scores, &three_families(), &JudgingSettings::default());
        assert_eq!(result.winner, None);
        assert!(result.near_tie);
        assert!((result.margin - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_plan_nobody_could_score_goes_to_you() {
        let scores: Vec<Score> = [
            both("claude-code", "claude", 2, 9.0),
            both("codex", "gpt", 1, 3.0),
        ]
        .concat();
        let result = aggregate(&scores, &three_families(), &JudgingSettings::default());
        assert_eq!(result.plans[2].counted, 0);
        assert!(result.near_tie);
    }

    #[test]
    fn two_drafters_who_agree_pick_the_plan() {
        let authors = authors(&[(1, "claude-code", "claude"), (2, "codex", "gpt")]);
        let agree: Vec<Score> = [
            both("claude-code", "claude", 1, 6.0),
            both("claude-code", "claude", 2, 8.0),
            both("codex", "gpt", 1, 5.0),
            both("codex", "gpt", 2, 9.0),
        ]
        .concat();
        let result = aggregate(&agree, &authors, &JudgingSettings::default());
        assert_eq!(result.winner, Some(2), "{result:?}");
        // Own-plan scores rank, but never enter a mean.
        let (plan1, plan2) = (&result.plans[0], &result.plans[1]);
        assert!((plan1.mean - 5.0).abs() < 1e-9);
        assert!((plan2.mean - 8.0).abs() < 1e-9);
        assert_eq!((plan2.counted, plan2.dropped), (1, 2));
        // The mean of their leads: 2 and 4.
        assert!((result.margin - 3.0).abs() < 1e-9);
    }

    #[test]
    fn two_drafters_agree_even_when_their_scales_differ() {
        let authors = authors(&[(1, "claude-code", "claude"), (2, "codex", "gpt")]);
        // codex scores everything high, so plan 1 has the higher mean, yet both rank plan 2
        // first.
        let scores: Vec<Score> = [
            both("claude-code", "claude", 1, 7.0),
            both("claude-code", "claude", 2, 8.5),
            both("codex", "gpt", 1, 9.0),
            both("codex", "gpt", 2, 10.0),
        ]
        .concat();
        let result = aggregate(&scores, &authors, &JudgingSettings::default());
        assert!(result.plans[0].mean > result.plans[1].mean);
        assert_eq!(result.winner, Some(2), "{result:?}");
        assert!((result.margin - 1.25).abs() < 1e-9);
    }

    #[test]
    fn two_drafters_who_each_prefer_their_own_plan_leave_it_to_you() {
        let authors = authors(&[(1, "claude-code", "claude"), (2, "codex", "gpt")]);
        let split: Vec<Score> = [
            both("claude-code", "claude", 1, 9.0),
            both("claude-code", "claude", 2, 6.0),
            both("codex", "gpt", 1, 5.0),
            both("codex", "gpt", 2, 8.0),
        ]
        .concat();
        let result = aggregate(&split, &authors, &JudgingSettings::default());
        assert!(result.near_tie, "{result:?}");

        // A lead under the margin is still a near tie.
        let close: Vec<Score> = [
            both("claude-code", "claude", 1, 7.0),
            both("claude-code", "claude", 2, 7.5),
            both("codex", "gpt", 1, 6.0),
            both("codex", "gpt", 2, 7.0),
        ]
        .concat();
        let result = aggregate(&close, &authors, &JudgingSettings::default());
        assert!(result.near_tie, "{result:?}");

        // Without their own-plan scores neither ranks one plan above the other.
        let cross: Vec<Score> = [
            both("claude-code", "claude", 2, 9.0),
            both("codex", "gpt", 1, 3.0),
        ]
        .concat();
        let result = aggregate(&cross, &authors, &JudgingSettings::default());
        assert!(result.near_tie, "{result:?}");
    }

    #[test]
    fn two_outside_judges_must_agree() {
        let authors = authors(&[(1, "claude-code", "claude"), (2, "codex", "gpt")]);
        // Two outside judges who both rank plan 2 first: the choice stands.
        let agree: Vec<Score> = [
            both("antigravity-cli", "gemini", 1, 5.0),
            both("antigravity-cli", "gemini", 2, 8.0),
            both("opencode/mistral", "mistral", 1, 6.0),
            both("opencode/mistral", "mistral", 2, 7.5),
        ]
        .concat();
        let result = aggregate(&agree, &authors, &JudgingSettings::default());
        assert_eq!(result.winner, Some(2));

        // They disagree: the means still differ by the margin, but the choice is yours.
        let disagree: Vec<Score> = [
            both("antigravity-cli", "gemini", 1, 3.0),
            both("antigravity-cli", "gemini", 2, 9.0),
            both("opencode/mistral", "mistral", 1, 6.0),
            both("opencode/mistral", "mistral", 2, 5.0),
        ]
        .concat();
        let result = aggregate(&disagree, &authors, &JudgingSettings::default());
        assert_eq!(result.winner, None);
    }

    #[test]
    fn a_preference_that_flips_with_the_order_is_no_agreement() {
        let authors = authors(&[(1, "claude-code", "claude"), (2, "codex", "gpt")]);
        let scores = vec![
            score("antigravity-cli", "gemini", 1, 0, 4.0),
            score("antigravity-cli", "gemini", 2, 0, 9.0),
            score("antigravity-cli", "gemini", 1, 1, 4.0),
            score("antigravity-cli", "gemini", 2, 1, 9.0),
            score("opencode/mistral", "mistral", 1, 0, 8.0),
            score("opencode/mistral", "mistral", 2, 0, 9.0),
            score("opencode/mistral", "mistral", 1, 1, 8.0),
            score("opencode/mistral", "mistral", 2, 1, 7.5),
        ];
        let result = aggregate(&scores, &authors, &JudgingSettings::default());
        assert!(result.near_tie);
    }

    #[test]
    fn malformed_scores_are_dropped() {
        let mut short = score("codex", "gpt", 1, 0, 9.0);
        short.criteria.pop();
        let mut nan = score("antigravity-cli", "gemini", 1, 0, 9.0);
        nan.criteria[2] = f64::NAN;
        let result = aggregate(
            &[short, nan],
            &three_families(),
            &JudgingSettings::default(),
        );
        assert_eq!(result.plans[0].dropped, 2);
        assert_eq!(result.plans[0].counted, 0);
    }

    #[test]
    fn a_majority_vote_wins_and_a_split_is_yours() {
        let settings = JudgingSettings {
            method: JudgingMethod::Vote,
            ..JudgingSettings::default()
        };
        let majority: Vec<Score> = [
            both("claude-code", "claude", 2, 8.0),
            both("claude-code", "claude", 3, 5.0),
            both("codex", "gpt", 1, 5.0),
            both("codex", "gpt", 3, 4.0),
            both("antigravity-cli", "gemini", 1, 6.0),
            both("antigravity-cli", "gemini", 2, 7.0),
        ]
        .concat();
        let result = vote(&majority, &three_families(), &settings);
        assert_eq!(result.winner, Some(2));
        assert_eq!(result.method, JudgingMethod::Vote);

        let split: Vec<Score> = [
            both("claude-code", "claude", 2, 8.0),
            both("claude-code", "claude", 3, 5.0),
            both("codex", "gpt", 1, 5.0),
            both("codex", "gpt", 3, 9.0),
            both("antigravity-cli", "gemini", 1, 7.0),
            both("antigravity-cli", "gemini", 2, 6.0),
        ]
        .concat();
        let result = vote(&split, &three_families(), &settings);
        assert!(result.near_tie);
    }
}
