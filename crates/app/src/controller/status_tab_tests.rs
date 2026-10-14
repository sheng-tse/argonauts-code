use argonauts_core::api::UsagePeriod;
use argonauts_core::app::{AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::input::{SlashCommand, ViewAction};

use crate::controller::test_support::{Harness, HarnessOptions};

/// A folder without git, as someone's Downloads folder is.
async fn no_git() -> Harness {
    Harness::start_with(HarnessOptions {
        prepare: Some(Box::new(|repo| {
            std::fs::remove_dir_all(repo.root().join(".git")).expect("the repo's .git removed");
        })),
        ..HarnessOptions::default()
    })
    .await
}

fn is_answer(event: &AppEvent) -> bool {
    matches!(event, AppEvent::View { .. })
}

/// What each tab asks for as it opens and as it stays open.
fn every_tab() -> Vec<UiCommand> {
    let mut asks = vec![ViewRequest::StatusTab, ViewRequest::Context];
    asks.extend(
        [
            UsagePeriod::Today,
            UsagePeriod::Week,
            UsagePeriod::Month,
            UsagePeriod::All,
        ]
        .map(|period| ViewRequest::Usage { period }),
    );
    // The Usage tab reads every CLI again as it opens, and the Overview asks for its numbers.
    asks.push(ViewRequest::ReadPlanUsage);
    asks.push(ViewRequest::Overview {
        period: argonauts_core::overview::OverviewPeriod::All,
        import: None,
    });
    let mut commands: Vec<UiCommand> = asks
        .into_iter()
        .map(|view| UiCommand::Request { view })
        .collect();
    // `/status` typed opens the same tab.
    commands.push(UiCommand::Slash {
        command: SlashCommand::Status {
            action: ViewAction::Open,
        },
    });
    commands
}

#[tokio::test]
async fn opening_every_tab_with_nothing_to_show_writes_nothing_into_the_chat() {
    let mut harness = no_git().await;
    // Whatever the opening says is said by the time its first answer comes.
    harness
        .send(UiCommand::Request {
            view: ViewRequest::StatusTab,
        })
        .await;
    harness.until(is_answer).await;
    let opened = harness.seen().len();
    // Twice over, as the tabs ask again while they stay open.
    for command in every_tab().into_iter().chain(every_tab()) {
        // A live read answers later, through the next numbers the tab asks for.
        let answers = !matches!(
            command,
            UiCommand::Request {
                view: ViewRequest::ReadPlanUsage
            }
        );
        harness.send(command).await;
        if answers {
            harness.until(is_answer).await;
        }
    }
    let after: Vec<AppEvent> = harness.seen()[opened..].to_vec();
    let spoke: Vec<&AppEvent> = after
        .iter()
        .filter(|event| !matches!(event, AppEvent::View { .. } | AppEvent::Status { .. }))
        .collect();
    assert!(spoke.is_empty(), "the tabs wrote into the chat: {spoke:#?}");
    let status_tab = after
        .iter()
        .filter(|event| {
            matches!(event, AppEvent::View { data }
                if matches!(data.as_ref(), ViewData::StatusTab { .. }))
        })
        .count();
    assert_eq!(status_tab, 4, "the tab's own read and /status, twice");
    harness.exit().await;
}

#[tokio::test]
async fn the_status_tab_answers_with_no_run_and_no_git() {
    let mut harness = no_git().await;
    harness
        .send(UiCommand::Request {
            view: ViewRequest::StatusTab,
        })
        .await;
    let AppEvent::View { data } = harness.until(is_answer).await else {
        unreachable!()
    };
    let ViewData::StatusTab { view } = *data else {
        panic!("{data:?}")
    };
    assert!(view.runs.is_empty() && view.run.is_none());
    assert!(!view.folder.git);
    assert_eq!(view.version, env!("CARGO_PKG_VERSION"));
    harness.exit().await;
}

#[test]
fn claude_codes_auth_status_names_the_account_and_the_plan() {
    let printed = r#"{
      "loggedIn": true,
      "authMethod": "claude.ai",
      "email": "you@example.com",
      "subscriptionType": "max"
    }"#;
    assert_eq!(
        super::sign_in_from(printed),
        super::SignIn {
            account: Some("you@example.com".to_owned()),
            plan: Some("Max".to_owned()),
            signed_out: false,
        }
    );
    // Codex's says only how it is signed in.
    assert_eq!(
        super::sign_in_from("Logged in using ChatGPT\n"),
        super::SignIn::default()
    );
    // Claude Code's says when no one is.
    let out = super::sign_in_from(r#"{"loggedIn": false, "authMethod": "none"}"#);
    assert!(out.signed_out && out.account.is_none(), "{out:?}");
}

/// A login check that fails, as `codex login status` does with no one signed in, means
/// signed out, never "signed in".
#[tokio::test]
async fn a_login_check_that_fails_means_signed_out() {
    let dir = tempfile::tempdir().unwrap();
    let check = dir.path().join("login-check");
    std::fs::write(&check, "#!/bin/sh\necho 'Not logged in'\nexit 1\n").unwrap();
    std::fs::set_permissions(&check, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let out = super::sign_in(&check, &[]).await.unwrap();
    assert!(out.signed_out && out.account.is_none(), "{out:?}");
}

fn check(
    id: &str,
    name: &str,
    status: argonauts_conductor::AgentStatus,
) -> argonauts_conductor::AgentCheck {
    argonauts_conductor::AgentCheck {
        id: id.to_owned(),
        display_name: name.to_owned(),
        binary: id.to_owned(),
        path: None,
        version: Some("1.0.0".to_owned()),
        status,
        tested_versions: Vec::new(),
        cheapest_model: String::new(),
        cheapest_effort: None,
        problem: None,
        enabled: true,
    }
}

#[test]
fn every_cli_doctor_checked_is_listed_the_main_sessions_first() {
    use argonauts_conductor::{AgentStatus, DoctorReport, GitCheck};

    let report = DoctorReport {
        agents: vec![
            check("codex", "Codex", AgentStatus::Tested),
            check("claude-code", "Claude Code", AgentStatus::Newer),
            check("opencode", "OpenCode", AgentStatus::NotInstalled),
        ],
        git: GitCheck {
            version: None,
            problem: None,
        },
    };
    let mut sign_ins = std::collections::BTreeMap::new();
    sign_ins.insert(
        "claude-code".to_owned(),
        super::SignIn {
            account: Some("you@example.com".to_owned()),
            plan: Some("Max".to_owned()),
            signed_out: false,
        },
    );
    // Codex's login check names no account; its live plan reading does.
    let quota = argonauts_testkit::services::MemoryQuotaBoard::default();
    let mut live = argonauts_core::plan_usage::LiveUsage::new(
        argonauts_core::names::VendorId("codex".to_owned()),
        argonauts_core::limits::QuotaSource::AppServer,
        time::OffsetDateTime::now_utc(),
    );
    live.account = Some("me@work.example".to_owned());
    live.plan = Some("Plus".to_owned());
    argonauts_core::limits::QuotaBoard::observe_live(&quota, live);
    let clis = super::clis(&report, "claude-code", &sign_ins, &quota);
    let names: Vec<&str> = clis.iter().map(|cli| cli.name.as_str()).collect();
    assert_eq!(names, ["Claude Code", "Codex", "OpenCode"]);
    assert!(clis[0].main);
    assert_eq!(clis[0].account.as_deref(), Some("you@example.com"));
    assert_eq!(clis[1].account.as_deref(), Some("me@work.example"));
    assert_eq!(clis[1].plan.as_deref(), Some("Plus"));
    assert_eq!(clis[0].note, "newer than checked, works");
    assert_eq!(clis[1].note, "checked");
    assert!(!clis[2].installed);
    assert_eq!(clis[2].version, None);
}

#[test]
fn only_other_servers_count_as_the_main_sessions_mcp_servers() {
    let tools = [
        "Bash",
        "mcp__github__create_issue",
        "mcp__argonauts__argonauts_status",
        "mcp__argonauts_status__approve",
        "mcp__linear__list",
        "mcp__github__comment",
    ]
    .map(str::to_owned);
    assert_eq!(
        crate::controller::chat::own_mcp_servers(&tools),
        ["github", "linear"]
    );
}

#[tokio::test]
async fn the_status_tab_counts_each_settings_file_and_says_how_the_folder_is_set_up() {
    let mut harness = Harness::start_with(HarnessOptions {
        prepare: Some(Box::new(|repo| {
            std::fs::create_dir_all(repo.home()).unwrap();
            std::fs::write(repo.global_config(), "workflow_size = \"large\"\n").unwrap();
        })),
        ..HarnessOptions::default()
    })
    .await;
    harness
        .send(UiCommand::Request {
            view: ViewRequest::StatusTab,
        })
        .await;
    let AppEvent::View { data } = harness.until(is_answer).await else {
        unreachable!()
    };
    let ViewData::StatusTab { view } = *data else {
        panic!("{data:?}")
    };
    assert!(view.folder.git);
    assert_eq!(view.workflow.size, "large");
    assert!(!view.mode.is_empty());
    let global = view
        .settings
        .files
        .iter()
        .find(|file| file.layer == "global")
        .unwrap_or_else(|| panic!("{:#?}", view.settings));
    assert_eq!(global.keys, 1);
    assert!(global.path.ends_with("config.toml"), "{}", global.path);
    assert!(
        view.settings
            .keys
            .iter()
            .any(|entry| entry.key == "workflow_size" && entry.layer == "global")
    );
    assert!(view.settings.mods_built_in > 0);
    assert_eq!(view.main.as_ref().unwrap().worker.worker, "claude-code");
    // Doctor never ran here, so no CLI is listed and none was asked who it's signed in as.
    assert!(view.clis.is_empty());
    harness.exit().await;
}

/// A settings file still in the names from before reads as today's names, and the Status tab
/// counts and lists its keys under those names, as coming from that file.
#[tokio::test]
async fn the_status_tab_lists_an_old_files_keys_under_todays_names() {
    let mut harness = Harness::start_with(HarnessOptions {
        prepare: Some(Box::new(|repo| {
            std::fs::create_dir_all(repo.home()).unwrap();
            std::fs::write(
                repo.global_config(),
                "preset = \"peer-crew\"\n\n[roles.coordinator]\nworkers = [\"claude-code\"]\n",
            )
            .unwrap();
        })),
        ..HarnessOptions::default()
    })
    .await;
    harness
        .send(UiCommand::Request {
            view: ViewRequest::StatusTab,
        })
        .await;
    let AppEvent::View { data } = harness.until(is_answer).await else {
        unreachable!()
    };
    let ViewData::StatusTab { view } = *data else {
        panic!("{data:?}")
    };
    let from_the_file: Vec<(&str, &str)> = view
        .settings
        .keys
        .iter()
        .filter(|entry| entry.layer == "global")
        .map(|entry| (entry.key.as_str(), entry.value.as_str()))
        .collect();
    assert!(
        from_the_file.contains(&("workflow", "\"council\"")),
        "{from_the_file:?}"
    );
    assert!(
        from_the_file
            .iter()
            .any(|(key, _)| *key == "roles.orchestrator.workers"),
        "{from_the_file:?}"
    );
    assert!(
        !from_the_file
            .iter()
            .any(|(key, _)| key.starts_with("preset") || key.contains("coordinator")),
        "{from_the_file:?}"
    );
    let global = view
        .settings
        .files
        .iter()
        .find(|file| file.layer == "global")
        .unwrap_or_else(|| panic!("{:#?}", view.settings));
    assert_eq!(global.keys, from_the_file.len());
    harness.exit().await;
}
