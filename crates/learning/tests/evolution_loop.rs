//! The evolution loop: the mutation rules, the paired comparison, the holdout, the canary,
//! rollbacks, the stages of autonomy, and the monthly budget. Replays are synthetic results and
//! every repository is a temporary one.

use std::path::{Path, PathBuf};

use rand::SeedableRng;
use rand::rngs::StdRng;
use time::{Duration, OffsetDateTime};

use argonauts_core::RunId;
use argonauts_core::events::run::{Party, RunOutcome};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::evolution::EvolutionStage;
use argonauts_core::steps::card::ProcessChange;
use argonauts_core::workflow::{Step, StepKind, Workflow};
use argonauts_learning::evolution::{
    Action, CandidateState, Evolution, Stage, Thresholds, Tripwire, check_evolvable,
    check_mutation, compare, holdout_ok, stage, tripwires,
};
use argonauts_learning::outcomes::{self, LogLine, Reversal};
use argonauts_learning::recipes::{self, ChangeClass, LoadedRecipe, Recipe};
use argonauts_learning::{RecipeRef, ReplayResult, RunRecord, Store, git};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/recipes")
}

fn start() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap()
}

/// A git repository with `api-feature` v1 committed, and the conductor's state beside it.
struct World {
    _dir: tempfile::TempDir,
    store: Store,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let store = Store::new(dir.path().join("state"), &repo);
        let folder = store.recipes_dir().join("api-feature");
        std::fs::create_dir_all(&folder).unwrap();
        for entry in std::fs::read_dir(fixtures().join("api-feature")).unwrap() {
            let path = entry.unwrap().path();
            std::fs::copy(&path, folder.join(path.file_name().unwrap())).unwrap();
        }
        git::run(&repo, &["init", "-q", "-b", "main"]).unwrap();
        commit_all(&repo, "first");
        Self { _dir: dir, store }
    }

    fn repo(&self) -> &Path {
        &self.store.repo_root
    }

    fn evolution(&self) -> Evolution {
        Evolution::open(&self.store).unwrap().at(start())
    }

    fn folder(&self) -> PathBuf {
        self.store.recipes_dir().join("api-feature")
    }

    fn parent(&self) -> LoadedRecipe {
        recipes::load_file(&self.folder().join("v1.yaml")).unwrap()
    }
}

fn commit_all(repo: &Path, message: &str) {
    git::run(repo, &["add", "-A"]).unwrap();
    git::run(
        repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@local",
            "commit",
            "-qm",
            message,
        ],
    )
    .unwrap();
}

fn settings() -> Settings {
    let mut settings = Settings::default();
    settings.skills.export_after_canary = false;
    settings
}

fn api(version: u32) -> RecipeRef {
    RecipeRef {
        name: "api-feature".to_owned(),
        version,
    }
}

fn run(n: u32, recipe: Option<RecipeRef>) -> RunRecord {
    RunRecord {
        run: RunId::at(start() + Duration::minutes(i64::from(n)), n as u16),
        at: start() + Duration::minutes(i64::from(n)),
        task_type: "backend".to_owned(),
        recipe,
        workflow: Some(("review-loop".to_owned(), "f00d".to_owned())),
        outcome: RunOutcome::Merged,
        escalations: 0,
        final_checks_passed: true,
        cost_usd: 10.0,
        minutes: 20.0,
        canary: false,
    }
}

fn proposal(setting: &str, value: &str) -> ProcessChange {
    ProcessChange {
        setting: setting.to_owned(),
        value: value.to_owned(),
        why: "the last run missed it".to_owned(),
    }
}

/// Replays of `tasks`, each `(parent passes, child passes)` out of three attempts.
fn replays(
    parent: &RecipeRef,
    child: &RecipeRef,
    tasks: &[(u32, u32)],
    costs: (f64, f64),
    holdout: bool,
) -> Vec<ReplayResult> {
    let mut results = Vec::new();
    for (i, (parent_passes, child_passes)) in tasks.iter().enumerate() {
        for attempt in 1..=3 {
            for (recipe, passes, cost) in [
                (parent, parent_passes, costs.0),
                (child, child_passes, costs.1),
            ] {
                results.push(ReplayResult {
                    task: format!("{}task-{i}", if holdout { "holdout-" } else { "" }),
                    recipe: recipe.clone(),
                    attempt,
                    passed: attempt <= *passes,
                    cost_usd: cost,
                    minutes: 30.0,
                    holdout,
                });
            }
        }
    }
    results
}

/// Thirty tasks where the child solves `wins` more than the parent and `losses` fewer.
fn tuning(
    parent: &RecipeRef,
    child: &RecipeRef,
    wins: usize,
    losses: usize,
    costs: (f64, f64),
) -> Vec<ReplayResult> {
    let mut tasks = vec![(3, 3); 15];
    tasks.extend(vec![(0, 0); 15 - wins - losses]);
    tasks.extend(vec![(0, 3); wins]);
    tasks.extend(vec![(3, 0); losses]);
    replays(parent, child, &tasks, costs, false)
}

/// `results` with a holdout both versions pass and fail alike, so it shows no regression.
fn and_level_holdout(
    mut results: Vec<ReplayResult>,
    parent: &RecipeRef,
    child: &RecipeRef,
) -> Vec<ReplayResult> {
    results.extend(replays(
        parent,
        child,
        &[(3, 3), (0, 0)],
        (10.0, 10.0),
        true,
    ));
    results
}

