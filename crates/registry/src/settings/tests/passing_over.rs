//! As the app reads the files at start: a key that can't be used is left out with a warning
//! that names it, and the rest of the file still applies.

use argonauts_core::names::RoleName;

use super::*;
use crate::{SettingsWarning, resolve};

fn warned(resolved: &ResolvedSettings, wanted: &str) {
    assert!(
        resolved
            .warnings
            .iter()
            .any(|warning| warning.contains(wanted)),
        "{wanted}: {:?}",
        resolved.warnings
    );
}

#[test]
fn a_role_with_no_such_name_leaves_the_other_roles_in_place() {
    let repo = Repo::new();
    repo.global(
        "[roles.foreman]\nmodels = [\"codex\"]\n\
         [roles.orchestrator]\nmodels = [\"claude-code\"]\n",
    );
    let resolved = resolve(&repo.forgiving()).unwrap();
    let orchestrator = &resolved.settings.roles[&RoleName::Orchestrator];
    assert_eq!(orchestrator.models.len(), 1);
    warned(&resolved, "\"foreman\"");
    warned(&resolved, "so the default is used");
    assert!(
        resolve(&repo.trusted()).is_err(),
        "a check still refuses it"
    );
}

/// A role and a value nobody knows, in one file: each is passed over with a notice, and the
/// rest of the file applies.
#[test]
fn an_unknown_role_and_value_are_each_passed_over_with_a_notice() {
    let repo = Repo::new();
    repo.global(
        "[roles.navigator]\nmodels = [\"codex\"]\n\
         [captain_or_crew]\notherwise = \"crew_then_captain\"\n\
         [crew]\nmain = \"codex\"\n",
    );
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert_eq!(resolved.settings.crew.main.agent, "codex");
    assert!(
        !resolved
            .settings
            .roles
            .contains_key(&RoleName::Orchestrator)
    );
    assert_eq!(
        resolved.settings.captain_or_crew.otherwise,
        Settings::default().captain_or_crew.otherwise
    );
    warned(&resolved, "\"navigator\"");
    warned(&resolved, "\"crew_then_captain\"");
}

/// Two roles nobody knows and none that is known: the whole table is left out, and each of
/// the two is named, not only the first serde met.
#[test]
fn every_unknown_role_is_named_when_no_role_is_left() {
    let repo = Repo::new();
    repo.global(
        "[roles.navigator]\nmodels = [\"codex\"]\n\
         [roles.foreman]\nmodels = [\"claude-code\"]\n\
         [crew]\nmain = \"codex\"\n",
    );
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert_eq!(resolved.settings.crew.main.agent, "codex");
    warned(&resolved, "\"navigator\"");
    warned(&resolved, "\"foreman\"");
    let named = |name: &str| {
        resolved
            .warnings
            .iter()
            .filter(|warning| warning.contains(name))
            .count()
    };
    assert_eq!(
        (named("\"navigator\""), named("\"foreman\"")),
        (1, 1),
        "{:?}",
        resolved.warnings
    );
}

/// A table at fault as a whole says so once, however many keys it holds.
#[test]
fn a_table_missing_what_it_needs_is_named_once() {
    let repo = Repo::new();
    repo.global("[[checks.run]]\nname = \"unit\"\n[[checks.run]]\nname = \"lint\"\n");
    let resolved = resolve(&repo.forgiving()).unwrap();
    let about_checks = resolved
        .warnings
        .iter()
        .filter(|warning| warning.contains("checks.run"))
        .count();
    assert_eq!(about_checks, 1, "{:?}", resolved.warnings);
}

#[test]
fn a_wrong_value_falls_back_to_its_default() {
    let repo = Repo::new();
    repo.global("[review]\nper_task = \"two\"\n[ui]\nkeys = \"vim\"\n");
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert_eq!(
        resolved.settings.review.per_task,
        Settings::default().review.per_task
    );
    assert_eq!(resolved.settings.ui.keys, argonauts_core::KeyScheme::Vim);
    warned(&resolved, "review.per_task");
    warned(&resolved, &repo.global_file());
}

#[test]
fn a_value_the_checks_refuse_falls_back_too() {
    let repo = Repo::new();
    repo.global("[evolution]\nevolvable = [\"critique.rounds\", \"review.per_task\"]\n");
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert_eq!(
        resolved.settings.evolution.evolvable,
        Settings::default().evolution.evolvable
    );
    warned(&resolved, "review.per_task");
}

