//! Owners by `routing.algorithm`, among the role's candidates. The engine keeps the rules that
//! remove candidates: agents that are down, degraded when another fits, or out of plan, which
//! is when their provider says so or their plan window is as full as your `limits.plan_usage`.
//! With learning's track records, `bandit` ranks what's left by Thompson
//! draws on the routing utility and `epsilon-greedy` by the posterior means, both exploring
//! only where `routing::may_explore` allows, and every draw is kept for the outcome log.
//! Without them, `bandit` samples Beta(passes + 1, failures + 1) from plain counts. Among
//! workers within `routing.tie_band` of the best, the lower registry cost wins, then fewer
//! running cards. Seeded, so tests are deterministic.

use std::cmp::Ordering;

use rand::RngExt;
use rand::rngs::StdRng;

use argonauts_core::CardId;
use argonauts_core::limits::{CostEstimate, HealthState, TrackRecord};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::protocol::{RoutingAlgorithm, RoutingSettings};
use argonauts_learning::Assignment;
use argonauts_learning::records::Record;
use argonauts_learning::routing::{self, Considered, Draw, RunSoFar};

/// Draws used to estimate how likely the chosen owner was, logged with the choice.
const PROBABILITY_DRAWS: u32 = 200;

/// A worker the router may pick, with what it knows about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub worker: WorkerRef,
    pub record: Option<TrackRecord>,
    pub health: HealthState,
    /// Share of quota used, 0 to 1, when known.
    pub quota_used: Option<f64>,
    /// Your `limits.plan_usage`, when you set one.
    pub plan_limit: Option<f64>,
    pub cost: CostEstimate,
    pub running_cards: u32,
    /// Its learning record for the card's task type, from the book divide rebuilt.
    pub learned: Option<Record>,
}

/// The card being routed, as the exploration guardrails and the draw log see it.
#[derive(Debug, Clone, Copy)]
pub struct CardToRoute<'a> {
    pub card: &'a CardId,
    pub task_type: &'a str,
    pub labels: &'a [String],
    pub touches_protected: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub worker: WorkerRef,
    pub explored: bool,
    /// One line for `Divide::Owner`.
    pub reason: String,
}

/// Thompson sampling over track records. `None` when no candidate is up and under its quota.
pub fn choose(
    candidates: &[Candidate],
    labels: &[String],
    settings: &RoutingSettings,
    rng: &mut StdRng,
) -> Option<Choice> {
    let eligible = eligible(candidates);
    if eligible.is_empty() {
        return None;
    }
    let may_explore = !labels.iter().any(|label| {
        settings
            .no_explore_labels
            .iter()
            .any(|no| no.eq_ignore_ascii_case(label))
    });
    if may_explore && eligible.len() > 1 && rng.random::<f64>() < settings.explore_rate {
        return Some(explore(&eligible, rng));
    }
    let sampled: Vec<(&Candidate, f64)> = eligible
        .iter()
        .map(|candidate| {
            let (passed, failed) = passes_and_failures(candidate.record);
            let rate = beta_sample(f64::from(passed) + 1.0, f64::from(failed) + 1.0, rng);
            (*candidate, rate)
        })
        .collect();
    let best = sampled
        .iter()
        .map(|(_, rate)| *rate)
        .fold(f64::NEG_INFINITY, f64::max);
    let top = sampled
        .iter()
        .find(|(_, rate)| *rate == best)
        .map(|(candidate, _)| &candidate.worker);
    let (chosen, _) = sampled
        .iter()
        .filter(|(_, rate)| *rate >= best - settings.tie_band)
        .min_by(|(a, a_rate), (b, b_rate)| {
            cost_order(a.cost, b.cost)
                .then(a.running_cards.cmp(&b.running_cards))
                .then(b_rate.total_cmp(a_rate))
        })?;
    let mut reason = record_line(chosen);
    if Some(&chosen.worker) != top {
        reason.push_str(", tied with the best and cheaper or less busy");
    }
    Some(Choice {
        worker: chosen.worker.clone(),
        explored: false,
        reason,
    })
}

