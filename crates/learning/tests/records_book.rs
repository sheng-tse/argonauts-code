//! Track records: one book from one log and date, pooled while sparse, fading with age.

mod records_support;

use std::collections::BTreeMap;
use std::sync::Arc;

use argonauts_core::RunId;
use argonauts_core::limits::{TrackRecord, TrackRecords};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{PriorRates, RoutingSettings};
use argonauts_learning::Store;
use argonauts_learning::outcomes::{self, LogLine, Outcome};
use argonauts_learning::records::{Beta, Book, LiveRecords, POOLED_ALL_TYPES, POOLED_CREW};
use argonauts_learning::types::Assignment;
use time::{Duration, OffsetDateTime};

use records_support::{AS_OF, cards, close, days_ago, lines, outcome, worker};

/// Every cell scored on its own evidence, so a test sees one cell's arithmetic.
fn own_cells() -> RoutingSettings {
    RoutingSettings {
        own_type_after: 0,
        ..RoutingSettings::default()
    }
}

fn configured_prior(owner: &str, task_type: &str, rate: f64) -> RoutingSettings {
    let mut settings = own_cells();
    settings.priors = BTreeMap::from([(
        owner.to_owned(),
        BTreeMap::from([(
            task_type.to_owned(),
            PriorRates {
                first_pass_rate: Some(rate),
                approval_rate: None,
            },
        )]),
    )]);
    settings
}

fn with_model(mut outcome: Outcome, model: Option<&str>) -> Outcome {
    outcome.model_id = model.map(str::to_owned);
    outcome
}

#[test]
fn track_records_same_log_same_book() {
    let mut log = cards(1, "codex", "backend", 6, 2, 3.0);
    log.extend(cards(20, "claude-code/opus", "frontend", 3, 1, 10.0));
    log.extend(cards(40, "claude-code/opus", "backend", 1, 2, 40.0));
    log[0].reviewer = Some(worker("claude-code/opus"));
    log[0].approved_first_review = Some(true);
    log[1].minutes = Some(12.0);
    let lines = lines(log);
    let settings = RoutingSettings::default();

    let book = Book::build(&lines, &settings, AS_OF);
    assert_eq!(book, Book::build(&lines, &settings, AS_OF));
    let order: Vec<(String, String)> = book
        .rows()
        .iter()
        .map(|record| (record.task_type.clone(), record.worker.to_string()))
        .collect();
    assert_eq!(
        order,
        [
            ("backend", "claude-code/opus"),
            ("backend", "codex"),
            ("frontend", "claude-code/opus"),
        ]
        .map(|(kind, name)| (kind.to_owned(), name.to_owned()))
    );
    let codex = book.record(&worker("codex"), "backend");
    assert_eq!(codex.counts, TrackRecord { passed: 6, of: 8 });

    // What happened after the date doesn't count.
    let mut later = lines.clone();
    later.push(LogLine::Outcome(outcome(
        90, "codex", "backend", false, -2.0,
    )));
    assert_eq!(Book::build(&later, &settings, AS_OF), book);
    let after = days_ago(-3.0);
    assert_ne!(
        Book::build(&later, &settings, after),
        Book::build(&lines, &settings, after)
    );

    // Rebuilding from the files on disk gives the same book.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("state"), dir.path().join("repo"));
    outcomes::append(&store, &RunId::new("run-01").unwrap(), &lines).unwrap();
    assert_eq!(Book::rebuild(&store, &settings, AS_OF).unwrap(), book);

    // The live service reads as of the clock, so its lines are dated by it. It builds once,
    // and again only after a refresh.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("state"), dir.path().join("repo"));
    let yesterday = |mut outcome: Outcome| {
        outcome.at = Some(OffsetDateTime::now_utc() - Duration::days(1));
        LogLine::Outcome(outcome)
    };
    let first: Vec<LogLine> = cards(1, "codex", "backend", 6, 2, 0.0)
        .into_iter()
        .map(yesterday)
        .collect();
    outcomes::append(&store, &RunId::new("run-01").unwrap(), &first).unwrap();
    let live = LiveRecords::new(
        store.state_dir.clone(),
        store.repo_root.clone(),
        Arc::new(Settings::default()),
    );
    assert_eq!(
        live.get(&worker("codex"), "backend"),
        Some(TrackRecord { passed: 6, of: 8 })
    );
    assert_eq!(live.get(&worker("agy"), "backend"), None);
    let next = [yesterday(outcome(91, "codex", "backend", true, 0.0))];
    outcomes::append(&store, &RunId::new("run-02").unwrap(), &next).unwrap();
    assert_eq!(live.get(&worker("codex"), "backend").map(|r| r.of), Some(8));
    live.refresh();
    assert_eq!(
        live.get(&worker("codex"), "backend"),
        Some(TrackRecord { passed: 7, of: 9 })
    );
    assert_eq!(live.book().rows().len(), 1);
}