fn thresholds() -> Thresholds {
    Thresholds {
        min_gain_points: 10.0,
        max_cost_increase: 0.10,
        not_worse: false,
    }
}

fn find<'a>(actions: &'a [Action], kind: &str) -> Vec<&'a Action> {
    actions
        .iter()
        .filter(|action| serde_json::to_value(action).unwrap()["action"].as_str() == Some(kind))
        .collect()
}

fn state_of(evolution: &Evolution, id: &str) -> CandidateState {
    evolution.candidate(id).unwrap().unwrap().state
}

/// Proposes, replays with a clear win, and leaves `api-feature-v2` waiting for promotion.
fn awaiting_promotion(world: &World, settings: &Settings) -> Evolution {
    let evolution = world.evolution();
    let actions = evolution
        .after_run(
            settings,
            &run(1, Some(api(1))),
            Some(&proposal("critique.rounds", "2")),
        )
        .unwrap();
    assert_eq!(find(&actions, "replay").len(), 1, "{actions:?}");
    let mut results = tuning(&api(1), &api(2), 6, 0, (10.0, 10.0));
    results.extend(replays(
        &api(1),
        &api(2),
        &[(3, 3), (0, 0)],
        (10.0, 10.0),
        true,
    ));
    let actions = evolution
        .after_replay(settings, "api-feature-v2", &results, 50.0)
        .unwrap();
    assert_eq!(find(&actions, "ask_promotion").len(), 1, "{actions:?}");
    evolution
}

/// The child the table checks: the parent changed by `edit`, written as v2 beside it.
fn child_of(world: &World, name: &str, edit: impl FnOnce(&mut Recipe, &Path)) -> LoadedRecipe {
    let folder = world.store.recipes_dir().join(format!("case-{name}"));
    std::fs::create_dir_all(&folder).unwrap();
    for entry in std::fs::read_dir(world.folder()).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, folder.join(path.file_name().unwrap())).unwrap();
    }
    let mut recipe = world.parent().recipe;
    recipe.version = 2;
    recipe.parent = Some(1);
    edit(&mut recipe, &folder);
    let path = folder.join("v2.yaml");
    std::fs::write(&path, recipes::render(&recipe).unwrap()).unwrap();
    recipes::load_file(&path).unwrap()
}

type Edit = Box<dyn FnOnce(&mut Recipe, &Path)>;

fn edit(change: impl FnOnce(&mut Recipe, &Path) + 'static) -> Edit {
    Box::new(change)
}

fn set(recipe: &mut Recipe, section: &str, key: &str, value: serde_json::Value) {
    let table = recipe
        .settings
        .entry(section.to_owned())
        .or_insert_with(|| serde_json::json!({}));
    table[key] = value;
}

/// Writes the parent's workflow, changed by `edit`, beside the child.
fn with_workflow(recipe: &mut Recipe, folder: &Path, edit: impl FnOnce(&mut Vec<Step>)) {
    let mut workflow: Workflow =
        serde_saphyr::from_str(&std::fs::read_to_string(folder.join("review-loop.yaml")).unwrap())
            .unwrap();
    edit(&mut workflow.phases[2].steps[0].steps[0].steps);
    std::fs::write(
        folder.join("review-loop-v2.yaml"),
        serde_saphyr::to_string(&workflow).unwrap(),
    )
    .unwrap();
    recipe.workflow = Some("review-loop-v2.yaml".to_owned());
}

