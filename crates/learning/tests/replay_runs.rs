//! Replaying a candidate against its parent: equal budgets, prefix replay, the stopping rule,
//! the month's budget, and which workers may spend on it.

mod replay_support;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use time::OffsetDateTime;

use argonauts_core::AuthMode;
use argonauts_core::limits::GateRunner;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::agents::{AgentSettings, EndpointSettings};
use argonauts_core::workflow::StandardPhase;
use argonauts_learning::LearningError;
use argonauts_learning::evolution::Thresholds;
use argonauts_learning::recipes::LoadedRecipe;
use argonauts_learning::replay::{
    ReplayReport, ReplayRequest, ReplayRunner, Versions, replay, replay_workers, start_phase,
};

use replay_support::{Fixture, RUN_SPEND_USD, ScriptedRunner, ShellGates, files_under, recipe};

/// Tasks named `names`, all in the tuning set.
async fn tuning_fixture(names: &[&str], settings: &Settings) -> Fixture {
    let mut fixture = Fixture::new();
    let tasks: Vec<_> = names.iter().map(|name| fixture.task(name)).collect();
    let all: Vec<_> = tasks.iter().map(|task| (task, false)).collect();
    fixture.accept(&all, settings).await;
    fixture
}

fn base_settings() -> Settings {
    let mut settings = replay_support::settings();
    settings.evolution.replay.holdout_tasks = 0;
    settings
}

fn versions() -> (LoadedRecipe, LoadedRecipe) {
    (
        recipe(1, json!({ "critique": { "rounds": 1 } })),
        recipe(2, json!({ "critique": { "rounds": 2 } })),
    )
}

fn child_only(request: &ReplayRequest, _attempt: u32) -> bool {
    request.recipe.recipe.version == 2
}

async fn run(
    fixture: &Fixture,
    settings: &Settings,
    budget_left_usd: f64,
    runner: &Arc<ScriptedRunner>,
) -> Result<ReplayReport, LearningError> {
    let (parent, child) = versions();
    run_versions(fixture, settings, &parent, &child, budget_left_usd, runner).await
}

async fn run_versions(
    fixture: &Fixture,
    settings: &Settings,
    parent: &LoadedRecipe,
    child: &LoadedRecipe,
    budget_left_usd: f64,
    runner: &Arc<ScriptedRunner>,
) -> Result<ReplayReport, LearningError> {
    let thresholds = Thresholds {
        min_gain_points: settings.evolution.min_gain_points,
        max_cost_increase: settings.evolution.max_cost_increase,
        not_worse: false,
    };
    let versions = Versions {
        parent,
        child,
        thresholds: &thresholds,
    };
    run_with(fixture, settings, versions, budget_left_usd, runner).await
}

async fn run_with(
    fixture: &Fixture,
    settings: &Settings,
    versions: Versions<'_>,
    budget_left_usd: f64,
    runner: &Arc<ScriptedRunner>,
) -> Result<ReplayReport, LearningError> {
    let gates: Arc<dyn GateRunner> = Arc::new(ShellGates::default());
    let runner: Arc<dyn ReplayRunner> = runner.clone();
    replay(
        &fixture.set,
        versions,
        settings,
        budget_left_usd,
        runner,
        gates,
    )
    .await
}

