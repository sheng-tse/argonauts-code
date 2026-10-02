//! Recipes as files: what they may hold, how they match a task, how two versions differ, and
//! Starlark loops that build workflows.

use std::path::{Path, PathBuf};

use argonauts_core::workflow::{StepKind, Workflow};
use argonauts_learning::Store;
use argonauts_learning::recipes::{
    self, ChangeClass, LoadedRecipe, Recipe, loops, matching, settings_overlay,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/recipes")
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("state"), dir.path().join("repo"));
    (dir, store)
}

/// Copies a fixture recipe folder into the store's checkout.
fn install(store: &Store, name: &str) -> PathBuf {
    let to = store.recipes_dir().join(name);
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(fixtures().join(name)).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, to.join(path.file_name().unwrap())).unwrap();
    }
    to
}

fn write_recipe(folder: &Path, recipe: &Recipe) -> LoadedRecipe {
    std::fs::create_dir_all(folder).unwrap();
    let path = folder.join(recipes::version_file(recipe.version));
    std::fs::write(&path, recipes::render(recipe).unwrap()).unwrap();
    recipes::load_file(&path).unwrap()
}

fn yaml_recipe(text: &str) -> Recipe {
    recipes::parse(Path::new("inline.yaml"), text).unwrap()
}

#[test]
fn recipe_refuses_safety_and_fixed_core_keys() {
    let unsafe_recipe = recipes::load_file(&fixtures().join("unsafe/v1.yaml"))
        .unwrap()
        .recipe;
    let problems = recipes::validate(&unsafe_recipe).join("\n");
    for key in [
        "sandbox.network",
        "budget.run_usd",
        "judging.method",
        "drafting.blind",
        "evolution.min_gain_points",
        "review.assign",
        "gates.run",
        "approvals.merge.mode",
        "prompts.judge",
    ] {
        assert!(
            problems.contains(&format!("`{key}`")),
            "{key} not refused:\n{problems}"
        );
    }
    assert!(problems.contains("fixed core"), "{problems}");
    assert!(problems.contains("safety key"), "{problems}");
    // Keys beside the guarded ones are fine on their own.
    assert!(!problems.contains("budget.per_card_usd"), "{problems}");
    assert!(!problems.contains("drafting.deadline_min"), "{problems}");

    let fine = yaml_recipe(
        "recipe: fine\nversion: 1\ndescription: d\nmatch: { task_types: [backend] }\n\
         drafting: { deadline_min: 10 }\ncritique: { rounds: 2 }\n",
    );
    assert_eq!(recipes::validate(&fine), Vec::<String>::new());

    let (_dir, store) = store();
    install(&store, "unsafe");
    install(&store, "api-feature");
    let sets = recipes::load_all(&store).unwrap();
    let names: Vec<&str> = sets.iter().map(|set| set.name.as_str()).collect();
    assert_eq!(
        names,
        ["api-feature"],
        "a recipe that breaks a rule is left out whole"
    );

    // Files beside the version, by their full paths, so they read the same from any repo.
    let overlay = settings_overlay(sets[0].current().unwrap()).unwrap();
    let beside = |file: &str| {
        store
            .recipes_dir()
            .join("api-feature")
            .join(file)
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(
        overlay["workflow"].as_str(),
        Some(beside("review-loop.yaml").as_str())
    );
    assert_eq!(
        overlay["prompts"]["drafting"].as_str(),
        Some(beside("drafting-v1.md").as_str())
    );
    assert_eq!(overlay["critique"]["rounds"].as_integer(), Some(1));
    assert!(!overlay.contains_key("match") && !overlay.contains_key("recipe"));
}

#[test]
fn recipe_files_hold_no_statistics() {
    let stats = recipes::load_file(&fixtures().join("stats/v1.yaml"))
        .unwrap()
        .recipe;
    let problems = recipes::validate(&stats).join("\n");
    assert!(
        problems.contains("`pass_rate` is a statistic"),
        "{problems}"
    );
    assert!(problems.contains("`stats` is a statistic"), "{problems}");

    let (_dir, store) = store();
    install(&store, "stats");
    assert!(recipes::load_all(&store).unwrap().is_empty());

    // A recipe's file never records what its runs did: the outcome log does.
    let folder = install(&store, "api-feature");
    let before = std::fs::read(folder.join("v1.yaml")).unwrap();
    let evolution = argonauts_learning::evolution::Evolution::open(&store).unwrap();
    let mut settings = argonauts_core::settings::Settings::default();
    settings.evolution.stage = argonauts_core::settings::evolution::EvolutionStage::Record;
    let run = argonauts_learning::RunRecord {
        run: argonauts_core::RunId::at(time::OffsetDateTime::UNIX_EPOCH, 1),
        at: time::OffsetDateTime::UNIX_EPOCH,
        task_type: "backend".to_owned(),
        recipe: Some(argonauts_learning::RecipeRef {
            name: "api-feature".to_owned(),
            version: 1,
        }),
        workflow: None,
        outcome: argonauts_core::events::run::RunOutcome::Merged,
        escalations: 0,
        final_checks_passed: true,
        cost_usd: 4.0,
        minutes: 12.0,
        canary: false,
    };
    evolution.after_run(&settings, &run, None).unwrap();
    assert_eq!(std::fs::read(folder.join("v1.yaml")).unwrap(), before);
    let reloaded = recipes::load_all(&store).unwrap();
    assert_eq!(
        reloaded[0].current().unwrap().sha256,
        argonauts_core::sha256_hex(&before)
    );
}

#[test]
fn recipe_matches_task_type_and_labels() {
    let (_dir, store) = store();
    install(&store, "api-feature");
    let root = store.recipes_dir();
    write_recipe(
        &root.join("backend-general"),
        &yaml_recipe(
            "recipe: backend-general\nversion: 1\ndescription: d\nmatch: { task_types: [backend] }\n",
        ),
    );
    write_recipe(
        &root.join("docs"),
        &yaml_recipe("recipe: docs\nversion: 1\ndescription: d\nmatch: { labels_any: [docs] }\n"),
    );
    let frontend = root.join("frontend");
    write_recipe(
        &frontend,
        &yaml_recipe(
            "recipe: frontend\nversion: 1\ndescription: first\nmatch: { task_types: [frontend] }\n",
        ),
    );
    write_recipe(
        &frontend,
        &yaml_recipe(
            "recipe: frontend\nversion: 2\nparent: 1\ndescription: second\n\
             match: { task_types: [frontend] }\ncritique: { rounds: 2 }\n",
        ),
    );
    std::fs::write(frontend.join("current"), "v1\n").unwrap();

    let sets = recipes::load_all(&store).unwrap();
    let pick = |task_type: &str, labels: &[&str]| {
        let labels: Vec<String> = labels.iter().map(|l| (*l).to_owned()).collect();
        matching(&sets, task_type, &labels).map(|l| (l.recipe.recipe.clone(), l.recipe.version))
    };
    let named = |name: &str, version: u32| Some((name.to_owned(), version));

    assert_eq!(pick("backend", &["api"]), named("api-feature", 1));
    assert_eq!(pick("backend", &["ui"]), named("backend-general", 1));
    assert_eq!(pick("backend", &[]), named("backend-general", 1));
    assert_eq!(pick("research", &["docs", "x"]), named("docs", 1));
    assert_eq!(
        pick("frontend", &[]),
        named("frontend", 1),
        "`current` picks the version"
    );
    assert_eq!(pick("research", &[]), None);

    std::fs::remove_file(frontend.join("current")).unwrap();
    let sets = recipes::load_all(&store).unwrap();
    assert_eq!(
        matching(&sets, "frontend", &[]).map(|l| l.recipe.version),
        Some(2),
        "without `current`, the newest runs"
    );
}

#[test]
fn recipe_diff_is_structural() {
    let (_dir, store) = store();
    let folder = install(&store, "api-feature");
    let parent = recipes::load_file(&folder.join("v1.yaml")).unwrap();

    // Same words under another file name, a Starlark file that builds the same steps, keys in
    // another order, and checklist items reordered: none of it is a change.
    std::fs::copy(
        folder.join("drafting-v1.md"),
        folder.join("drafting-copy.md"),
    )
    .unwrap();
    let same = "recipe: api-feature\nversion: 2\nparent: 1\n\
        crew: [claude-code, codex]\nbudget: { per_card_usd: 2 }\n\
        critique: { checklist: [testability, criteria], critic: rotate, rounds: 1 }\n\
        description: Reworded, which isn't a change\npreset: peer-crew\nmode: crew\n\
        workflow: review-loop.star\nmatch: { labels_any: [api, endpoint], task_types: [backend] }\n\
        prompts: { drafting: drafting-copy.md }\n";
    let child = write_recipe(&folder, &yaml_recipe(same));
    assert_eq!(recipes::diff(&parent, &child).unwrap(), Vec::new());

    // Checklist items count one by one; the rounds and a prompt's words count once each.
    std::fs::write(folder.join("drafting-v3.md"), "Draft it differently.\n").unwrap();
    let mut changed = yaml_recipe(same);
    changed.version = 3;
    changed
        .prompts
        .insert("drafting".to_owned(), "drafting-v3.md".to_owned());
    changed.settings.insert(
        "critique".to_owned(),
        serde_json::json!({ "rounds": 2, "critic": "rotate", "checklist": ["criteria", "response_headers"] }),
    );
    let child = write_recipe(&folder, &changed);
    let changes = recipes::diff(&parent, &child).unwrap();
    let mut classes: Vec<(String, ChangeClass)> =
        changes.iter().map(|c| (c.key.clone(), c.class)).collect();
    classes.sort();
    assert_eq!(
        classes,
        [
            ("critique.checklist".to_owned(), ChangeClass::ChecklistItem),
            ("critique.checklist".to_owned(), ChangeClass::ChecklistItem),
            ("critique.rounds".to_owned(), ChangeClass::CritiqueRounds),
            ("prompts.drafting".to_owned(), ChangeClass::DraftingPrompt),
        ]
    );
    let removed: Vec<_> = changes.iter().filter_map(|c| c.before.clone()).collect();
    assert!(
        removed.contains(&serde_json::json!("testability")),
        "{changes:?}"
    );

    // One step added deep inside the loop is one change, not a shift of what follows it.
    let mut workflow = loops::build_file(&folder.join("review-loop.star")).unwrap();
    let card = &mut workflow.phases[2].steps[0].steps[0].steps;
    let mut third = card[2].clone();
    third.name = Some("review-3".to_owned());
    card.insert(2, third);
    std::fs::write(
        folder.join("review-loop-v4.yaml"),
        serde_saphyr::to_string(&workflow).unwrap(),
    )
    .unwrap();
    let mut stepped = yaml_recipe(same);
    stepped.version = 4;
    stepped
        .prompts
        .insert("drafting".to_owned(), "drafting-v1.md".to_owned());
    stepped.workflow = Some("review-loop-v4.yaml".to_owned());
    let child = write_recipe(&folder, &stepped);
    let changes = recipes::diff(&parent, &child).unwrap();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].class, ChangeClass::StepGraph);
    assert_eq!(changes[0].key, "workflow");
    assert_eq!(changes[0].before, None);
    assert_eq!(changes[0].after.as_ref().unwrap()["name"], "review-3");
}