#[test]
fn records_pool_sparse_cells_into_groups() {
    let settings = RoutingSettings::default();
    assert_eq!(settings.own_type_after, 10);
    let mut log = cards(1, "codex", "backend", 8, 0, 0.0);
    log.extend(cards(20, "claude-code", "frontend", 0, 4, 0.0));
    log.extend(cards(30, "claude-code", "migration", 1, 1, 0.0));
    let book = Book::build(&lines(log.clone()), &settings, AS_OF);

    // Eight backend cards are too few alone, so backend and data share them.
    let backend = book.record(&worker("codex"), "backend");
    let data = book.record(&worker("codex"), "data");
    assert_eq!(backend.pooled_from.as_deref(), Some("backend_and_data"));
    assert_eq!(data.pooled_from.as_deref(), Some("backend_and_data"));
    assert_eq!(backend.pass, data.pass);
    assert!(close(data.evidence, 8.0, 1e-9));
    assert_eq!(data.counts.of, 0, "no card of its own yet");

    // A group with nothing in it falls to the worker's rate on every type.
    let docs = book.record(&worker("codex"), "docs");
    assert_eq!(docs.pooled_from.as_deref(), Some(POOLED_ALL_TYPES));
    let migration = book.record(&worker("claude-code"), "migration");
    assert_eq!(migration.pooled_from.as_deref(), Some(POOLED_ALL_TYPES));

    // A worker with no evidence starts from the crew's rate: 9 passes of 14, on a prior of
    // 1.5 of 3.
    let newcomer = book.record(&worker("agy"), "frontend");
    assert_eq!(newcomer.pooled_from.as_deref(), Some(POOLED_CREW));
    let crew_rate = (1.5 + 9.0) / (3.0 + 14.0);
    assert!(close(newcomer.pass.mean(), crew_rate, 1e-9));
    assert!(close(newcomer.pass.alpha + newcomer.pass.beta, 3.0, 1e-9));

    // Each worker's prior leans on what it does elsewhere.
    let failing = book.record(&worker("claude-code"), "frontend");
    assert!(failing.pass.mean() < crew_rate && crew_rate < backend.pass.mean());

    // Past ten recent cards, a cell stands on its own.
    log.extend(cards(50, "codex", "backend", 2, 2, 0.0));
    let book = Book::build(&lines(log), &settings, AS_OF);
    let backend = book.record(&worker("codex"), "backend");
    assert_eq!(backend.pooled_from, None);
    assert!(close(backend.evidence, 12.0, 1e-9));
    assert_eq!(
        book.record(&worker("codex"), "data").pooled_from.as_deref(),
        Some("backend_and_data")
    );
}

#[test]
fn records_decay_by_half_life_prior_stays() {
    let settings = configured_prior("codex", "backend", 0.8);
    let failures = |days: f64| lines(cards(1, "codex", "backend", 0, 12, days));

    let fresh = Book::build(&failures(0.0), &settings, AS_OF).record(&worker("codex"), "backend");
    assert!(close(fresh.evidence, 12.0, 1e-9));
    assert!(close(fresh.pass.mean(), 2.4 / 15.0, 1e-9));

    // Two half-lives: each card counts a quarter.
    let old = Book::build(&failures(60.0), &settings, AS_OF).record(&worker("codex"), "backend");
    assert!(close(old.evidence, 3.0, 1e-9));
    assert!(close(old.pass.mean(), 2.4 / 6.0, 1e-9));

    // The prior never fades, so very old evidence drifts back to it.
    let ancient =
        Book::build(&failures(3650.0), &settings, AS_OF).record(&worker("codex"), "backend");
    assert!(close(ancient.pass.mean(), 0.8, 1e-3));
    for record in [&fresh, &old, &ancient] {
        let prior_left = record.pass.alpha + record.pass.beta - record.evidence;
        assert!(close(prior_left, settings.prior_strength, 1e-9));
    }

    let slower = RoutingSettings {
        half_life_days: 60.0,
        ..settings.clone()
    };
    let half = Book::build(&failures(60.0), &slower, AS_OF).record(&worker("codex"), "backend");
    assert!(close(half.evidence, 6.0, 1e-9));
}