#[test]
fn mutation_rules_table() {
    let world = World::new();
    let parent = world.parent();
    let mut settings = settings();
    settings
        .evolution
        .evolvable
        .extend(["workflow".to_owned(), "crew".to_owned()]);
    settings.budget.run_usd = Some(20.0);

    let cases: Vec<(&str, Edit, Result<ChangeClass, &str>)> = vec![
        (
            "adds a checklist item",
            edit(|r, _| {
                set(
                    r,
                    "critique",
                    "checklist",
                    serde_json::json!(["criteria", "testability", "retry_after"]),
                )
            }),
            Ok(ChangeClass::ChecklistItem),
        ),
        (
            "adds two checklist items",
            edit(|r, _| {
                set(
                    r,
                    "critique",
                    "checklist",
                    serde_json::json!(["criteria", "testability", "a", "b"]),
                )
            }),
            Err("exactly one"),
        ),
        (
            "changes the critique rounds",
            edit(|r, _| set(r, "critique", "rounds", serde_json::json!(2))),
            Ok(ChangeClass::CritiqueRounds),
        ),
        ("changes nothing", edit(|_, _| {}), Err("changes nothing")),
        (
            "changes a key that isn't evolvable",
            edit(|r, _| set(r, "critique", "critic", serde_json::json!("codex"))),
            Err("isn't on your `evolution.evolvable`"),
        ),
        (
            "changes who judges",
            edit(|r, _| set(r, "judging", "method", serde_json::json!("single"))),
            Err("fixed core"),
        ),
        (
            "opens the sandbox",
            edit(|r, _| set(r, "sandbox", "network", serde_json::json!(true))),
            Err("safety key"),
        ),
        (
            "raises a budget above its ceiling",
            edit(|r, _| set(r, "budget", "per_card_usd", serde_json::json!(50))),
            Err("ceiling"),
        ),
        (
            "moves a budget within its ceiling",
            edit(|r, _| set(r, "budget", "per_card_usd", serde_json::json!(3))),
            Ok(ChangeClass::CardBudget),
        ),
        (
            "removes a gate",
            edit(|r, f| with_workflow(r, f, |steps| steps.retain(|s| s.kind != StepKind::Gate))),
            Err("removes a `gate` step"),
        ),
        (
            "adds a reviewer",
            edit(|r, f| {
                with_workflow(r, f, |steps| {
                    let mut third = steps[3].clone();
                    third.name = Some("review-3".to_owned());
                    steps.push(third);
                })
            }),
            Ok(ChangeClass::StepGraph),
        ),
        (
            "moves the gate after the reviews",
            edit(|r, f| {
                with_workflow(r, f, |steps| {
                    let gate = steps.remove(1);
                    steps.push(gate);
                })
            }),
            Ok(ChangeClass::StepGraph),
        ),
        (
            "lets a reviewer see the others",
            edit(|r, f| with_workflow(r, f, |steps| steps[2].blind = false)),
            Err("blindness"),
        ),
        (
            "replaces a reviewer",
            edit(|r, f| {
                with_workflow(r, f, |steps| {
                    steps[2].role = Some(argonauts_core::names::RoleName::Owner)
                })
            }),
            Err("replaces who does it"),
        ),
        (
            "swaps the workflow by name",
            edit(|r, _| r.workflow = Some("coordinated-crew".to_owned())),
            Err("whole workflow"),
        ),
        (
            "fills the crew from one family",
            edit(|r, _| {
                r.settings.insert(
                    "crew".to_owned(),
                    serde_json::json!(["claude-code", "claude-code/opus"]),
                );
            }),
            Err("family"),
        ),
        (
            "adds a third family to the crew",
            edit(|r, _| {
                r.settings.insert(
                    "crew".to_owned(),
                    serde_json::json!(["claude-code", "codex", "antigravity-cli"]),
                );
            }),
            Ok(ChangeClass::CrewComposition),
        ),
        (
            "rewrites the drafting prompt",
            edit(|r, f| {
                std::fs::write(
                    f.join("drafting-v2.md"),
                    "Draft it with Retry-After in mind.\n",
                )
                .unwrap();
                r.prompts
                    .insert("drafting".to_owned(), "drafting-v2.md".to_owned());
            }),
            Ok(ChangeClass::DraftingPrompt),
        ),
        (
            "renames the prompt file, same words",
            edit(|r, f| {
                std::fs::copy(f.join("drafting-v1.md"), f.join("drafting-v2.md")).unwrap();
                r.prompts
                    .insert("drafting".to_owned(), "drafting-v2.md".to_owned());
            }),
            Err("changes nothing"),
        ),
        (
            "names no parent",
            edit(|r, _| r.parent = None),
            Err("as its parent"),
        ),
    ];

    for (name, edit, expected) in cases {
        let child = child_of(&world, &name.replace(' ', "-"), edit);
        let outcome = check_mutation(&parent, &child, &settings);
        match (&outcome, expected) {
            (Ok(change), Ok(class)) => assert_eq!(change.class, class, "{name}"),
            (Err(reason), Err(fragment)) => {
                assert!(reason.contains(fragment), "{name}: {reason}")
            }
            _ => panic!("{name}: expected {expected:?}, got {outcome:?}"),
        }
    }
}

#[test]
fn fixed_core_key_on_evolvable_is_config_error() {
    assert!(check_evolvable(&Settings::default().evolution.evolvable).is_empty());
    let keys = |keys: &[&str]| keys.iter().map(|k| (*k).to_owned()).collect::<Vec<_>>();
    for fixed in [
        "judging.method",
        "review.*",
        "drafting.blind",
        "goal.questions",
        "mapping",
    ] {
        let errors = check_evolvable(&keys(&[fixed]));
        assert_eq!(errors.len(), 1, "{fixed}");
        assert!(errors[0].contains("fixed core"), "{}", errors[0]);
    }
    for safety in [
        "limits.evolution_usd_per_month",
        "gates.run",
        "approvals.merge.mode",
        "budget.run_usd",
    ] {
        let errors = check_evolvable(&keys(&[safety]));
        assert_eq!(errors.len(), 1, "{safety}");
        assert!(errors[0].contains("safety key"), "{}", errors[0]);
    }
    assert_eq!(check_evolvable(&keys(&["*"])).len(), 1);
    assert_eq!(
        check_evolvable(&keys(&["critique.*", "budget.per_card_usd"])).len(),
        0
    );

    // While the list is wrong, nothing evolves, even a change that is on it.
    let world = World::new();
    let mut settings = settings();
    settings
        .evolution
        .evolvable
        .push("judging.method".to_owned());
    let child = child_of(&world, "config", |r, _| {
        set(r, "critique", "rounds", serde_json::json!(2))
    });
    let reason = check_mutation(&world.parent(), &child, &settings).unwrap_err();
    assert!(reason.contains("fixed-core or safety key"), "{reason}");
}

