use std::collections::BTreeMap;
use std::fs;

use argonauts_core::names::RoleName;
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::{CrewRole, FallbackEntry};
use toml::{Table, Value};

use super::{Repo, invalid, layer_of};
use crate::presets::DEFAULT_PRESET;
use crate::resolve::leaves;
use crate::{SettingsError, SettingsSources, presets, presets_in, resolve};

/// What each built-in sets on top of the defaults, plus the `workflow` each protocol preset
/// starts from.
const CONTRACT: &[(&str, &[(&str, &str)])] = &[
    ("council", &[]),
    (
        "orchestrator",
        &[
            ("phases", "\"orchestrator\""),
            ("drafting.by", "\"orchestrator\""),
            ("judging.method", "\"orchestrator\""),
            ("routing.algorithm", "\"orchestrator\""),
            ("review.assign", "\"orchestrator\""),
            ("review.per_task", "2"),
            ("review.blind", "true"),
            ("roles.adjudicator.when", "\"reviewers_disagree\""),
            ("roles.adjudicator.fresh_session", "true"),
        ],
    ),
    (
        "solo",
        &[("phases", "\"solo\""), ("solo_or_crew.force", "\"solo\"")],
    ),
    (
        "maker-checker",
        &[
            ("phases", "\"maker-checker\""),
            ("solo_or_crew.force", "\"solo\""),
            ("solo.review", "true"),
        ],
    ),
    (
        "best-of-n",
        &[
            ("phases", "\"best-of-n\""),
            ("solo_or_crew.force", "\"solo\""),
            ("solo.attempts", "3"),
        ],
    ),
    (
        "draft-and-vote",
        &[
            ("phases", "\"draft-and-vote\""),
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
fn every_preset_resolves() {
    let names: Vec<String> = presets().into_iter().map(|preset| preset.name).collect();
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
        let path = format!("{}/workflows/{name}.toml", env!("CARGO_MANIFEST_DIR"));
        let file: Table = fs::read_to_string(&path).unwrap().parse().unwrap();
        let file: BTreeMap<String, Value> = leaves(&file).into_iter().collect();
        assert_eq!(set, file, "{name} against {path}");
    }
}

#[test]
fn default_preset_is_peer_crew() {
    assert_eq!(DEFAULT_PRESET, "council");
    assert_eq!(Settings::default().workflow, DEFAULT_PRESET);
    let resolved = resolve(&SettingsSources::default()).unwrap();
    assert_eq!(resolved.settings.workflow, "council");
    assert_eq!(resolved.settings.phases.as_str(), "council");
    assert!(
        resolved
            .entries
            .iter()
            .all(|entry| entry.layer != "workflow")
    );
    let peer_crew = resolve(&SettingsSources {
        overrides: vec!["workflow=council".into()],
        ..SettingsSources::default()
    })
    .unwrap();
    assert_eq!(peer_crew.settings, resolved.settings);
    assert_eq!(peer_crew.sha256, resolved.sha256);
}

#[test]
fn preset_file_overrides_builtin() {
    let repo = Repo::new();
    repo.project("workflow = \"solo\"\n");
    let built_in = resolve(&repo.sources()).unwrap();
    assert!(built_in.settings.triage.force.is_some());

    // The file replaces the built-in whole: it no longer forces solo.
    let path = repo.global_preset("solo", "[critique]\nrounds = 3\n");
    let edited = resolve(&repo.sources()).unwrap();
    assert_eq!(edited.settings.critique.rounds, 3);
    assert!(edited.settings.triage.force.is_none());
    assert_eq!(
        layer_of(&edited, "critique.rounds").as_deref(),
        Some("workflow")
    );

    fs::remove_file(&path).unwrap();
    let back = resolve(&repo.sources()).unwrap();
    assert_eq!(back.settings, built_in.settings);

    // The default preset reads its file the same way.
    repo.project("");
    let path = repo.global_preset("council", "[judging]\nanonymize = false\n");
    assert!(!resolve(&repo.sources()).unwrap().settings.judging.anonymize);
    fs::remove_file(path).unwrap();
    assert!(resolve(&repo.sources()).unwrap().settings.judging.anonymize);
}

/// A project's workflow file that waits for your trust sets nothing: the one below it stands.
#[test]
fn a_project_preset_waiting_for_trust_is_passed_over() {
    let repo = Repo::new();
    repo.global("workflow = \"team\"\n");
    repo.global_preset("team", "[critique]\nrounds = 2\n");
    let path = repo.project_preset("team", "[critique]\nrounds = 4\n");
    let mut sources = repo.sources();
    sources.waiting = vec![path];
    assert_eq!(resolve(&sources).unwrap().settings.critique.rounds, 2);
}

#[test]
fn project_preset_overrides_global() {
    let repo = Repo::new();
    repo.global("workflow = \"team\"\n");
    repo.global_preset("team", "[critique]\nrounds = 2\n[divide]\nmax_tasks = 9\n");
    let global = resolve(&repo.sources()).unwrap();
    assert_eq!(global.settings.critique.rounds, 2);
    assert_eq!(global.settings.divide.max_tasks, 9);

    let path = repo.project_preset("team", "[critique]\nrounds = 4\n");
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
fn your_own_preset_listed_and_resolved() {
    let repo = Repo::new();
    repo.project_preset(
        "my-team",
        "[review]\nper_task = 2\n[roles.adjudicator]\nblind = true\n",
    );
    let listed = presets_in(
        &repo.home().join("workflows"),
        Some(&repo.dot().join("workflows")),
    );
    let mine = listed
        .iter()
        .find(|preset| preset.name == "my-team")
        .expect("my-team is listed");
    assert!(mine.source.ends_with("my-team.toml"), "{}", mine.source);

    repo.project("workflow = \"my-team\"\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.workflow, "my-team");
    assert_eq!(resolved.settings.review.per_task, 2);
    let adjudicator = &resolved.settings.roles[&RoleName::Adjudicator];
    assert_eq!(adjudicator.blind, Some(true));
    assert_eq!(adjudicator.fresh_session, Some(true));
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

#[test]
fn repo_preset_leaves_your_keys_alone() {
    let repo = Repo::new();
    repo.local("main_agent = \"codex\"\n[roles.reviewer]\nworkers = [\"codex\"]\n");
    let text = "main_agent = \"opencode\"\n[agents.codex]\nenabled = false\n\
                [roles.reviewer]\nworkers = [\"opencode\"]\nblind = false\n\
                [critique]\nrounds = 3\n";
    let path = repo.project_preset(DEFAULT_PRESET, text);
    let file = path.display().to_string();
    let resolved = resolve(&repo.sources()).unwrap();
    let settings = &resolved.settings;
    assert_eq!(settings.main_agent.worker, "codex");
    assert_eq!(
        layer_of(&resolved, "main_agent").as_deref(),
        Some("project-local")
    );
    assert_eq!(
        settings.roles[&RoleName::Reviewer].workers[0].worker,
        "codex"
    );
    assert!(settings.agents["codex"].enabled);
    assert_eq!(settings.critique.rounds, 3, "protocol keys still apply");
    assert_eq!(settings.roles[&RoleName::Reviewer].blind, Some(false));

    let warned = |key: &str| {
        resolved
            .warnings
            .iter()
            .any(|warning| warning.starts_with(key) && warning.contains(&file))
    };
    for key in [
        "main_agent",
        "agents.codex.enabled",
        "roles.reviewer.workers",
    ] {
        assert!(warned(key), "{key}: {:?}", resolved.warnings);
    }
    assert!(
        warned(&format!("the {DEFAULT_PRESET} workflow comes from")),
        "shadowing a built-in says so: {:?}",
        resolved.warnings
    );
    assert_eq!(resolved.warnings.len(), 4, "{:?}", resolved.warnings);

    // The same file in your own presets folder is yours, and applies whole. What's left to
    // say is that it hides the same keys in config.local.toml.
    fs::remove_file(path).unwrap();
    repo.global_preset(DEFAULT_PRESET, text);
    let resolved = resolve(&repo.sources()).unwrap();
    let mut hidden: Vec<&str> = resolved
        .warnings
        .iter()
        .filter(|warning| warning.contains("has no effect"))
        .filter_map(|warning| warning.split(' ').next())
        .collect();
    hidden.sort_unstable();
    assert_eq!(
        hidden,
        ["main_agent", "roles.reviewer.workers"],
        "{:?}",
        resolved.warnings
    );
    assert_eq!(resolved.warnings.len(), 2, "{:?}", resolved.warnings);
    assert_eq!(resolved.settings.main_agent.worker, "opencode");
    assert!(!resolved.settings.agents["codex"].enabled);
}

#[test]
fn safety_keys_rejected_from_presets() {
    let repo = Repo::new();
    repo.project("workflow = \"loose\"\n");
    for text in [
        "[sandbox]\nnetwork = true\n",
        "[approvals.merge]\nmode = \"auto\"\n",
        "[agents.codex]\nextra_args = [\"--yolo\"]\n",
        "[limits]\nspend_per_run = 100.0\n",
        "[checks]\nrun = []\n",
        "[notifications.chat]\ncommand = [\"/bin/echo\", \"{text}\"]\n",
        "[fallback]\nallow_api_billing = true\n",
    ] {
        let path = repo.project_preset("loose", text);
        let err = resolve(&repo.trusted()).unwrap_err();
        let SettingsError::Refused(message) = &err else {
            panic!("{text}: {err}")
        };
        let key = leaves(&text.parse::<Table>().unwrap()).remove(0).0;
        assert!(message.contains(&key), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
    }

    // A fallback entry's billing counts only from your own settings.
    let path = repo.project_preset(
        "loose",
        "[fallback.workers]\norder = [{ worker = \"codex\", allow_api_billing = true }]\n",
    );
    let resolved = resolve(&repo.trusted()).unwrap();
    let entry = &resolved.settings.fallback.roles[&CrewRole::Workers].order[0];
    assert!(matches!(entry, FallbackEntry::WithRules(rules) if !rules.allow_api_billing));
    assert!(
        resolved.warnings.iter().any(|warning| warning
            .contains("fallback.workers.order[0].allow_api_billing")
            && warning.contains(&path.display().to_string())),
        "{:?}",
        resolved.warnings
    );

    // A preset can't pick another preset.
    repo.project_preset("loose", "workflow = \"solo\"\n[critique]\nrounds = 0\n");
    let resolved = resolve(&repo.trusted()).unwrap();
    assert_eq!(resolved.settings.workflow, "loose");
    assert!(resolved.settings.triage.force.is_none());
    assert_eq!(resolved.warnings.len(), 1, "{:?}", resolved.warnings);
}

#[test]
fn entry_billing_needs_a_trusted_project_file() {
    let repo = Repo::new();
    repo.project(
        "[fallback]\norder = [{ worker = \"codex/<model>\", allow_api_billing = true }]\n",
    );
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
    repo.global("[fallback]\norder = [{ worker = \"codex/<model>\", allow_api_billing = true }]\n");
    assert!(billing(&repo.sources()).0);
}

#[test]
fn the_file_that_picks_a_preset_keeps_its_own_values() {
    let repo = Repo::new();
    repo.global("workflow = \"orchestrator\"\n[review]\nper_task = 1\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.review.per_task, 1);
    assert_eq!(
        layer_of(&resolved, "review.per_task").as_deref(),
        Some("global")
    );
    // The rest of the preset still applies.
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

    // Picked in the project, the preset sits above your global file, and says so.
    repo.global("[review]\nper_task = 1\n");
    repo.project("workflow = \"orchestrator\"\n");
    let resolved = resolve(&repo.sources()).unwrap();
    assert_eq!(resolved.settings.review.per_task, 2);
    assert_eq!(
        layer_of(&resolved, "review.per_task").as_deref(),
        Some("workflow")
    );
    let hidden: Vec<&String> = resolved
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