#[tokio::test]
async fn replay_pairs_equal_budgets() {
    let mut settings = base_settings();
    settings.evolution.replay.sequential = false;
    let fixture = tuning_fixture(&["alpha", "beta", "gamma"], &settings).await;
    let [alpha, beta, gamma] = [0, 1, 2].map(|i| fixture.tasks[i].id.clone());
    let decided = alpha.clone();
    let both = beta.clone();
    let runner = ScriptedRunner::new(&fixture.tasks, move |request, _| {
        if request.entry.id == decided {
            request.recipe.recipe.version == 2
        } else {
            request.entry.id == both
        }
    });

    let report = run(&fixture, &settings, 1000.0, &runner).await.unwrap();

    let requests = runner.requests();
    let mut pairs: BTreeMap<(String, u32), Vec<&ReplayRequest>> = BTreeMap::new();
    let mut seen: BTreeMap<(String, u32), u32> = BTreeMap::new();
    for request in &requests {
        let count = seen
            .entry((request.entry.id.clone(), request.recipe.recipe.version))
            .or_default();
        *count += 1;
        pairs
            .entry((request.entry.id.clone(), *count))
            .or_default()
            .push(request);
    }
    for ((task, attempt), pair) in &pairs {
        let versions: Vec<u32> = pair
            .iter()
            .map(|request| request.recipe.recipe.version)
            .collect();
        assert_eq!(versions, [1, 2], "{task} attempt {attempt}");
        assert_eq!(pair[0].budget_usd, pair[1].budget_usd);
        assert_eq!(pair[0].budget_usd, RUN_SPEND_USD);
        assert_eq!(pair[0].workers, pair[1].workers);
        assert_eq!(pair[0].from_phase, pair[1].from_phase);
        assert!(pair[0].entry.tests.is_empty() && !pair[0].entry.holdout);
    }
    let attempts = |task: &str| pairs.keys().filter(|(id, _)| id == task).count();
    assert_eq!(
        attempts(&alpha),
        3,
        "a task the versions split keeps all its runs"
    );
    assert_eq!(attempts(&beta), 1, "both passed it, so it's dropped");
    assert_eq!(attempts(&gamma), 1, "both failed it, so it's dropped");

    assert_eq!(report.results.len(), 10);
    assert_eq!(report.spent_usd, 10.0 * RUN_SPEND_USD);
    assert!(!report.stopped_early && !report.out_of_budget);
    let passed = |version: u32| {
        report
            .results
            .iter()
            .filter(|result| result.recipe.version == version && result.passed)
            .count()
    };
    assert_eq!((passed(1), passed(2)), (1, 4));
    assert!(files_under(&fixture.set.repos_dir()).is_empty());
    assert!(!runner.saw_hidden());
}

/// Every replayed run is told what its cards keep hidden: the task's hidden tests at their
/// repo paths and the replay folder, while the tests themselves never travel with it.
#[tokio::test]
async fn replay_cards_hide_the_tests_and_the_replay_folder() {
    let mut settings = base_settings();
    settings.evolution.replay.sequential = false;
    let fixture = tuning_fixture(&["alpha"], &settings).await;
    let runner = ScriptedRunner::new(&fixture.tasks, child_only);
    run(&fixture, &settings, 1000.0, &runner).await.unwrap();
    let replay_dir = fixture.store.replay_dir().to_string_lossy().into_owned();
    let requests = runner.requests();
    assert!(!requests.is_empty());
    for request in &requests {
        assert_eq!(
            request.hidden_paths,
            ["tests/alpha.sh", replay_dir.as_str()]
        );
        assert!(request.entry.tests.is_empty());
    }
    assert!(!runner.saw_hidden());
}