#[test]
fn thresholds_fixed_when_queued() {
    let world = World::new();
    let queued_with = settings();
    let evolution = world.evolution();
    evolution
        .after_run(
            &queued_with,
            &run(1, Some(api(1))),
            Some(&proposal("critique.rounds", "2")),
        )
        .unwrap();
    let candidate = evolution.candidate("api-feature-v2").unwrap().unwrap();
    assert_eq!(candidate.thresholds, thresholds());

    // Raising the bar afterwards doesn't move it for a candidate already queued.
    let mut stricter = settings();
    stricter.evolution.min_gain_points = 50.0;
    stricter.evolution.max_cost_increase = 0.0;
    let results = and_level_holdout(
        tuning(&api(1), &api(2), 6, 0, (10.0, 10.5)),
        &api(1),
        &api(2),
    );
    let actions = evolution
        .after_replay(&stricter, "api-feature-v2", &results, 40.0)
        .unwrap();
    let asks = find(&actions, "ask_promotion");
    assert_eq!(asks.len(), 1, "{actions:?}");
    let Action::AskPromotion { packet, .. } = asks[0] else {
        unreachable!()
    };
    assert_eq!(packet["change_class"], "critique_rounds");
    assert_eq!(packet["last_promoted"], serde_json::Value::Null);
    assert_eq!(packet["thresholds"]["min_gain_points"], 10.0);
    assert_eq!(
        evolution
            .candidate("api-feature-v2")
            .unwrap()
            .unwrap()
            .thresholds,
        thresholds()
    );
}

#[test]
fn promotion_needs_the_paired_bootstrap_gain() {
    let verdict = |results: &[ReplayResult], thresholds: &Thresholds| {
        compare(
            results,
            &api(1),
            &api(2),
            thresholds,
            &mut StdRng::seed_from_u64(7),
        )
    };

    // Six tasks newly solved, none lost: 20 points, clearly above zero.
    let clear = verdict(&tuning(&api(1), &api(2), 6, 0, (10.0, 10.0)), &thresholds());
    assert!(clear.passed, "{clear:?}");
    assert!((clear.gain_points - 20.0).abs() < 1e-9);
    assert!(clear.lower_bound > 0.0);

    // The same average gain of 6.7 points from six wins and four losses is noise.
    let noisy = verdict(&tuning(&api(1), &api(2), 6, 4, (10.0, 10.0)), &thresholds());
    assert!(!noisy.passed);
    assert!(noisy.lower_bound <= 0.0, "{noisy:?}");
    assert!(
        noisy.reasons.iter().any(|r| r.contains("isn't clear")),
        "{noisy:?}"
    );

    // A clear gain that falls short of the bar set in advance.
    let mut higher = thresholds();
    higher.min_gain_points = 15.0;
    let short = verdict(&tuning(&api(1), &api(2), 3, 0, (10.0, 10.0)), &higher);
    assert!(short.lower_bound > 0.0, "{short:?}");
    assert!(!short.passed);
    assert!(
        short.reasons.iter().any(|r| r.contains("short of the 15")),
        "{short:?}"
    );

    // Paired by task: a task only one version ran tells nothing.
    let mut unpaired = tuning(&api(1), &api(2), 6, 0, (10.0, 10.0));
    unpaired.retain(|result| result.recipe == api(1) || result.task.ends_with('0'));
    let thin = verdict(&unpaired, &thresholds());
    assert!(!thin.passed);

    // Seeded, so the same evidence always gives the same verdict.
    let again = verdict(&tuning(&api(1), &api(2), 6, 4, (10.0, 10.0)), &thresholds());
    assert_eq!(again, noisy);
    assert!(verdict(&[], &thresholds()).reasons[0].contains("no task"));
}

#[test]
fn promotion_not_worse_for_listed_classes() {
    let world = World::new();
    let mut settings = settings();
    settings.evolution.not_worse_classes = vec!["checklist_item".to_owned()];
    let evolution = world.evolution();
    let even = |child: u32| {
        and_level_holdout(
            tuning(&api(1), &api(child), 0, 0, (10.0, 10.0)),
            &api(1),
            &api(child),
        )
    };

    let actions = evolution
        .after_run(
            &settings,
            &run(1, Some(api(1))),
            Some(&proposal(
                "critique.checklist",
                r#"["criteria", "testability", "retry_after"]"#,
            )),
        )
        .unwrap();
    assert_eq!(find(&actions, "replay").len(), 1, "{actions:?}");
    assert!(
        evolution
            .candidate("api-feature-v2")
            .unwrap()
            .unwrap()
            .thresholds
            .not_worse
    );
    let actions = evolution
        .after_replay(&settings, "api-feature-v2", &even(2), 30.0)
        .unwrap();
    assert_eq!(find(&actions, "ask_promotion").len(), 1, "{actions:?}");

    // Rounds aren't listed, so the same flat result needs a real gain and fails.
    evolution
        .after_run(
            &settings,
            &run(2, Some(api(1))),
            Some(&proposal("critique.rounds", "2")),
        )
        .unwrap();
    let actions = evolution
        .after_replay(&settings, "api-feature-v3", &even(3), 30.0)
        .unwrap();
    assert!(find(&actions, "ask_promotion").is_empty(), "{actions:?}");
    assert!(matches!(
        state_of(&evolution, "api-feature-v3"),
        CandidateState::Rejected { .. }
    ));

    // "Not worse" still refuses a real loss.
    let mut lenient = thresholds();
    lenient.not_worse = true;
    let losing = tuning(&api(1), &api(2), 0, 6, (10.0, 10.0));
    let verdict = compare(
        &losing,
        &api(1),
        &api(2),
        &lenient,
        &mut StdRng::seed_from_u64(3),
    );
    assert!(!verdict.passed);
    assert!(verdict.reasons[0].contains("may be worse"), "{verdict:?}");
}

