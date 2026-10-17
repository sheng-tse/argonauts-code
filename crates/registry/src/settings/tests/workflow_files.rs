use std::collections::BTreeMap;
use std::fs;

use argonauts_core::names::RoleName;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::fallback::FallbackEntry;
use toml::{Table, Value};

use super::{Repo, invalid, layer_of};
use crate::resolve::leaves;
use crate::workflows::{BASE_WORKFLOW, DEFAULT_WORKFLOW};
use crate::{
    SettingsError, SettingsSources, SettingsWarning, resolve, workflow_choices, workflow_choices_in,
};

/// What each built-in sets on top of the defaults.
const CONTRACT: &[(&str, &[(&str, &str)])] = &[
    ("council", &[]),
    (
        "orchestrator",
        &[
            ("drafting.by", "\"orchestrator\""),
            ("judging.method", "\"orchestrator\""),
            ("routing.algorithm", "\"orchestrator\""),
            ("review.assign", "\"orchestrator\""),
            ("review.per_task", "2"),
            ("review.blind", "true"),
        ],
    ),
    ("captain", &[("captain_or_crew.force", "\"captain\"")]),
    (
        "maker-checker",
        &[
            ("captain_or_crew.force", "\"captain\""),
            ("captain.review", "true"),
        ],
    ),
    (
        "best-of-n",
        &[
            ("captain_or_crew.force", "\"captain\""),
            ("captain.tries", "3"),
        ],
    ),
    (
        "draft-and-vote",
        &[
            ("judging.method", "\"vote\""),
            ("critique.rounds", "0"),
            ("experiments.enabled", "false"),
            ("divide.single_worker", "true"),
        ],
    ),
    ("ablate-no-blind", &[("drafting.blind", "false")]),
    ("ablate-no-anonymize", &[("judging.anonymize", "false")]),
    ("ablate-no-critique", &[("critique.rounds", "0")]),
    ("ablate-fixed-roles", &[("routing.algorithm", "\"fixed\"")]),
    (
        "ablate-same-provider",
        &[("review.other_provider", "false")],
    ),
    ("ablate-no-experiments", &[("experiments.enabled", "false")]),
];

fn value(text: &str) -> Value {
    format!("v = {text}").parse::<Table>().unwrap()["v"].clone()
}

#[test]
fn every_workflow_resolves() {
    let names: Vec<String> = workflow_choices()
        .into_iter()
        .map(|chosen| chosen.name)
        .collect();
    let contract: Vec<&str> = CONTRACT.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, contract);
    for (name, expected) in CONTRACT {
        let resolved = resolve(&SettingsSources {
            overrides: vec![format!("workflow={name}")],
            ..SettingsSources::default()
        })
        .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(resolved.settings.workflow, *name);
        assert!(
            resolved.warnings.is_empty(),
            "{name}: {:?}",
            resolved.warnings
        );
        let set: BTreeMap<String, Value> = resolved
            .entries
            .iter()
            .filter(|entry| entry.layer == "workflow")
            .map(|entry| (entry.key.clone(), value(&entry.value)))
            .collect();
        let contract: BTreeMap<String, Value> = expected
            .iter()
            .map(|(key, text)| ((*key).to_owned(), value(text)))
            .collect();
        assert_eq!(set, contract, "{name} against the contract");
        let mut file: Table = argonauts_core::workflow_files::built_in(name)
            .unwrap()
            .parse()
            .unwrap();
        for key in argonauts_core::settings::workflows::WORKFLOW_FILE_KEYS {
            file.remove(key);
        }
        let file: BTreeMap<String, Value> = leaves(&file).into_iter().collect();
        assert_eq!(set, file, "{name} against its file");
    }
}

#[test]
fn captain_runs_when_nothing_names_a_workflow_on_councils_settings() {
    assert_eq!(DEFAULT_WORKFLOW, "captain");
    assert_eq!(BASE_WORKFLOW, "council");
    assert_eq!(Settings::default().workflow, BASE_WORKFLOW);
    let resolved = resolve(&SettingsSources::default()).unwrap();
    assert_eq!(resolved.settings.workflow, "captain");
    let from_the_workflow: Vec<&str> = resolved
        .entries
        .iter()
        .filter(|entry| entry.layer == "workflow")
        .map(|entry| entry.key.as_str())
        .collect();
    assert_eq!(from_the_workflow, ["captain_or_crew.force"]);
    // Council's file sets no key, so picking it gives the built-in settings.
    let council = resolve(&SettingsSources {
        overrides: vec!["workflow=council".into()],
        ..SettingsSources::default()
    })
    .unwrap();
    assert_eq!(council.settings, Settings::default());
}

