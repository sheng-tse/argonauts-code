//! As the app reads the files at start: a key that can't be used is left out with a warning
//! that names it, and the rest of the file still applies.

use argonauts_core::names::RoleName;

use super::*;
use crate::resolve;

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
        "[roles.foreman]\nworkers = [\"codex\"]\n\
         [roles.orchestrator]\nworkers = [\"claude-code\"]\n",
    );
    let resolved = resolve(&repo.forgiving()).unwrap();
    let orchestrator = &resolved.settings.roles[&RoleName::Orchestrator];
    assert_eq!(orchestrator.workers.len(), 1);
    warned(&resolved, "\"foreman\"");
    warned(&resolved, "so the default is used");
    assert!(
        resolve(&repo.trusted()).is_err(),
        "a check still refuses it"
    );
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
fn a_workflow_with_no_such_name_runs_the_default_one() {
    let repo = Repo::new();
    repo.project("workflow = \"no-such-workflow\"\n");
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert_eq!(resolved.settings.workflow, Settings::default().workflow);
    warned(&resolved, "no-such-workflow");
}

#[test]
fn odd_names_and_global_only_keys_in_a_project_file_are_ignored() {
    let repo = Repo::new();
    repo.project(
        "[agents.\"my.tool\"]\nextra_args = [\"--yolo\"]\n\
         [notifications.chat]\ncommand = [\"/usr/bin/notify\", \"{text}\"]\n\
         [ui]\nkeys = \"vim\"\n",
    );
    let resolved = resolve(&repo.forgiving()).unwrap();
    assert!(!resolved.settings.agents.contains_key("my.tool"));
    assert!(resolved.settings.notifications.chat.command.is_empty());
    assert_eq!(resolved.settings.ui.keys, argonauts_core::KeyScheme::Vim);
    warned(&resolved, "\"my.tool\" under agents");
    warned(&resolved, "notifications.chat.command");
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