#[test]
fn promotion_refuses_cost_over_limit() {
    let mut rng = StdRng::seed_from_u64(11);
    let dear = compare(
        &tuning(&api(1), &api(2), 6, 0, (10.0, 12.5)),
        &api(1),
        &api(2),
        &thresholds(),
        &mut rng,
    );
    assert!(!dear.passed);
    assert!((dear.cost_change - 0.25).abs() < 1e-9);
    assert_eq!(dear.reasons.len(), 1, "{dear:?}");
    assert!(dear.reasons[0].contains("cost rises 25%"), "{dear:?}");

    let fair = compare(
        &tuning(&api(1), &api(2), 6, 0, (10.0, 10.8)),
        &api(1),
        &api(2),
        &thresholds(),
        &mut rng,
    );
    assert!(fair.passed, "{fair:?}");
}

#[test]
fn holdout_regression_rejects_as_overfit() {
    // No holdout result is no check at all, so it never passes for one.
    assert!(!holdout_ok(&[], &api(1), &api(2)));
    assert!(!holdout_ok(
        &tuning(&api(1), &api(2), 6, 0, (10.0, 10.0)),
        &api(1),
        &api(2)
    ));
    let worse = replays(&api(1), &api(2), &[(3, 0), (3, 3)], (10.0, 10.0), true);
    assert!(!holdout_ok(&worse, &api(1), &api(2)));
    let level = replays(&api(1), &api(2), &[(3, 3), (0, 0)], (10.0, 10.0), true);
    assert!(holdout_ok(&level, &api(1), &api(2)));

    let world = World::new();
    let settings = settings();
    let evolution = world.evolution();
    evolution
        .after_run(
            &settings,
            &run(1, Some(api(1))),
            Some(&proposal("critique.rounds", "2")),
        )
        .unwrap();
    let mut results = tuning(&api(1), &api(2), 6, 0, (10.0, 10.0));
    results.extend(worse);
    let actions = evolution
        .after_replay(&settings, "api-feature-v2", &results, 60.0)
        .unwrap();
    assert!(find(&actions, "ask_promotion").is_empty(), "{actions:?}");
    let CandidateState::Rejected { reason } = state_of(&evolution, "api-feature-v2") else {
        panic!("not rejected")
    };
    assert!(
        reason.contains("holdout") && reason.contains("overfit"),
        "{reason}"
    );

    // A replay that stopped before its holdout, however well its tuning went, is rejected for
    // having none.
    let world = World::new();
    let evolution = world.evolution();
    evolution
        .after_run(
            &settings,
            &run(1, Some(api(1))),
            Some(&proposal("critique.rounds", "2")),
        )
        .unwrap();
    let actions = evolution
        .after_replay(
            &settings,
            "api-feature-v2",
            &tuning(&api(1), &api(2), 6, 0, (10.0, 10.0)),
            60.0,
        )
        .unwrap();
    assert!(find(&actions, "ask_promotion").is_empty(), "{actions:?}");
    let CandidateState::Rejected { reason } = state_of(&evolution, "api-feature-v2") else {
        panic!("not rejected")
    };
    assert!(reason.contains("no holdout result"), "{reason}");
}