#[test]
fn the_default_nobody_picked_gives_way_to_every_settings_file() {
    let repo = Repo::new();
    repo.global("[captain_or_crew]\nforce = \"crew\"\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.workflow, "captain");
    assert_eq!(
        resolved.settings.captain_or_crew.force,
        Some(argonauts_core::settings::protocol::ForcedMode::Crew)
    );
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
}

#[test]
fn workflow_file_overrides_builtin() {
    let repo = Repo::new();
    repo.project("workflow = \"captain\"\n");
    let built_in = resolve(&repo.sources()).unwrap();
    assert!(built_in.settings.captain_or_crew.force.is_some());

    // The file replaces the built-in whole: it no longer forces Captain.
    let path = repo.global_workflow("captain", "[critique]\nrounds = 3\n");
    let edited = resolve(&repo.sources()).unwrap();
    assert_eq!(edited.settings.critique.rounds, 3);
    assert!(edited.settings.captain_or_crew.force.is_none());
    assert_eq!(
        layer_of(&edited, "critique.rounds").as_deref(),
        Some("workflow")
    );

    fs::remove_file(&path).unwrap();
    let back = resolve(&repo.sources()).unwrap();
    assert_eq!(back.settings, built_in.settings);

    // The default workflow reads its file the same way.
    repo.project("");
    let path = repo.global_workflow(DEFAULT_WORKFLOW, "[judging]\nanonymize = false\n");
    assert!(!resolve(&repo.sources()).unwrap().settings.judging.anonymize);
    fs::remove_file(path).unwrap();
    assert!(resolve(&repo.sources()).unwrap().settings.judging.anonymize);
}

/// A project's workflow file that waits for your trust sets nothing: the one below it stands.
#[test]
fn a_project_workflow_waiting_for_trust_is_passed_over() {
    let repo = Repo::new();
    repo.global("workflow = \"team\"\n");
    repo.global_workflow("team", "[critique]\nrounds = 2\n");
    let path = repo.project_workflow("team", "[critique]\nrounds = 4\n");
    let mut sources = repo.sources();
    sources.waiting = vec![path];
    assert_eq!(resolve(&sources).unwrap().settings.critique.rounds, 2);
}

#[test]
fn project_workflow_overrides_global() {
    let repo = Repo::new();
    repo.global("workflow = \"team\"\n");
    repo.global_workflow("team", "[critique]\nrounds = 2\n[divide]\nmax_tasks = 9\n");
    let global = resolve(&repo.sources()).unwrap();
    assert_eq!(global.settings.critique.rounds, 2);
    assert_eq!(global.settings.divide.max_tasks, 9);

    let path = repo.project_workflow("team", "[critique]\nrounds = 4\n");
    let project = resolve(&repo.sources()).unwrap();
    assert_eq!(project.settings.critique.rounds, 4);
    assert_eq!(
        project.settings.divide.max_tasks,
        Settings::default().divide.max_tasks,
        "the project file replaces the global one whole"
    );
    fs::remove_file(path).unwrap();
    assert_eq!(
        resolve(&repo.sources()).unwrap().settings.critique.rounds,
        2
    );
}

#[test]
fn your_own_workflow_listed_and_resolved() {
    let repo = Repo::new();
    repo.project_workflow(
        "my-team",
        "[review]\nper_task = 2\n[roles.judge]\nblind = true\n",
    );
    let listed = workflow_choices_in(
        &repo.home().join("workflows"),
        Some(&repo.dot().join("workflows")),
    );
    let mine = listed
        .iter()
        .find(|chosen| chosen.name == "my-team")
        .expect("my-team is listed");
    assert!(mine.source.ends_with("my-team.toml"), "{}", mine.source);

    repo.project("workflow = \"my-team\"\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.workflow, "my-team");
    assert_eq!(resolved.settings.review.per_task, 2);
    let judge = &resolved.settings.roles[&RoleName::Judge];
    assert_eq!(judge.blind, Some(true));
    assert_eq!(judge.fresh_session, Some(true));
    assert_eq!(
        layer_of(&resolved, "review.per_task").as_deref(),
        Some("workflow")
    );
    assert_eq!(layer_of(&resolved, "workflow").as_deref(), Some("project"));

    // A name nobody has names where it was set, and what you can pick.
    repo.project("workflow = \"nobody\"\n");
    let (layer, message) = invalid(resolve(&repo.sources()));
    assert_eq!(layer, repo.project_file());
    assert!(message.contains("\"nobody\""), "{message}");
    assert!(
        message.contains("council") && message.contains("my-team"),
        "{message}"
    );
    let (layer, _) = invalid(resolve(&repo.with_flags(&["workflow=../../etc/passwd"])));
    assert_eq!(layer, "--set workflow=../../etc/passwd");
}

/// A workflow may be its phases alone. It is picked by name like any other and sets nothing,
/// and one named after another workflow leaves that workflow's settings in place.
#[test]
fn a_file_of_phases_is_a_workflow_the_one_key_picks() {
    let repo = Repo::new();
    let workflows = repo.dot().join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    let phases = "# Two looks before the merge\nname: two-looks\nphases: []\n";
    std::fs::write(workflows.join("two-looks.yaml"), phases).unwrap();
    std::fs::write(workflows.join("orchestrator.yaml"), phases).unwrap();

    let listed = workflow_choices_in(&repo.home().join("workflows"), Some(&workflows));
    let mine = listed
        .iter()
        .find(|chosen| chosen.name == "two-looks")
        .expect("two-looks is listed");
    assert!(mine.source.ends_with("two-looks.yaml"), "{}", mine.source);
    assert_eq!(mine.description, "Two looks before the merge");
    assert!(mine.keys.is_empty(), "{:?}", mine.keys);
    let built_in = listed
        .iter()
        .find(|chosen| chosen.name == "orchestrator")
        .expect("orchestrator is listed");
    assert_eq!(built_in.source, "built-in");

    repo.project("workflow = \"two-looks\"\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.workflow, "two-looks");
    assert_eq!(resolved.settings.review, Settings::default().review);

    repo.project("workflow = \"orchestrator\"\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.review.per_task, 2);
}

#[test]
fn repo_workflow_leaves_your_keys_alone() {
    let repo = Repo::new();
    repo.local("crew.main = \"codex\"\n[roles.reviewer]\nmodels = [\"codex\"]\n");
    let text = "crew.main = \"opencode\"\n[agents.codex]\nenabled = false\n\
                [roles.reviewer]\nmodels = [\"opencode\"]\nblind = false\n\
                [critique]\nrounds = 3\n";
    let path = repo.project_workflow(DEFAULT_WORKFLOW, text);
    let file = path.display().to_string();
    let resolved = resolve(&repo.sources()).unwrap();
    let settings = &resolved.settings;
    assert_eq!(settings.crew.main.agent, "codex");
    assert_eq!(
        layer_of(&resolved, "crew.main").as_deref(),
        Some("project-local")
    );
    assert_eq!(settings.roles[&RoleName::Reviewer].models[0].agent, "codex");
    assert!(settings.agents["codex"].enabled);
    assert_eq!(settings.critique.rounds, 3, "protocol keys still apply");
    assert_eq!(settings.roles[&RoleName::Reviewer].blind, Some(false));

    let warned = |key: &str| {
        resolved
            .warnings
            .iter()
            .any(|warning| warning.starts_with(key) && warning.contains(&file))
    };
    for key in ["crew.main", "agents.codex.enabled", "roles.reviewer.models"] {
        assert!(warned(key), "{key}: {:?}", resolved.warnings);
    }
    assert!(
        warned(&format!("the {DEFAULT_WORKFLOW} workflow comes from")),
        "shadowing a built-in says so: {:?}",
        resolved.warnings
    );
    assert_eq!(resolved.warnings.len(), 4, "{:?}", resolved.warnings);

    // The same file in your own workflows folder is yours. While nothing names it, it is the
    // default, which gives way to every settings file, so it hides none of their keys.
    fs::remove_file(path).unwrap();
    repo.global_workflow(DEFAULT_WORKFLOW, text);
    let resolved = resolve(&repo.sources()).unwrap();
    let hidden: Vec<&str> = resolved
        .warnings
        .iter()
        .filter(|warning| warning.contains("has no effect"))
        .filter_map(|warning| warning.split(' ').next())
        .collect();
    assert!(hidden.is_empty(), "{:?}", resolved.warnings);
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    assert_eq!(resolved.settings.crew.main.agent, "codex");
    assert_eq!(resolved.settings.critique.rounds, 3);
    assert!(!resolved.settings.agents["codex"].enabled);
}

#[test]
fn safety_keys_rejected_from_workflows() {
    let repo = Repo::new();
    repo.project("workflow = \"loose\"\n");
    for text in [
        "[sandbox]\nnetwork = true\n",
        "[approvals.merge]\nmode = \"auto\"\n",
        "[agents.codex]\nextra_args = [\"--yolo\"]\n",
        "[limits]\nspend_per_run = 100.0\n",
        "[checks]\nrun = []\n",
        "[notifications.conversation]\ncommand = [\"/bin/echo\", \"{text}\"]\n",
        "[fallback]\nallow_api_billing = true\n",
    ] {
        let path = repo.project_workflow("loose", text);
        let err = resolve(&repo.trusted()).unwrap_err();
        let SettingsError::Refused(message) = &err else {
            panic!("{text}: {err}")
        };
        let key = leaves(&text.parse::<Table>().unwrap()).remove(0).0;
        assert!(message.contains(&key), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
    }

    // A fallback entry's billing counts only from your own settings.
    let path = repo.project_workflow(
        "loose",
        "[fallback.worker]\norder = [{ model = \"codex\", allow_api_billing = true }]\n",
    );
    let resolved = resolve(&repo.trusted()).unwrap();
    let entry = &resolved.settings.fallback.roles[&CrewRole::Worker].order[0];
    assert!(matches!(entry, FallbackEntry::WithRules(rules) if !rules.allow_api_billing));
    assert!(
        resolved.warnings.iter().any(|warning| warning
            .contains("fallback.worker.order[0].allow_api_billing")
            && warning.contains(&path.display().to_string())),
        "{:?}",
        resolved.warnings
    );

    // A workflow can't pick another workflow.
    repo.project_workflow("loose", "workflow = \"captain\"\n[critique]\nrounds = 0\n");
    let resolved = resolve(&repo.trusted()).unwrap();
    assert_eq!(resolved.settings.workflow, "loose");
    assert!(resolved.settings.captain_or_crew.force.is_none());
    assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
}

#[test]
fn entry_billing_needs_a_trusted_project_file() {
    let repo = Repo::new();
    repo.project("[fallback]\norder = [{ model = \"codex/<model>\", allow_api_billing = true }]\n");
    let billing = |sources: &SettingsSources| {
        let resolved = resolve(sources).unwrap();
        let entry = resolved.settings.fallback.order[0].clone();
        let FallbackEntry::WithRules(rules) = entry else {
            panic!("{entry:?}")
        };
        (rules.allow_api_billing, resolved.warnings)
    };
    let (allowed, warnings) = billing(&repo.sources());
    assert!(!allowed);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("trust"), "{warnings:?}");
    let (allowed, warnings) = billing(&repo.trusted());
    assert!(allowed);
    assert!(warnings.is_empty(), "{warnings:?}");
    // Your global file is always yours.
    repo.project("");
    repo.global("[fallback]\norder = [{ model = \"codex/<model>\", allow_api_billing = true }]\n");
    assert!(billing(&repo.sources()).0);
}

#[test]
fn the_file_that_picks_a_workflow_keeps_its_own_values() {
    let repo = Repo::new();
    repo.global("workflow = \"orchestrator\"\n[review]\nper_task = 1\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.review.per_task, 1);
    assert_eq!(
        layer_of(&resolved, "review.per_task").as_deref(),
        Some("global")
    );
    // The rest of the workflow still applies.
    assert!(resolved.settings.review.blind);
    assert_eq!(
        layer_of(&resolved, "review.blind").as_deref(),
        Some("workflow")
    );
    assert!(
        !resolved
            .warnings
            .iter()
            .any(|w| w.contains("has no effect")),
        "{:?}",
        resolved.warnings
    );

    // A later file wins too.
    repo.project("[review]\nper_task = 3\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.review.per_task, 3);
    assert_eq!(
        layer_of(&resolved, "review.per_task").as_deref(),
        Some("project")
    );

    // Picked in the project, the workflow sits above your global file, and says so.
    repo.global("[review]\nper_task = 1\n");
    repo.project("workflow = \"orchestrator\"\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.review.per_task, 2);
    assert_eq!(
        layer_of(&resolved, "review.per_task").as_deref(),
        Some("workflow")
    );
    let hidden: Vec<&SettingsWarning> = resolved
        .warnings
        .iter()
        .filter(|w| w.starts_with("review.per_task") && w.contains("has no effect"))
        .collect();
    assert_eq!(hidden.len(), 1, "{:?}", resolved.warnings);
    assert!(hidden[0].contains("is picked in"), "{}", hidden[0]);

    // Your own project file is later than the shared one, so it wins as well.
    repo.local("[review]\nper_task = 4\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.review.per_task, 4);
}