/// The candidates best first, the same every time: those up and under their quota first, then
/// by the track record's expected pass rate, then the cheaper and the less busy, then as listed.
/// Code goes by it where a pick must not change between tries, such as moving an assignment
/// that broke the rules.
pub fn rank(candidates: &[Candidate]) -> Vec<WorkerRef> {
    let up = eligible(candidates);
    let expected = |candidate: &Candidate| {
        let (passed, failed) = passes_and_failures(candidate.record);
        (f64::from(passed) + 1.0) / (f64::from(passed + failed) + 2.0)
    };
    let mut ranked: Vec<(usize, &Candidate)> = candidates.iter().enumerate().collect();
    ranked.sort_by(|(a_at, a), (b_at, b)| {
        let a_up = up.iter().any(|candidate| std::ptr::eq(*candidate, *a));
        let b_up = up.iter().any(|candidate| std::ptr::eq(*candidate, *b));
        b_up.cmp(&a_up)
            .then(expected(b).total_cmp(&expected(a)))
            .then(cost_order(a.cost, b.cost))
            .then(a.running_cards.cmp(&b.running_cards))
            .then(a_at.cmp(b_at))
    });
    ranked
        .into_iter()
        .map(|(_, candidate)| candidate.worker.clone())
        .collect()
}

/// The algorithm that routes a card under `algorithm`. The coordinator's own picks are taken
/// from its assignments before routing, so any card it left without an owner goes to `bandit`.
pub fn effective(algorithm: RoutingAlgorithm) -> RoutingAlgorithm {
    match algorithm {
        RoutingAlgorithm::Orchestrator => RoutingAlgorithm::Bandit,
        other => other,
    }
}

/// Picks by `algorithm`, as [`effective`] resolves it. `turn` counts the cards routed so far,
/// for round-robin.
pub fn route(
    algorithm: RoutingAlgorithm,
    candidates: &[Candidate],
    labels: &[String],
    settings: &RoutingSettings,
    rng: &mut StdRng,
    turn: usize,
) -> Option<Choice> {
    match effective(algorithm) {
        RoutingAlgorithm::Bandit
        | RoutingAlgorithm::EpsilonGreedy
        | RoutingAlgorithm::Orchestrator => choose(candidates, labels, settings, rng),
        RoutingAlgorithm::RoundRobin => {
            let eligible = eligible(candidates);
            let chosen = eligible.get(turn % eligible.len().max(1))?;
            Some(Choice {
                worker: chosen.worker.clone(),
                explored: false,
                reason: format!("round robin, turn {}", turn + 1),
            })
        }
        RoutingAlgorithm::Fixed => {
            let chosen = eligible(candidates).into_iter().next()?;
            let reason = if chosen.worker == candidates[0].worker {
                "the first listed worker".to_owned()
            } else {
                "the first listed worker that is up and under its plan usage".to_owned()
            };
            Some(Choice {
                worker: chosen.worker.clone(),
                explored: false,
                reason,
            })
        }
    }
}

/// [`route`] for one card, with learning's records when every candidate has one: `bandit` and
/// `epsilon-greedy` rank by the routing utility, explore only where the guardrails allow and
/// count it in `so_far`, and return the draw for the outcome log.
pub fn route_card(
    algorithm: RoutingAlgorithm,
    candidates: &[Candidate],
    to_route: &CardToRoute,
    settings: &RoutingSettings,
    rng: &mut StdRng,
    turn: usize,
    so_far: &mut RunSoFar,
) -> Option<(Choice, Option<Draw>)> {
    let algorithm = effective(algorithm);
    let learning = matches!(
        algorithm,
        RoutingAlgorithm::Bandit | RoutingAlgorithm::EpsilonGreedy
    ) && !candidates.is_empty()
        && candidates
            .iter()
            .all(|candidate| candidate.learned.is_some());
    if !learning {
        let choice = route(algorithm, candidates, to_route.labels, settings, rng, turn)?;
        so_far.routed += 1;
        so_far.explored += u32::from(choice.explored);
        return Some((choice, None));
    }
    let (choice, draw) = learned(algorithm, candidates, to_route, settings, rng, *so_far)?;
    so_far.routed += 1;
    so_far.explored += u32::from(choice.explored);
    Some((choice, Some(draw)))
}