#[test]
fn records_shrink_when_model_id_changes() {
    let settings = own_cells();
    let history = |old: Option<&str>, new: Option<&str>| {
        let mut log: Vec<Outcome> = cards(1, "codex", "backend", 0, 10, 0.0)
            .into_iter()
            .map(|outcome| with_model(outcome, old))
            .collect();
        log.push(with_model(outcome(20, "codex", "backend", true, 0.0), new));
        Book::build(&lines(log), &settings, AS_OF).record(&worker("codex"), "backend")
    };

    let moved = history(Some("gpt-5.5"), Some("gpt-5.6"));
    assert_eq!(moved.model_id.as_deref(), Some("gpt-5.6"));
    assert!(close(
        moved.evidence,
        10.0 * settings.model_change_keep + 1.0,
        1e-9
    ));

    let same = history(Some("gpt-5.5"), Some("gpt-5.5"));
    assert!(close(same.evidence, 11.0, 1e-9));
    assert!(
        moved.pass.mean() > same.pass.mean(),
        "the new model gets tried again"
    );

    // A harness update that names no model resets nothing.
    let unnamed = history(None, Some("gpt-5.5"));
    assert!(close(unnamed.evidence, 11.0, 1e-9));
    assert_eq!(unnamed.pass, same.pass);
}

#[test]
fn records_retries_and_fallbacks_not_first_pass() {
    let settings = own_cells();
    let mut log = cards(1, "codex", "backend", 3, 0, 0.0);
    for (n, assigned) in [(10, Assignment::Fallback), (11, Assignment::Retry)] {
        for i in 0..3 {
            let mut inherited = outcome(n * 10 + i, "codex", "backend", false, 0.0);
            inherited.assigned = Some(assigned);
            log.push(inherited);
        }
    }
    let book = Book::build(&lines(log.clone()), &settings, AS_OF);
    let record = book.record(&worker("codex"), "backend");
    assert_eq!(record.counts, TrackRecord { passed: 3, of: 3 });
    assert!(close(record.evidence, 3.0, 1e-9));

    let first_pass_only: Vec<Outcome> = log
        .into_iter()
        .filter(|outcome| outcome.assigned == Some(Assignment::Exploit))
        .collect();
    let without = Book::build(&lines(first_pass_only), &settings, AS_OF);
    assert_eq!(
        without.record(&worker("codex"), "backend").pass,
        record.pass
    );
}

#[test]
fn records_pins_and_resets_from_settings() {
    let mut settings = own_cells();
    settings.pins = BTreeMap::from([
        (
            "codex".to_owned(),
            BTreeMap::from([("backend".to_owned(), 0.3)]),
        ),
        (
            "agy/gemini-3.6-flash".to_owned(),
            BTreeMap::from([("docs".to_owned(), 0.65)]),
        ),
    ]);
    settings.resets = BTreeMap::from([
        ("claude-code".to_owned(), "2026-09-20".to_owned()),
        ("opencode".to_owned(), "last tuesday".to_owned()),
    ]);
    let mut log = cards(1, "codex/gpt-5.6", "backend", 10, 0, 0.0);
    log.extend(cards(20, "claude-code/opus", "backend", 0, 10, 21.0));
    log.extend(cards(40, "claude-code/opus", "backend", 2, 0, 5.0));
    log.extend(cards(60, "opencode", "backend", 1, 1, 30.0));
    let book = Book::build(&lines(log), &settings, AS_OF);

    // A harness's pin covers every model it runs, whatever the outcomes say.
    let pinned = book.record(&worker("codex/gpt-5.6"), "backend");
    assert!(pinned.pinned);
    assert!(close(pinned.pass.mean(), 0.3, 1e-12));
    assert_eq!(pinned.counts, TrackRecord { passed: 10, of: 10 });
    assert!(!book.record(&worker("codex/gpt-5.6"), "frontend").pinned);

    // A pinned cell shows in the book before it has an outcome.
    let unseen = book
        .rows()
        .iter()
        .find(|record| record.worker == worker("agy/gemini-3.6-flash"))
        .expect("pinned row");
    assert!(unseen.pinned && close(unseen.pass.mean(), 0.65, 1e-12));

    // A reset drops what came before its date, and a date that doesn't parse is ignored.
    let reset = book.record(&worker("claude-code/opus"), "backend");
    assert_eq!(reset.counts, TrackRecord { passed: 2, of: 2 });
    assert_eq!(
        book.record(&worker("opencode"), "backend").counts,
        TrackRecord { passed: 1, of: 2 }
    );
}