#[test]
fn a_workflow_with_no_such_name_runs_the_default_one_with_one_notice() {
    let repo = Repo::new();
    repo.project("workflow = \"no-such-workflow\"\n");
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert_eq!(resolved.settings.workflow, "captain");
    let named: Vec<&SettingsWarning> = resolved
        .warnings
        .iter()
        .filter(|warning| warning.contains("no-such-workflow"))
        .collect();
    assert_eq!(
        named,
        [
            "workflow = \"no-such-workflow\" in .argonauts/config.toml names no workflow, so the \
             default is used; /workflow lists the ones you can pick"
        ]
    );
}

#[test]
fn a_workflow_with_no_such_name_still_stops_a_strict_check() {
    let repo = Repo::new();
    repo.project("workflow = \"no-such-workflow\"\n");
    let error = resolve(&repo.sources()).unwrap_err().to_string();
    assert!(
        error.contains("pick one of council, orchestrator, captain"),
        "{error}"
    );
}

#[test]
fn odd_names_and_global_only_keys_in_a_project_file_are_ignored() {
    let repo = Repo::new();
    repo.project(
        "[agents.\"my.tool\"]\nextra_args = [\"--yolo\"]\n\
         [notifications.conversation]\ncommand = [\"/usr/bin/notify\", \"{text}\"]\n\
         [ui]\nkeys = \"vim\"\n",
    );
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert!(!resolved.settings.agents.contains_key("my.tool"));
    assert!(
        resolved
            .settings
            .notifications
            .conversation
            .command
            .is_empty()
    );
    assert_eq!(resolved.settings.ui.keys, argonauts_core::KeyScheme::Vim);
    warned(&resolved, "\"my.tool\" under agents");
    warned(&resolved, "notifications.conversation.command");
}

#[test]
fn a_flag_you_typed_still_stops_with_its_error() {
    let repo = Repo::new();
    let sources = SettingsSources {
        overrides: vec!["review.per_task=\"two\"".to_owned()],
        ..repo.forgiving()
    };
    let err = resolve(&sources).unwrap_err();
    assert!(err.to_string().contains("review.per_task"), "{err}");
}

#[test]
fn only_your_own_file_turns_telemetry_on() {
    let repo = Repo::new();
    repo.project("[telemetry]\nenabled = true\n");
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert!(!resolved.settings.telemetry.enabled);
    warned(&resolved, "telemetry.enabled");

    repo.project("");
    repo.global("[telemetry]\nenabled = true\n");
    assert!(
        resolve(&repo.forgiving())
            .unwrap()
            .settings
            .telemetry
            .enabled
    );
}

#[test]
fn a_file_that_doesnt_read_as_toml_is_skipped_and_approvals_wait_for_you() {
    use argonauts_core::settings::approvals::ApprovalMode;

    let repo = Repo::new();
    repo.global("[approvals]\nautopilot = true\nmerge.mode = \"auto\n");
    repo.project("[approvals]\nautopilot = true\n");
    let resolved = resolve(&repo.forgiving()).unwrap();
    // One plain line that names the place once.
    let warning = resolved
        .warnings
        .iter()
        .find(|warning| warning.contains("doesn't read as TOML"))
        .expect("the skipped file's warning");
    assert!(
        warning.contains("doesn't read as TOML at line 3 (")
            && warning.ends_with(
                "): merge.mode = \"auto. It is skipped until fixed, and until then approvals \
                 wait for you and autopilot is off"
            ),
        "{warning}"
    );
    assert_eq!(warning.matches("line 3").count(), 1, "{warning}");
    assert!(!warning.contains('\n'), "{warning}");
    let approvals = &resolved.settings.approvals;
    assert!(!approvals.autopilot);
    assert_eq!(approvals.merge.mode, ApprovalMode::Supervised);
    assert_eq!(approvals.mode(), ApprovalMode::Supervised);
    assert!(
        resolve(&repo.trusted()).is_err(),
        "a check still refuses it"
    );
}

#[test]
fn a_switch_written_as_its_default_is_known() {
    let repo = Repo::new();
    repo.global("[crew]\nworker_workflows = false\n");
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    assert!(
        resolved
            .entries
            .iter()
            .any(|entry| entry.key == "crew.worker_workflows")
    );
    let catalog = crate::catalog::settings_catalog();
    for key in ["crew.worker_workflows", "roles.*.shell"] {
        assert!(catalog.iter().any(|meta| meta.key == key), "{key}");
    }
}