#[tokio::test]
async fn prefix_replay_starts_at_the_changed_phase() {
    let parent = recipe(
        1,
        json!({
            "critique": { "rounds": 1, "checklist": ["errors"] },
            "divide": { "max_cards": 4 },
            "budget": { "per_card_usd": 2.0 },
        }),
    );
    let changed = |settings: serde_json::Value| -> LoadedRecipe {
        let mut child = parent.clone();
        child.recipe.version = 2;
        child.sha256 = "sha-v2".to_owned();
        if let serde_json::Value::Object(map) = settings {
            child.recipe.settings.extend(map);
        }
        child
    };
    let cases = [
        (
            json!({ "critique": { "rounds": 2, "checklist": ["errors"] } }),
            Some(StandardPhase::Plan),
        ),
        (
            json!({ "critique": { "rounds": 1, "checklist": ["errors", "retries"] } }),
            Some(StandardPhase::Plan),
        ),
        (
            json!({ "divide": { "max_cards": 6 } }),
            Some(StandardPhase::Do),
        ),
        (
            json!({ "budget": { "per_card_usd": 3.0 } }),
            Some(StandardPhase::Do),
        ),
        (json!({ "gates": { "timeout": "10m" } }), None),
    ];
    for (settings, phase) in cases {
        assert_eq!(
            start_phase(&parent, &changed(settings.clone())),
            phase,
            "{settings}"
        );
    }
    let prompts = |name: &str| {
        let mut child = changed(json!({}));
        child
            .recipe
            .prompts
            .insert(name.to_owned(), format!("{name}-v2.md"));
        start_phase(&parent, &child)
    };
    assert_eq!(prompts("drafting"), Some(StandardPhase::Plan));
    assert_eq!(prompts("build"), Some(StandardPhase::Do));
    let mut rewired = changed(json!({}));
    rewired.recipe.workflow = Some("api-feature.star".to_owned());
    assert_eq!(start_phase(&parent, &rewired), None);

    let settings = base_settings();
    let fixture = tuning_fixture(&["alpha"], &settings).await;
    let child = changed(json!({ "budget": { "per_card_usd": 3.0 } }));
    let runner = ScriptedRunner::new(&fixture.tasks, child_only);
    run_versions(&fixture, &settings, &parent, &child, 1000.0, &runner)
        .await
        .unwrap();
    let requests = runner.requests();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| request.from_phase == Some(StandardPhase::Do))
    );

    let mut whole = settings.clone();
    whole.evolution.replay.prefix_replay = false;
    let fixture = tuning_fixture(&["alpha"], &whole).await;
    let runner = ScriptedRunner::new(&fixture.tasks, child_only);
    run_versions(&fixture, &whole, &parent, &child, 1000.0, &runner)
        .await
        .unwrap();
    assert!(
        runner
            .requests()
            .iter()
            .all(|request| request.from_phase.is_none())
    );
}

#[tokio::test]
async fn sequential_replay_stops_when_clear() {
    let mut settings = base_settings();
    let fixture = tuning_fixture(&["alpha", "beta", "gamma"], &settings).await;
    let planned = fixture.tasks.len() * 3 * 2;

    let runner = ScriptedRunner::new(&fixture.tasks, child_only);
    let better = run(&fixture, &settings, 1000.0, &runner).await.unwrap();
    assert!(better.stopped_early);
    assert_eq!(
        better.results.len(),
        (3 + 2) * 2,
        "every task's first pair, then until even the worst case keeps a 10 point gain"
    );
    assert_eq!(runner.requests().len(), better.results.len());

    let runner = ScriptedRunner::new(&fixture.tasks, |request, _| {
        request.recipe.recipe.version == 1
    });
    let worse = run(&fixture, &settings, 1000.0, &runner).await.unwrap();
    assert!(worse.stopped_early);
    assert!(worse.results.len() < planned);
    assert_eq!(worse.results.len() % 2, 0, "it stops between pairs");

    settings.evolution.replay.sequential = false;
    let runner = ScriptedRunner::new(&fixture.tasks, child_only);
    let full = run(&fixture, &settings, 1000.0, &runner).await.unwrap();
    assert!(!full.stopped_early);
    assert_eq!(full.results.len(), planned);
}