#[test]
fn canary_tripwires_roll_back() {
    let settings = settings();
    let canary_run = |n: u32| {
        let mut run = run(n, Some(api(2)));
        run.canary = true;
        run
    };
    let baseline: Vec<RunRecord> = (0..20).map(|n| run(100 + n, Some(api(1)))).collect();

    // The tripwires themselves.
    assert!(tripwires(&[canary_run(1)], &baseline, 0, &settings).is_empty());
    let mut escalated = canary_run(1);
    escalated.escalations = 2;
    assert_eq!(
        tripwires(&[escalated.clone()], &baseline, 0, &settings),
        [Tripwire::Escalations]
    );
    let mut failed = canary_run(1);
    failed.final_checks_passed = false;
    assert_eq!(
        tripwires(&[failed], &baseline, 0, &settings),
        [Tripwire::FailedFinalChecks]
    );
    let mut dear = canary_run(1);
    dear.cost_usd = 14.0;
    assert_eq!(
        tripwires(&[dear], &baseline, 0, &settings),
        [Tripwire::Cost]
    );
    let mut abandoned = canary_run(1);
    abandoned.outcome = RunOutcome::Abandoned;
    assert_eq!(
        tripwires(&[abandoned], &baseline, 0, &settings),
        [Tripwire::Rejected]
    );
    assert_eq!(
        tripwires(&[canary_run(1)], &baseline, 1, &settings),
        [Tripwire::Reverted]
    );
    // A parent that escalated often makes one escalation unremarkable.
    let rough: Vec<RunRecord> = baseline
        .iter()
        .cloned()
        .map(|mut run| {
            run.escalations = 1;
            run
        })
        .collect();
    assert!(tripwires(&[escalated.clone()], &rough, 0, &settings).is_empty());

    // In the loop: the parent's runs are in the outcome log, the canary escalates, and the
    // recipe is back on v1 before the next run starts.
    let world = World::new();
    for record in &baseline {
        outcomes::append(&world.store, &record.run, &[LogLine::Run(record.clone())]).unwrap();
    }
    let evolution = awaiting_promotion(&world, &settings);
    evolution
        .decide("api-feature-v2", true, &Party::Rules)
        .unwrap();
    assert_eq!(evolution.running_version("api-feature").unwrap(), Some(2));
    assert_eq!(
        evolution
            .running("api-feature")
            .unwrap()
            .unwrap()
            .recipe
            .version,
        2
    );

    let actions = evolution
        .after_run(&settings, &canary_run(1), None)
        .unwrap();
    assert!(find(&actions, "rolled_back").is_empty(), "{actions:?}");
    assert!(matches!(
        state_of(&evolution, "api-feature-v2"),
        CandidateState::Canary { runs: 1 }
    ));

    let mut bad = canary_run(2);
    bad.escalations = 3;
    let actions = evolution.after_run(&settings, &bad, None).unwrap();
    let rolled = find(&actions, "rolled_back");
    assert_eq!(rolled.len(), 1, "{actions:?}");
    let Action::RolledBack { to, reason, .. } = rolled[0] else {
        unreachable!()
    };
    assert_eq!(*to, 1);
    assert!(reason.contains("escalated"), "{reason}");
    assert_eq!(evolution.running_version("api-feature").unwrap(), Some(1));
    assert!(matches!(
        state_of(&evolution, "api-feature-v2"),
        CandidateState::RolledBack { .. }
    ));

    // A merge you revert trips the next canary too.
    let world = World::new();
    let evolution = awaiting_promotion(&world, &settings);
    evolution
        .decide("api-feature-v2", true, &Party::User)
        .unwrap();
    let first = canary_run(1);
    outcomes::append(
        &world.store,
        &first.run,
        &[LogLine::Reversal(Reversal {
            run: first.run.clone(),
            card: None,
            what: "reverted the merge".to_owned(),
            at: start(),
        })],
    )
    .unwrap();
    let actions = evolution.after_run(&settings, &first, None).unwrap();
    assert_eq!(find(&actions, "rolled_back").len(), 1, "{actions:?}");
    assert_eq!(evolution.running_version("api-feature").unwrap(), Some(1));
}

#[test]
fn canary_that_holds_is_promoted_on_a_branch() {
    let world = World::new();
    let settings = settings();
    let evolution = awaiting_promotion(&world, &settings);
    let head = git::run(world.repo(), &["rev-parse", "HEAD"]).unwrap();
    evolution
        .decide("api-feature-v2", true, &Party::Rules)
        .unwrap();

    let mut actions = Vec::new();
    for n in 1..=settings.evolution.canary_runs {
        let mut canary = run(n, Some(api(2)));
        canary.canary = true;
        actions = evolution.after_run(&settings, &canary, None).unwrap();
    }
    let promoted = find(&actions, "promoted");
    assert_eq!(promoted.len(), 1, "{actions:?}");
    let Action::Promoted {
        recipe,
        branch,
        pr_command,
    } = promoted[0]
    else {
        unreachable!()
    };
    assert_eq!(*recipe, api(2));
    assert_eq!(branch, "argonauts/recipes/api-feature-v2");
    assert_eq!(&pr_command[..3], ["gh", "pr", "create"]);
    assert!(pr_command.contains(branch));
    assert_eq!(
        state_of(&evolution, "api-feature-v2"),
        CandidateState::Current
    );
    assert_eq!(evolution.running_version("api-feature").unwrap(), Some(2));

    // The checkout is untouched; the version waits on its branch.
    assert_eq!(
        git::run(world.repo(), &["rev-parse", "HEAD"]).unwrap(),
        head
    );
    assert_eq!(
        git::run(world.repo(), &["status", "--porcelain"]).unwrap(),
        ""
    );
    let current = git::run(
        world.repo(),
        &[
            "show",
            &format!("{branch}:.argonauts/recipes/api-feature/current"),
        ],
    )
    .unwrap();
    assert_eq!(current, "2");
    let text = git::run(
        world.repo(),
        &[
            "show",
            &format!("{branch}:.argonauts/recipes/api-feature/v2.yaml"),
        ],
    )
    .unwrap();
    let promoted = recipes::parse(Path::new("v2.yaml"), &text).unwrap();
    assert_eq!(promoted.parent, Some(1));
    assert_eq!(promoted.settings["critique"]["rounds"], 2);
    let touched = git::run(world.repo(), &["diff", "--name-only", &head, branch]).unwrap();
    assert_eq!(
        touched, ".argonauts/recipes/api-feature/current\n.argonauts/recipes/api-feature/v2.yaml",
        "files the candidate shares with the checkout stay as they are"
    );

    // The next proposal's evidence packet knows when this recipe last changed.
    evolution
        .after_run(
            &settings,
            &run(50, Some(api(2))),
            Some(&proposal("critique.rounds", "3")),
        )
        .unwrap();
    let actions = evolution
        .after_replay(
            &settings,
            "api-feature-v3",
            &and_level_holdout(tuning(&api(2), &api(3), 6, 0, (9.0, 9.0)), &api(2), &api(3)),
            20.0,
        )
        .unwrap();
    let Action::AskPromotion { packet, .. } = find(&actions, "ask_promotion")[0] else {
        unreachable!()
    };
    let promoted_at = start()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    assert_eq!(packet["last_promoted"], promoted_at.as_str());
}