#[test]
fn records_claim_little_below_threshold() {
    let settings = own_cells();
    let mut log = cards(1, "opencode", "backend", 6, 4, 0.0);
    log.extend(cards(20, "agy", "backend", 4, 6, 0.0));
    log.extend(cards(40, "codex", "frontend", 45, 5, 0.0));
    log.extend(cards(100, "claude-code", "frontend", 15, 35, 0.0));
    let book = Book::build(&lines(log), &settings, AS_OF);
    let (codex, claude) = (worker("codex"), worker("claude-code"));

    // Six of ten against four of ten looks better, but it isn't clear yet.
    let (opencode, agy) = (worker("opencode"), worker("agy"));
    assert!(
        book.record(&opencode, "backend").pass.mean() > book.record(&agy, "backend").pass.mean()
    );
    assert_eq!(
        book.clearly_better(&opencode, &agy, "backend", &settings),
        None
    );
    assert_eq!(
        book.clearly_better(&agy, &opencode, "backend", &settings),
        None
    );
    assert_eq!(
        book.clearly_better(&codex, &claude, "frontend", &settings),
        Some(true)
    );
    assert_eq!(
        book.clearly_better(&claude, &codex, "frontend", &settings),
        Some(false)
    );
    let never = RoutingSettings {
        claim_above: 1.0,
        ..settings.clone()
    };
    assert_eq!(
        book.clearly_better(&codex, &claude, "frontend", &never),
        None
    );

    // The same posteriors always give the same probability.
    let strong = Beta {
        alpha: 9.0,
        beta: 1.0,
    };
    let weak = Beta {
        alpha: 1.0,
        beta: 9.0,
    };
    assert_eq!(strong.prob_above(&weak), strong.prob_above(&weak));
    assert!(strong.prob_above(&weak) > 0.99);
    assert!(close(strong.prob_above(&strong), 0.5, 0.02));
}

#[test]
fn records_approval_against_each_reviewers_usual_rate() {
    let settings = own_cells();
    let reviewed = |n: u32, owner: &str, reviewer: &str, approved: bool| {
        let mut card = outcome(n, owner, "backend", true, 0.0);
        card.reviewer = Some(worker(reviewer));
        card.approved_first_review = Some(approved);
        card
    };
    let mut log = Vec::new();
    for i in 0..10 {
        log.push(reviewed(i, "opencode", "strict", i < 2));
        log.push(reviewed(20 + i, "opencode", "lenient", i < 9));
    }
    for i in 0..3 {
        log.push(reviewed(40 + i, "codex", "strict", false));
        log.push(reviewed(50 + i, "claude-code", "lenient", false));
    }
    let book = Book::build(&lines(log), &settings, AS_OF);
    let strict = book.record(&worker("codex"), "backend").approval.mean();
    let lenient = book
        .record(&worker("claude-code"), "backend")
        .approval
        .mean();
    assert!(
        strict > lenient,
        "a strict reviewer's rejection costs less: {strict} vs {lenient}"
    );
}

#[test]
fn records_cost_and_time_against_the_type_median() {
    let settings = own_cells();
    let mut log = Vec::new();
    for i in 0..5 {
        let mut cheap = outcome(i, "codex", "backend", true, 0.0);
        cheap.minutes = Some(10.0);
        let mut dear = outcome(10 + i, "claude-code", "backend", true, 0.0);
        dear.spent_usd = 10.0;
        log.extend([cheap, dear]);
    }
    let book = Book::build(&lines(log), &settings, AS_OF);
    let codex = book.record(&worker("codex"), "backend");
    let claude = book.record(&worker("claude-code"), "backend");
    assert!(close(codex.cost_ratio, 1.0, 1e-12));
    assert!(close(claude.cost_ratio, 2.0, 1e-12), "capped at 2");
    assert!(close(codex.time_ratio, 1.0, 1e-12));
    assert!(close(claude.time_ratio, 1.0, 1e-12), "no minutes, no ratio");
    assert!(close(
        book.record(&worker("agy"), "backend").cost_ratio,
        1.0,
        1e-12
    ));
}