fn learned(
    algorithm: RoutingAlgorithm,
    candidates: &[Candidate],
    to_route: &CardToRoute,
    settings: &RoutingSettings,
    rng: &mut StdRng,
    so_far: RunSoFar,
) -> Option<(Choice, Draw)> {
    let eligible = eligible(candidates);
    let records: Vec<Record> = eligible
        .iter()
        .filter_map(|candidate| candidate.learned.clone())
        .collect();
    if records.is_empty() {
        return None;
    }
    let ranked = routing::rank(&records, &settings.weights, rng);
    let best_mean = ranked
        .iter()
        .max_by(|a, b| a.mean.total_cmp(&b.mean))?
        .worker
        .clone();
    let may_explore = eligible.len() > 1
        && routing::may_explore(
            to_route.labels,
            to_route.touches_protected,
            None,
            so_far,
            settings,
        );
    let by_worker = |worker: &WorkerRef| {
        eligible
            .iter()
            .find(|candidate| &candidate.worker == worker)
            .copied()
    };
    let explored = match algorithm {
        RoutingAlgorithm::EpsilonGreedy => {
            if may_explore && rng.random::<f64>() < settings.explore_rate {
                let others: Vec<&WorkerRef> = ranked
                    .iter()
                    .map(|ranked| &ranked.worker)
                    .filter(|worker| **worker != best_mean)
                    .collect();
                others
                    .get(rng.random_range(0..others.len().max(1)))
                    .map(|w| (*w).clone())
            } else {
                None
            }
        }
        _ => {
            let top = &ranked[0].worker;
            (may_explore && *top != best_mean).then(|| top.clone())
        }
    };
    let chosen = match &explored {
        Some(worker) => by_worker(worker)?,
        None => {
            // The best by mean, unless another sits within the tie band and costs less or is
            // less busy.
            let best = ranked
                .iter()
                .find(|ranked| ranked.worker == best_mean)
                .map_or(0.0, |ranked| ranked.mean);
            ranked
                .iter()
                .filter(|ranked| ranked.mean >= best - settings.tie_band)
                .filter_map(|ranked| by_worker(&ranked.worker).map(|c| (c, ranked.mean)))
                .min_by(|(a, a_mean), (b, b_mean)| {
                    cost_order(a.cost, b.cost)
                        .then(a.running_cards.cmp(&b.running_cards))
                        .then(b_mean.total_cmp(a_mean))
                })
                .map(|(candidate, _)| candidate)?
        }
    };
    let probability = routing::choice_probability(
        &records,
        &chosen.worker,
        &settings.weights,
        rng,
        PROBABILITY_DRAWS,
    );
    let reason = if explored.is_some() {
        Assignment::Explore
    } else {
        Assignment::Exploit
    };
    let considered = candidates
        .iter()
        .map(|candidate| {
            let ranked = ranked.iter().find(|r| r.worker == candidate.worker);
            Considered {
                worker: candidate.worker.clone(),
                removed_by: removed_by(candidate, &eligible),
                sampled: ranked.map(|r| r.sampled),
                mean: ranked.map(|r| r.mean),
            }
        })
        .collect();
    let line = match (&explored, chosen.learned.as_ref()) {
        (Some(_), Some(record)) => {
            format!("exploring: {:.0} similar tasks so far", record.evidence)
        }
        _ => record_line(chosen),
    };
    let draw = Draw {
        card: to_route.card.clone(),
        task_type: to_route.task_type.to_owned(),
        considered,
        chosen: chosen.worker.clone(),
        reason,
        probability,
    };
    Some((
        Choice {
            worker: chosen.worker.clone(),
            explored: explored.is_some(),
            reason: line,
        },
        draw,
    ))
}

/// The rule that removed a candidate, if one did, as the draw log names it.
fn removed_by(candidate: &Candidate, eligible: &[&Candidate]) -> Option<String> {
    if eligible.iter().any(|kept| kept.worker == candidate.worker) {
        return None;
    }
    let rule = if candidate.health == HealthState::Down {
        "agent down"
    } else if candidate.plan_ran_out() {
        "plan ran out"
    } else if candidate.out_of_plan() {
        "limits.plan_usage"
    } else {
        "degraded while another is healthy"
    };
    Some(rule.to_owned())
}