#[test]
fn rollback_takes_effect_at_once() {
    let world = World::new();
    let folder = world.folder();
    let mut v2 = world.parent().recipe;
    v2.version = 2;
    v2.parent = Some(1);
    v2.settings.insert("critique".to_owned(), serde_json::json!({ "rounds": 2, "critic": "rotate", "checklist": ["criteria", "testability"] }));
    std::fs::write(folder.join("v2.yaml"), recipes::render(&v2).unwrap()).unwrap();
    commit_all(world.repo(), "v2");
    let head = git::run(world.repo(), &["rev-parse", "HEAD"]).unwrap();

    let evolution = world.evolution();
    assert_eq!(evolution.running_version("api-feature").unwrap(), Some(2));
    let actions = evolution
        .rollback("api-feature", "you asked for it")
        .unwrap();
    let rolled = find(&actions, "rolled_back");
    assert_eq!(rolled.len(), 1, "{actions:?}");
    let Action::RolledBack { recipe, to, .. } = rolled[0] else {
        unreachable!()
    };
    assert_eq!((recipe.clone(), *to), (api(2), 1));
    assert_eq!(
        evolution.running_version("api-feature").unwrap(),
        Some(1),
        "at once"
    );
    assert_eq!(
        evolution
            .running("api-feature")
            .unwrap()
            .unwrap()
            .recipe
            .version,
        1
    );

    let branch = "argonauts/recipes/api-feature-rollback-v1";
    let pointer = git::run(
        world.repo(),
        &[
            "show",
            &format!("{branch}:.argonauts/recipes/api-feature/current"),
        ],
    )
    .unwrap();
    assert_eq!(pointer, "1");
    assert_eq!(
        git::run(world.repo(), &["rev-parse", "HEAD"]).unwrap(),
        head
    );
    assert_eq!(
        git::run(world.repo(), &["status", "--porcelain"]).unwrap(),
        ""
    );

    // Once you merge it, the checkout says the same; once you point elsewhere, you win.
    std::fs::write(folder.join("current"), "1\n").unwrap();
    commit_all(world.repo(), "merge the rollback");
    assert_eq!(evolution.running_version("api-feature").unwrap(), Some(1));
    std::fs::write(folder.join("current"), "2\n").unwrap();
    assert_eq!(evolution.running_version("api-feature").unwrap(), Some(2));

    std::fs::write(folder.join("current"), "1\n").unwrap();
    let message = evolution
        .rollback("api-feature", "again")
        .unwrap_err()
        .to_string();
    assert!(message.contains("no version before it"), "{message}");
    assert!(evolution.rollback("nothing-here", "why").is_err());
}

#[test]
fn stages_of_autonomy_table() {
    let rows = [
        (EvolutionStage::Record, ApprovalMode::Auto, Stage::Record),
        (EvolutionStage::Suggest, ApprovalMode::Auto, Stage::Suggest),
        (
            EvolutionStage::Evolve,
            ApprovalMode::Supervised,
            Stage::Evolve,
        ),
        (EvolutionStage::Evolve, ApprovalMode::Shadow, Stage::Evolve),
        (EvolutionStage::Evolve, ApprovalMode::Auto, Stage::Auto),
    ];
    assert_eq!(
        stage(&Settings::default()),
        Stage::Auto,
        "autonomous by default"
    );
    for (evolution_stage, mode, expected) in rows {
        let world = World::new();
        let mut settings = settings();
        settings.evolution.stage = evolution_stage;
        settings.approvals.recipe_promotion.mode = mode;
        assert_eq!(stage(&settings), expected);

        let evolution = world.evolution();
        let actions = evolution
            .after_run(
                &settings,
                &run(1, Some(api(1))),
                Some(&proposal("critique.rounds", "2")),
            )
            .unwrap();
        let candidate = evolution.candidate("api-feature-v2").unwrap();
        match expected {
            Stage::Record => {
                assert!(candidate.is_none(), "record proposes nothing");
                assert!(actions.is_empty(), "{actions:?}");
            }
            Stage::Suggest => {
                assert_eq!(candidate.unwrap().state, CandidateState::Suggested);
                assert!(find(&actions, "replay").is_empty());
                assert_eq!(find(&actions, "tell").len(), 1, "{actions:?}");
            }
            Stage::Evolve | Stage::Auto => {
                assert!(matches!(
                    candidate.unwrap().state,
                    CandidateState::Replaying { .. }
                ));
                assert_eq!(find(&actions, "replay").len(), 1, "{actions:?}");
                let results = and_level_holdout(
                    tuning(&api(1), &api(2), 6, 0, (10.0, 10.0)),
                    &api(1),
                    &api(2),
                );
                let actions = evolution
                    .after_replay(&settings, "api-feature-v2", &results, 10.0)
                    .unwrap();
                // Both ask the checkpoint; your approval settings decide who answers.
                assert_eq!(find(&actions, "ask_promotion").len(), 1, "{actions:?}");
            }
        }
    }

    // A fixed-core proposal never becomes a candidate, at any stage.
    let world = World::new();
    let evolution = world.evolution();
    let actions = evolution
        .after_run(
            &settings(),
            &run(1, Some(api(1))),
            Some(&proposal("judging.method", "\"single\"")),
        )
        .unwrap();
    assert!(evolution.candidates().unwrap().is_empty());
    assert!(find(&actions, "tell").len() == 1, "{actions:?}");
}