#[test]
fn starlark_builds_a_workflow() {
    let star = fixtures().join("api-feature/review-loop.star");
    let built = loops::build_file(&star).unwrap();
    let expected: Workflow = serde_saphyr::from_str(
        &std::fs::read_to_string(fixtures().join("api-feature/review-loop.yaml")).unwrap(),
    )
    .unwrap();
    assert_eq!(built, expected);
    assert_eq!(
        loops::build_file(&star).unwrap(),
        built,
        "the same file builds the same workflow"
    );

    let card = &built.phases[2].steps[0].steps[0];
    assert_eq!(card.kind, StepKind::LoopUntil);
    assert_eq!(card.max, Some(3));
    let reviews: Vec<_> = card
        .steps
        .iter()
        .filter(|step| step.kind == StepKind::Review)
        .map(|step| step.name.clone().unwrap())
        .collect();
    assert_eq!(reviews, ["review-1", "review-2"]);

    let source = r#"
order = {}
for name in ["zeta", "alpha", "mid"]:
    order[name] = len(order)
workflow(name = ",".join(order.keys()), phases = [phase("Do", "do", [build(role = "owner")])])
"#;
    for _ in 0..3 {
        assert_eq!(
            loops::build("order.star", source).unwrap().name,
            "zeta,alpha,mid"
        );
    }
}