impl Candidate {
    /// Its provider's plan window is spent.
    fn plan_ran_out(&self) -> bool {
        self.quota_used.is_some_and(|used| used >= 1.0)
    }

    /// Nothing new goes to it: its plan ran out, or it is as full as you let it get.
    fn out_of_plan(&self) -> bool {
        let limit = self.plan_limit.unwrap_or(1.0);
        self.quota_used.is_some_and(|used| used >= limit)
    }
}

/// A draw from Beta(alpha, beta), as the first of two Gamma draws over their sum.
pub fn beta_sample(alpha: f64, beta: f64, rng: &mut StdRng) -> f64 {
    let x = gamma_sample(alpha, rng);
    let y = gamma_sample(beta, rng);
    if x + y > 0.0 { x / (x + y) } else { 0.5 }
}

/// Candidates that may take a new card, in their given order.
fn eligible(candidates: &[Candidate]) -> Vec<&Candidate> {
    let usable: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.health != HealthState::Down)
        .filter(|candidate| !candidate.out_of_plan())
        .collect();
    if usable
        .iter()
        .any(|candidate| candidate.health == HealthState::Healthy)
    {
        usable
            .into_iter()
            .filter(|candidate| candidate.health == HealthState::Healthy)
            .collect()
    } else {
        usable
    }
}

/// The least-proven candidate, a random one among equals.
fn explore(eligible: &[&Candidate], rng: &mut StdRng) -> Choice {
    let trials = |candidate: &Candidate| candidate.record.map_or(0, |record| record.of);
    let fewest = eligible.iter().map(|c| trials(c)).min().unwrap_or(0);
    let least: Vec<&&Candidate> = eligible.iter().filter(|c| trials(c) == fewest).collect();
    let chosen = least[rng.random_range(0..least.len())];
    let reason = match fewest {
        0 => "exploring: no similar tasks yet".to_owned(),
        1 => "exploring: 1 similar task so far".to_owned(),
        n => format!("exploring: {n} similar tasks so far"),
    };
    Choice {
        worker: chosen.worker.clone(),
        explored: true,
        reason,
    }
}

fn passes_and_failures(record: Option<TrackRecord>) -> (u32, u32) {
    record.map_or((0, 0), |record| {
        let passed = record.passed.min(record.of);
        (passed, record.of - passed)
    })
}

/// Free, then a share of a subscription window, then dollars, then unknown. Units don't
/// compare with each other, only within themselves.
fn cost_order(a: CostEstimate, b: CostEstimate) -> Ordering {
    fn key(cost: CostEstimate) -> (u8, f64) {
        match cost {
            CostEstimate::Free => (0, 0.0),
            CostEstimate::Quota(share) => (1, share),
            CostEstimate::Usd(usd) => (2, usd),
            CostEstimate::Unknown => (3, 0.0),
        }
    }
    let (a, b) = (key(a), key(b));
    a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
}

fn record_line(candidate: &Candidate) -> String {
    let mut line = match candidate.record {
        Some(record) if record.of > 0 => {
            let noun = if record.of == 1 { "task" } else { "tasks" };
            format!("{} of {} similar {noun} passed", record.passed, record.of)
        }
        _ => "no track record yet".to_owned(),
    };
    if let Some(used) = candidate.quota_used {
        line.push_str(&format!(", {:.0}% of plan used", used * 100.0));
    }
    line
}

/// Marsaglia and Tsang's method; shapes below 1 are boosted by one and scaled back.
fn gamma_sample(shape: f64, rng: &mut StdRng) -> f64 {
    if shape <= 0.0 {
        return 0.0;
    }
    if shape < 1.0 {
        let u: f64 = 1.0 - rng.random::<f64>();
        return gamma_sample(shape + 1.0, rng) * u.powf(1.0 / shape);
    }
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();
    loop {
        let x = normal_sample(rng);
        let v = (1.0 + c * x).powi(3);
        if v <= 0.0 {
            continue;
        }
        let u: f64 = 1.0 - rng.random::<f64>();
        if u < 1.0 - 0.0331 * x.powi(4) || u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
            return d * v;
        }
    }
}