#[test]
fn evolution_budget_queues_candidates() {
    let world = World::new();
    let mut settings = settings();
    settings.limits.evolution_usd_per_month = 100.0;
    let evolution = world.evolution();

    let first = evolution
        .after_run(
            &settings,
            &run(1, Some(api(1))),
            Some(&proposal("critique.rounds", "2")),
        )
        .unwrap();
    assert_eq!(find(&first, "replay").len(), 1);
    let second = evolution
        .after_run(
            &settings,
            &run(2, Some(api(1))),
            Some(&proposal("critique.rounds", "3")),
        )
        .unwrap();
    assert!(
        find(&second, "replay").is_empty(),
        "one replay at a time: {second:?}"
    );
    assert_eq!(
        state_of(&evolution, "api-feature-v3"),
        CandidateState::Queued
    );

    let results = and_level_holdout(
        tuning(&api(1), &api(2), 6, 0, (10.0, 10.0)),
        &api(1),
        &api(2),
    );
    let actions = evolution
        .after_replay(&settings, "api-feature-v2", &results, 100.0)
        .unwrap();
    assert!(
        find(&actions, "replay").is_empty(),
        "the month is spent: {actions:?}"
    );
    assert_eq!(evolution.spent_this_month(start()).unwrap(), 100.0);
    assert_eq!(
        state_of(&evolution, "api-feature-v3"),
        CandidateState::Queued
    );
    assert!(evolution.next_replay(&settings).unwrap().is_empty());

    let next_month = Evolution::open(&world.store)
        .unwrap()
        .at(start() + Duration::days(31));
    assert_eq!(
        next_month
            .spent_this_month(start() + Duration::days(31))
            .unwrap(),
        0.0
    );
    let actions = next_month.next_replay(&settings).unwrap();
    assert_eq!(find(&actions, "replay").len(), 1, "{actions:?}");
    assert!(matches!(
        state_of(&next_month, "api-feature-v3"),
        CandidateState::Replaying { .. }
    ));

    // A budget of 0 spends nothing.
    let world = World::new();
    settings.limits.evolution_usd_per_month = 0.0;
    let actions = world
        .evolution()
        .after_run(
            &settings,
            &run(1, Some(api(1))),
            Some(&proposal("critique.rounds", "2")),
        )
        .unwrap();
    assert!(find(&actions, "replay").is_empty());
}

#[test]
fn stage_one_records_what_worked() {
    let world = World::new();
    let mut settings = settings();
    settings.evolution.stage = EvolutionStage::Record;
    let evolution = world.evolution();
    let head = git::run(world.repo(), &["rev-parse", "HEAD"]).unwrap();

    let mut worked = run(1, None);
    worked.task_type = "Data Migration".to_owned();
    let actions = evolution.after_run(&settings, &worked, None).unwrap();
    let recorded = find(&actions, "recorded");
    assert_eq!(recorded.len(), 1, "{actions:?}");
    let Action::Recorded { recipe, branch } = recorded[0] else {
        unreachable!()
    };
    assert_eq!(recipe.name, "data-migration");
    assert_eq!(recipe.version, 1);
    assert_eq!(branch, "argonauts/recipes/data-migration-v1");

    let text = git::run(
        world.repo(),
        &[
            "show",
            &format!("{branch}:.argonauts/recipes/data-migration/v1.yaml"),
        ],
    )
    .unwrap();
    let v1 = recipes::parse(Path::new("v1.yaml"), &text).unwrap();
    assert!(recipes::validate(&v1).is_empty());
    assert_eq!(v1.workflow.as_deref(), Some("review-loop"));
    assert_eq!(v1.matches.task_types, ["Data Migration"]);
    assert_eq!(v1.parent, None);
    assert_eq!(
        git::run(world.repo(), &["rev-parse", "HEAD"]).unwrap(),
        head
    );
    assert!(!world.store.recipes_dir().join("data-migration").exists());

    // Once is enough, and only for runs that merged and passed first time without a recipe.
    assert!(
        evolution
            .after_run(&settings, &worked, None)
            .unwrap()
            .is_empty()
    );
    let mut failed = run(2, None);
    failed.task_type = "frontend".to_owned();
    failed.final_checks_passed = false;
    assert!(
        evolution
            .after_run(&settings, &failed, None)
            .unwrap()
            .is_empty()
    );
    let covered = run(3, None);
    assert!(
        evolution
            .after_run(&settings, &covered, None)
            .unwrap()
            .is_empty(),
        "backend already has a recipe"
    );
}

/// The engine asks the retro for a change only every `propose_every` runs, so the loop takes
/// each proposal that arrives rather than counting runs a second time.
#[test]
fn every_proposal_that_arrives_is_due() {
    let world = World::new();
    let mut settings = settings();
    settings.evolution.propose_every = 3;
    let evolution = world.evolution();
    evolution
        .after_run(
            &settings,
            &run(1, Some(api(1))),
            Some(&proposal("critique.rounds", "2")),
        )
        .unwrap();
    assert!(matches!(
        state_of(&evolution, "api-feature-v2"),
        CandidateState::Replaying { .. }
    ));
}