#[tokio::test]
async fn holdout_runs_only_after_tuning_passes() {
    let settings = replay_support::settings();
    let mut fixture = Fixture::new();
    let tuning = fixture.task("alpha");
    let held = fixture.task("beta");
    fixture
        .accept(&[(&tuning, false), (&held, true)], &settings)
        .await;

    let runner = ScriptedRunner::new(&fixture.tasks, child_only);
    let better = run(&fixture, &settings, 1000.0, &runner).await.unwrap();
    let holdout: Vec<_> = better
        .results
        .iter()
        .filter(|result| result.holdout)
        .collect();
    assert_eq!(holdout.len(), 2);
    assert!(
        holdout
            .iter()
            .all(|result| result.task == held.id && result.attempt == 1)
    );
    let first_holdout = better
        .results
        .iter()
        .position(|result| result.holdout)
        .unwrap();
    assert!(
        better.results[..first_holdout]
            .iter()
            .all(|result| result.task == tuning.id)
    );
    assert_eq!(fixture.set.holdout_candidates().unwrap(), 1);
    assert!(!runner.saw_hidden());

    let runner = ScriptedRunner::new(&fixture.tasks, |_, _| false);
    let flat = run(&fixture, &settings, 1000.0, &runner).await.unwrap();
    assert!(flat.results.iter().all(|result| !result.holdout));
    assert_eq!(fixture.set.holdout_candidates().unwrap(), 1);
}

/// The stopping rule takes its margin from the bar frozen when the candidate was queued, so a
/// `min_gain_points` raised since, or a class added to `not_worse_classes`, moves nothing.
#[tokio::test]
async fn replay_margin_is_the_candidates_own() {
    let mut settings = replay_support::settings();
    settings.evolution.replay.sequential = false;
    let mut fixture = Fixture::new();
    let won = fixture.task("alpha");
    let lost = fixture.task("beta");
    let held = fixture.task("gamma");
    fixture
        .accept(&[(&won, false), (&lost, false), (&held, true)], &settings)
        .await;
    // Raised after the candidates were queued, and rounds listed as "not worse" since.
    settings.evolution.min_gain_points = 90.0;
    settings.evolution.not_worse_classes = vec!["critique_rounds".to_owned()];
    let (parent, child) = versions();
    let bar = |min_gain_points: f64, not_worse: bool| Thresholds {
        min_gain_points,
        max_cost_increase: 0.1,
        not_worse,
    };
    let holdout_ran = |report: &ReplayReport| report.results.iter().any(|result| result.holdout);

    // The child wins one task of two: 50 points, past a bar of 10, short of 90.
    let wins_alpha = {
        let alpha = won.id.clone();
        move |request: &ReplayRequest, _: u32| {
            request.recipe.recipe.version == 2 && request.entry.id == alpha
        }
    };
    for (thresholds, passes) in [(bar(10.0, false), true), (bar(60.0, false), false)] {
        let runner = ScriptedRunner::new(&fixture.tasks, wins_alpha.clone());
        let versions = Versions {
            parent: &parent,
            child: &child,
            thresholds: &thresholds,
        };
        let report = run_with(&fixture, &settings, versions, 1000.0, &runner)
            .await
            .unwrap();
        assert_eq!(holdout_ran(&report), passes, "{thresholds:?}");
    }

    // Level on every task: only a candidate queued as "not worse" goes on to the holdout.
    for (thresholds, passes) in [(bar(10.0, true), true), (bar(10.0, false), false)] {
        let runner = ScriptedRunner::new(&fixture.tasks, |_, _| false);
        let versions = Versions {
            parent: &parent,
            child: &child,
            thresholds: &thresholds,
        };
        let report = run_with(&fixture, &settings, versions, 1000.0, &runner)
            .await
            .unwrap();
        assert_eq!(holdout_ran(&report), passes, "{thresholds:?}");
    }
}