#[test]
fn starlark_has_no_io_and_a_step_limit() {
    let refused = |source: &str| match loops::build("io.star", source) {
        Ok(workflow) => panic!("built {workflow:?} from {source:?}"),
        Err(error) => error.to_string(),
    };
    let ok = "workflow(name = \"w\", phases = [])\n";
    assert!(loops::build("ok.star", ok).is_ok());

    assert!(!refused(&format!("load(\"other.star\", \"x\")\n{ok}")).is_empty());
    for call in [
        "open(\"/etc/passwd\")",
        "read_file(\"x\")",
        "http_get(\"https://example.com\")",
        "time()",
        "now()",
        "random()",
        "print(\"hi\")",
    ] {
        let message = refused(&format!("{call}\n{ok}"));
        assert!(message.contains("isn't defined"), "{call}: {message}");
    }

    let message = refused(&format!(
        "for i in range(1000):\n    for j in range(1000):\n        pass\n{ok}"
    ));
    assert!(message.contains("step limit"), "{message}");
    let message = refused(&format!("for i in range(1000000000):\n    pass\n{ok}"));
    assert!(message.contains("step limit"), "{message}");
    let message = refused(&format!("def f(n):\n    return f(n - 1)\nf(3)\n{ok}"));
    assert!(message.contains("recursion"), "{message}");
    let message = refused(&format!("big = [0] * 1000000\n{ok}"));
    assert!(message.contains("limit"), "{message}");
    let message = refused(&format!("x = []\nx.append(x)\n{ok}"));
    assert!(message.contains("itself"), "{message}");
}