/// Box and Muller's transform.
fn normal_sample(rng: &mut StdRng) -> f64 {
    let u1: f64 = 1.0 - rng.random::<f64>();
    let u2: f64 = rng.random();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;

    fn candidate(worker: &str, passed: u32, of: u32) -> Candidate {
        Candidate {
            worker: worker.parse().unwrap(),
            record: Some(TrackRecord { passed, of }),
            health: HealthState::Healthy,
            quota_used: None,
            plan_limit: None,
            cost: CostEstimate::Unknown,
            running_cards: 0,
            learned: None,
        }
    }

    fn no_explore() -> RoutingSettings {
        RoutingSettings {
            explore_rate: 0.0,
            ..RoutingSettings::default()
        }
    }

    fn picks(candidates: &[Candidate], settings: &RoutingSettings, seed: u64) -> Vec<String> {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..50)
            .map(|_| {
                choose(candidates, &[], settings, &mut rng)
                    .unwrap()
                    .worker
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn router_thompson_sampling_seeded() {
        let crew = [candidate("codex", 9, 10), candidate("claude-code", 2, 10)];
        let settings = RoutingSettings::default();
        assert_eq!(picks(&crew, &settings, 7), picks(&crew, &settings, 7));
        assert_ne!(picks(&crew, &settings, 7), picks(&crew, &settings, 8));

        let strong = picks(&crew, &no_explore(), 3)
            .iter()
            .filter(|w| *w == "codex")
            .count();
        assert!(
            strong >= 45,
            "the better record wins most samples: {strong} of 50"
        );

        // Exploration goes to the less-proven agent, and never on a no-explore label.
        let crew = [candidate("codex", 90, 100), candidate("agy", 0, 0)];
        let always = RoutingSettings {
            explore_rate: 1.0,
            ..RoutingSettings::default()
        };
        let mut rng = StdRng::seed_from_u64(1);
        let choice = choose(&crew, &[], &always, &mut rng).unwrap();
        assert!(choice.explored);
        assert_eq!(choice.worker.to_string(), "agy");
        assert_eq!(choice.reason, "exploring: no similar tasks yet");
        let choice = choose(&crew, &["Security".to_owned()], &always, &mut rng).unwrap();
        assert!(!choice.explored);
        assert_eq!(choice.reason, "90 of 100 similar tasks passed");

        let mut rng = StdRng::seed_from_u64(11);
        let draws: Vec<f64> = (0..4000).map(|_| beta_sample(3.0, 7.0, &mut rng)).collect();
        let mean = draws.iter().sum::<f64>() / draws.len() as f64;
        assert!((mean - 0.3).abs() < 0.02, "Beta(3, 7) mean {mean}");
        assert!(draws.iter().all(|x| (0.0..=1.0).contains(x)));
        let mut rng = StdRng::seed_from_u64(12);
        let small: f64 = (0..4000)
            .map(|_| beta_sample(0.5, 0.5, &mut rng))
            .sum::<f64>()
            / 4000.0;
        assert!((small - 0.5).abs() < 0.03, "Beta(0.5, 0.5) mean {small}");
    }

    #[test]
    fn a_plan_is_used_until_it_runs_out_unless_you_set_a_limit() {
        let mut busy = candidate("codex", 10, 10);
        busy.quota_used = Some(0.95);
        let mut fine = candidate("claude-code", 1, 10);
        fine.quota_used = Some(0.4);
        // No limit: 95% used is still usable, and the better record wins.
        let crew = [busy.clone(), fine.clone()];
        for pick in picks(&crew, &no_explore(), 5) {
            assert_eq!(pick, "codex");
        }
        let mut out = busy.clone();
        out.quota_used = Some(1.0);
        for pick in picks(&[out, fine.clone()], &no_explore(), 5) {
            assert_eq!(pick, "claude-code");
        }
        // Your limit of 80% takes the fuller one out before its provider does.
        let limited = [busy.clone(), fine.clone()].map(|mut candidate| {
            candidate.plan_limit = Some(0.8);
            candidate
        });
        for pick in picks(&limited, &no_explore(), 5) {
            assert_eq!(pick, "claude-code");
        }
        assert_eq!(
            removed_by(&limited[0], &[&limited[1]]).as_deref(),
            Some("limits.plan_usage")
        );
    }

    #[test]
    fn router_skips_an_agent_that_is_down_or_out_of_plan() {
        let mut busy = candidate("codex", 10, 10);
        busy.quota_used = Some(1.0);
        let mut down = candidate("agy", 10, 10);
        down.health = HealthState::Down;
        let mut fine = candidate("claude-code", 1, 10);
        fine.quota_used = Some(0.4);
        let crew = [busy.clone(), down.clone(), fine];
        for pick in picks(&crew, &RoutingSettings::default(), 5) {
            assert_eq!(pick, "claude-code");
        }
        let mut rng = StdRng::seed_from_u64(5);
        let choice = choose(&crew, &[], &no_explore(), &mut rng).unwrap();
        assert_eq!(
            choice.reason,
            "1 of 10 similar tasks passed, 40% of plan used"
        );

        // Degraded only when nothing healthy fits.
        let mut degraded = candidate("codex", 10, 10);
        degraded.health = HealthState::Degraded;
        let weak = candidate("claude-code", 0, 10);
        assert!(
            picks(&[degraded.clone(), weak], &no_explore(), 2)
                .iter()
                .all(|w| w == "claude-code")
        );
        assert_eq!(
            picks(&[degraded, down.clone()], &no_explore(), 2)[0],
            "codex"
        );
        let mut rng = StdRng::seed_from_u64(5);
        assert_eq!(choose(&[busy, down], &[], &no_explore(), &mut rng), None);
    }

    #[test]
    fn router_weighs_cost_and_load() {
        // Proven equally well, so every sample falls inside the tie band.
        let mut pricey = candidate("claude-code", 500, 500);
        pricey.cost = CostEstimate::Usd(4.0);
        let mut cheap = candidate("codex", 500, 500);
        cheap.cost = CostEstimate::Usd(1.5);
        for pick in picks(&[pricey.clone(), cheap.clone()], &no_explore(), 9) {
            assert_eq!(pick, "codex");
        }
        let mut free = candidate("ollama", 500, 500);
        free.cost = CostEstimate::Free;
        assert_eq!(picks(&[cheap.clone(), free], &no_explore(), 9)[0], "ollama");

        // Same cost: the less busy one.
        let mut busy = cheap.clone();
        busy.running_cards = 3;
        let mut idle = candidate("agy", 500, 500);
        idle.cost = CostEstimate::Usd(1.5);
        idle.running_cards = 1;
        for pick in picks(&[busy, idle], &no_explore(), 4) {
            assert_eq!(pick, "agy");
        }

        // Outside the band, the better record wins even when it costs more.
        let mut better = candidate("claude-code", 95, 100);
        better.cost = CostEstimate::Usd(4.0);
        let mut worse = candidate("codex", 5, 100);
        worse.cost = CostEstimate::Usd(0.5);
        for pick in picks(&[better, worse], &no_explore(), 4) {
            assert_eq!(pick, "claude-code");
        }
    }

    #[test]
    fn round_robin_and_fixed() {
        let crew = [
            candidate("codex", 0, 0),
            candidate("claude-code", 0, 0),
            candidate("agy", 0, 0),
        ];
        let settings = RoutingSettings::default();
        let mut rng = StdRng::seed_from_u64(0);
        let turns: Vec<String> = (0..4)
            .map(|turn| {
                route(
                    RoutingAlgorithm::RoundRobin,
                    &crew,
                    &[],
                    &settings,
                    &mut rng,
                    turn,
                )
                .unwrap()
                .worker
                .to_string()
            })
            .collect();
        assert_eq!(turns, ["codex", "claude-code", "agy", "codex"]);
        let mut down = crew.clone();
        down[0].health = HealthState::Down;
        let fixed = route(RoutingAlgorithm::Fixed, &down, &[], &settings, &mut rng, 3).unwrap();
        assert_eq!(fixed.worker.to_string(), "claude-code");
        assert_eq!(
            fixed.reason,
            "the first listed worker that is up and under its plan usage"
        );
    }
}