#[tokio::test]
async fn replay_waits_at_the_monthly_budget() {
    let mut settings = base_settings();
    settings.evolution.replay.sequential = false;
    let fixture = tuning_fixture(&["alpha", "beta"], &settings).await;
    let runner = ScriptedRunner::new(&fixture.tasks, child_only);

    let nothing = run(&fixture, &settings, 0.5, &runner).await.unwrap();
    assert!(nothing.out_of_budget);
    assert!(nothing.results.is_empty());
    assert!(runner.requests().is_empty());

    let first = run(&fixture, &settings, 2.5, &runner).await.unwrap();
    assert!(first.out_of_budget);
    assert_eq!(
        first.results.len(),
        2,
        "one whole pair fits, the next doesn't"
    );
    assert_eq!(first.spent_usd, 2.0 * RUN_SPEND_USD);
    assert_eq!(runner.requests().len(), 2);
    let now = OffsetDateTime::now_utc();
    assert_eq!(
        fixture.set.spent_in_month(now).unwrap(),
        2.0 * RUN_SPEND_USD
    );
    let mut limited = settings.clone();
    limited.limits.evolution_usd_per_month = 5.0;
    assert_eq!(fixture.set.budget_left(&limited, now).unwrap(), 3.0);

    let rest = run(&fixture, &settings, 1000.0, &runner).await.unwrap();
    assert!(!rest.out_of_budget);
    assert_eq!(rest.results[..2], first.results[..]);
    assert_eq!(rest.results.len(), 2 * 3 * 2);
    assert_eq!(
        runner.requests().len(),
        rest.results.len(),
        "nothing ran twice"
    );
    assert_eq!(rest.spent_usd, rest.results.len() as f64 * RUN_SPEND_USD);
    assert_eq!(fixture.set.spent_in_month(now).unwrap(), rest.spent_usd);
}

#[tokio::test]
async fn replays_skip_subscriptions_by_default() {
    let mut settings = base_settings();
    let api_agent = AgentSettings {
        auth: AuthMode::ApiKey,
        model: Some("gpt-5.6-luna".to_owned()),
        ..AgentSettings::default()
    };
    settings.agents.insert("codex".to_owned(), api_agent);
    settings
        .agents
        .insert("claude-code".to_owned(), AgentSettings::default());
    for (id, endpoint) in [
        (
            "openrouter",
            EndpointSettings {
                allow_api_billing: true,
                ..EndpointSettings::default()
            },
        ),
        ("work-api", EndpointSettings::default()),
        (
            "ollama",
            EndpointSettings {
                local: true,
                ..EndpointSettings::default()
            },
        ),
    ] {
        settings.endpoints.insert(id.to_owned(), endpoint);
    }
    settings.crew = [
        "claude-code",
        "codex",
        "openrouter/open-model",
        "work-api/big",
        "ollama/qwen",
    ]
    .map(|worker| worker.parse::<WorkerRef>().unwrap())
    .to_vec();
    settings.default_agent = WorkerRef::new("claude-code", None);

    let names = |workers: &[WorkerRef]| -> Vec<String> {
        workers.iter().map(ToString::to_string).collect()
    };
    assert_eq!(
        names(&replay_workers(&settings)),
        ["codex/gpt-5.6-luna", "openrouter/open-model", "ollama/qwen"]
    );
    let mut generous = settings.clone();
    generous.evolution.replay.use_subscriptions = true;
    assert_eq!(
        names(&replay_workers(&generous)),
        [
            "claude-code",
            "codex/gpt-5.6-luna",
            "openrouter/open-model",
            "work-api/big",
            "ollama/qwen"
        ]
    );

    let fixture = tuning_fixture(&["alpha"], &settings).await;
    let runner = ScriptedRunner::new(&fixture.tasks, child_only);
    run(&fixture, &settings, 1000.0, &runner).await.unwrap();
    for request in runner.requests() {
        assert_eq!(
            names(&request.workers),
            ["codex/gpt-5.6-luna", "openrouter/open-model", "ollama/qwen"]
        );
    }

    let mut subscriptions_only = settings.clone();
    subscriptions_only.crew = vec![WorkerRef::new("claude-code", None)];
    let runner = ScriptedRunner::new(&fixture.tasks, child_only);
    let refused = run(&fixture, &subscriptions_only, 1000.0, &runner)
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("use_subscriptions"),
        "{refused}"
    );
    assert!(runner.requests().is_empty());
}
