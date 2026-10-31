use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use argonauts_conductor::{AgentCheck, AgentStatus, DoctorReport, GitCheck};
use argonauts_core::app::setup_sheet::{SetupSheet, SheetAction, SheetRow, WorkflowStatus};
use argonauts_core::app::{AppEvent, UiCommand, ViewData};
use argonauts_core::settings::SettingScope;
use argonauts_testkit::TempRepo;

use super::*;
use crate::controller::base::test_support::{CliOptions, TestDriver};

/// A CLI on PATH whose login check exits `code`: 0 is logged in, 1 logged out.
fn fake_cli(dir: &Path, name: &str, code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    // Written beside it and moved over it, so a check running it meanwhile never runs half.
    let fresh = dir.join(format!(".{name}.new"));
    std::fs::write(&fresh, format!("#!/bin/sh\nexit {code}\n")).unwrap();
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&fresh, &path).unwrap();
    path
}

fn check(id: &str, name: &str, binary: &str, path: Option<PathBuf>) -> AgentCheck {
    AgentCheck {
        id: id.to_owned(),
        display_name: name.to_owned(),
        binary: binary.to_owned(),
        status: if path.is_some() {
            AgentStatus::Tested
        } else {
            AgentStatus::NotInstalled
        },
        version: path.as_ref().map(|_| "1.0.0".to_owned()),
        path,
        tested_versions: Vec::new(),
        cheapest_model: String::new(),
        cheapest_effort: None,
        problem: None,
        enabled: true,
    }
}

/// Claude Code logged in, Codex logged in or out as `codex_in` says, and agy and OpenCode not
/// installed.
fn report(repo: &TempRepo, codex_in: bool) -> DoctorReport {
    let bin = repo.home().join("bin");
    let claude = fake_cli(&bin, "claude", 0);
    let codex = fake_cli(&bin, "codex", if codex_in { 0 } else { 1 });
    DoctorReport {
        agents: vec![
            check("claude-code", "Claude Code", "claude", Some(claude)),
            check("codex", "Codex", "codex", Some(codex)),
            check("antigravity-cli", "agy", "agy", None),
            check("opencode", "OpenCode", "opencode", None),
        ],
        git: GitCheck {
            version: Some("2.50.0".to_owned()),
            problem: None,
        },
    }
}

fn on_captain() -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            let global = repo.global_config();
            std::fs::create_dir_all(global.parent().unwrap()).unwrap();
            std::fs::write(global, "workflow = \"captain\"\n").unwrap();
        })),
        ..CliOptions::default()
    }
}

fn file_says(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[tokio::test]
async fn a_bare_council_switches_where_changes_save_and_says_what_it_was() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.submit("/council").await;
    let changed = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { key, .. } if key == "workflow"))
        .await;
    let AppEvent::SettingChanged {
        value,
        scope,
        previous,
        ..
    } = changed
    else {
        unreachable!()
    };
    assert_eq!(value, "\"council\"");
    assert_eq!(scope, SettingScope::Global);
    assert_eq!(previous.as_deref(), Some("\"captain\""));
    assert!(file_says(&driver.repo.global_config()).contains("workflow = \"council\""));
    driver.exit().await;
}

#[tokio::test]
async fn the_workflow_in_use_says_so_and_saves_nothing() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.submit("/captain").await;
    let said = driver
        .until(|event| matches!(event, AppEvent::CommandReplied { .. }))
        .await;
    let AppEvent::CommandReplied { reply } = said else {
        unreachable!()
    };
    assert_eq!(
        reply.text,
        "Captain is the workflow already · Opus 5.5 does the work itself"
    );
    assert_eq!(
        file_says(&driver.repo.global_config()),
        "workflow = \"captain\"\n"
    );
    driver.exit().await;
}

/// A repository you clone can't send your changes into its shared file: its
/// `save_changes_to` counts for nothing, and a bare switch still saves where yours say.
#[tokio::test]
async fn a_team_file_that_sets_save_changes_to_sends_no_switch_into_it() {
    let options = CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            let global = repo.global_config();
            std::fs::create_dir_all(global.parent().unwrap()).unwrap();
            std::fs::write(global, "workflow = \"captain\"\n").unwrap();
            let project = repo.root().join(".argonauts/config.toml");
            std::fs::create_dir_all(project.parent().unwrap()).unwrap();
            std::fs::write(project, "save_changes_to = \"this_project\"\n").unwrap();
        })),
        ..CliOptions::default()
    };
    let mut driver = TestDriver::start_with(options).await;
    driver.submit("/council").await;
    let changed = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { key, .. } if key == "workflow"))
        .await;
    assert!(
        matches!(
            &changed,
            AppEvent::SettingChanged {
                scope: SettingScope::Global,
                ..
            }
        ),
        "{changed:?}"
    );
    assert!(file_says(&driver.repo.global_config()).contains("workflow = \"council\""));
    let team = file_says(&driver.repo.root().join(".argonauts/config.toml"));
    assert!(!team.contains("workflow"), "{team}");
    driver.exit().await;
}

#[tokio::test]
async fn a_team_file_that_sets_the_workflow_moves_the_switch_to_just_you() {
    let options = CliOptions {
        prepare: Some(Box::new(|repo: &TempRepo| {
            let project = repo.root().join(".argonauts/config.toml");
            std::fs::create_dir_all(project.parent().unwrap()).unwrap();
            std::fs::write(project, "workflow = \"captain\"\n").unwrap();
        })),
        ..CliOptions::default()
    };
    let mut driver = TestDriver::start_with(options).await;
    driver.submit("/workflow council").await;
    let changed = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { key, .. } if key == "workflow"))
        .await;
    assert!(
        matches!(
            &changed,
            AppEvent::SettingChanged {
                scope: SettingScope::ProjectLocal,
                why: Some(why),
                ..
            } if why == "since the team's file sets the workflow"
        ),
        "{changed:?}"
    );
    let local = driver.repo.root().join(".argonauts/config.local.toml");
    assert!(file_says(&local).contains("workflow = \"council\""));
    assert!(file_says(&driver.repo.root().join(".argonauts/config.toml")).contains("captain"));
    driver.exit().await;
}

fn sheet_of(event: &AppEvent) -> Option<&SetupSheet> {
    match event {
        AppEvent::View { data } => match data.as_ref() {
            ViewData::SetupSheet { sheet } => Some(sheet),
            _ => None,
        },
        _ => None,
    }
}

fn says(sheet: &SetupSheet, name: &str) -> bool {
    sheet.rows.iter().any(|row| match row {
        SheetRow::Aside(aside) => aside.name == name,
        _ => false,
    })
}

#[tokio::test]
async fn council_with_one_ready_cli_opens_its_sheet_and_switches_once_a_second_logs_in() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.cli_facts.checked(report(&driver.repo, false));
    driver.submit("/council").await;
    let opened = driver.until(|event| sheet_of(event).is_some()).await;
    let sheet = sheet_of(&opened).unwrap();
    assert!(!sheet.ready && !sheet.refresh);
    assert_eq!(sheet.title, "Set up Council");
    assert_eq!(
        sheet.pointer.as_ref().map(|at| at.row.as_str()),
        Some("add")
    );
    assert!(says(sheet, "Codex"), "Codex shows as logged out");
    assert!(file_says(&driver.repo.global_config()).contains("workflow = \"captain\""));

    // Codex logs in from another terminal: the sheet reads it again by itself.
    fake_cli(&driver.repo.home().join("bin"), "codex", 0);
    let deadline = argonauts_testkit::pty::deadline::scaled(std::time::Duration::from_secs(20));
    let again = driver
        .until_within(deadline, |event| {
            sheet_of(event).is_some_and(|sheet| sheet.refresh && !says(sheet, "Codex"))
        })
        .await;
    let sheet = sheet_of(&again).unwrap();
    assert!(sheet.ready, "two seats can answer now: {sheet:#?}");
    driver
        .send(UiCommand::SetupSheet {
            action: SheetAction::Save { session: false },
        })
        .await;
    let changed = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { key, .. } if key == "workflow"))
        .await;
    assert!(matches!(
        changed,
        AppEvent::SettingChanged {
            scope: SettingScope::Global,
            ..
        }
    ));
    let global = file_says(&driver.repo.global_config());
    assert!(global.contains("workflow = \"council\""), "{global}");
    driver.exit().await;
}

#[tokio::test]
async fn enter_saves_the_seats_in_one_write_and_esc_saves_nothing() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.cli_facts.checked(report(&driver.repo, false));
    driver.submit("/council").await;
    driver.until(|event| sheet_of(event).is_some()).await;
    driver
        .send(UiCommand::SetupSheet {
            action: SheetAction::Close,
        })
        .await;
    driver.submit("/council").await;
    driver.until(|event| sheet_of(event).is_some()).await;
    driver
        .send(UiCommand::SetupSheet {
            action: SheetAction::Toggle { row: "add".into() },
        })
        .await;
    let two = driver
        .until(|event| sheet_of(event).is_some_and(|sheet| sheet.refresh))
        .await;
    assert!(sheet_of(&two).unwrap().ready);
    assert!(
        file_says(&driver.repo.global_config()).contains("workflow = \"captain\""),
        "nothing is saved before Enter"
    );
    driver
        .send(UiCommand::SetupSheet {
            action: SheetAction::Save { session: false },
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::SettingChanged { key, .. } if key == "workflow"))
        .await;
    let changes = driver
        .seen()
        .iter()
        .filter(|event| matches!(event, AppEvent::SettingChanged { .. }))
        .count();
    assert_eq!(changes, 1, "one line for the whole sheet");
    let global: toml::Table = file_says(&driver.repo.global_config()).parse().unwrap();
    assert_eq!(global["workflow"].as_str(), Some("council"));
    let seats = global["council"]["seat"].as_array().expect("the seats");
    assert_eq!(seats.len(), 2);
    assert_eq!(seats[0]["model"].as_str(), seats[1]["model"].as_str());
    driver.exit().await;
}

#[tokio::test]
async fn config_workflow_goes_through_the_same_check() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.cli_facts.checked(report(&driver.repo, false));
    driver.submit("/config workflow=council").await;
    driver.until(|event| sheet_of(event).is_some()).await;
    driver.exit().await;
}

/// The `/workflow` picker's pick, for the place it names.
async fn pick_in_the_picker(driver: &TestDriver, name: &str, scope: SettingScope) {
    driver
        .send(UiCommand::SetSetting {
            key: "workflow".to_owned(),
            value: format!("\"{name}\""),
            scope: Some(scope),
        })
        .await;
}

#[tokio::test]
async fn the_pickers_pick_switches_where_it_says_and_opens_the_sheet_when_not_ready() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    pick_in_the_picker(&driver, "council", SettingScope::Project).await;
    let changed = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { key, .. } if key == "workflow"))
        .await;
    assert!(
        matches!(
            changed,
            AppEvent::SettingChanged {
                scope: SettingScope::Project,
                ..
            }
        ),
        "{changed:?}"
    );
    let project = driver.repo.root().join(".argonauts/config.toml");
    assert!(file_says(&project).contains("workflow = \"council\""));
    assert!(file_says(&driver.repo.global_config()).contains("workflow = \"captain\""));
    driver.exit().await;

    // Not ready, the pick opens the sheet, and Enter saves to the place the pick named.
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.cli_facts.checked(report(&driver.repo, false));
    pick_in_the_picker(&driver, "council", SettingScope::ProjectLocal).await;
    driver.until(|event| sheet_of(event).is_some()).await;
    driver
        .send(UiCommand::SetupSheet {
            action: SheetAction::Toggle { row: "add".into() },
        })
        .await;
    driver
        .until(|event| sheet_of(event).is_some_and(|sheet| sheet.refresh && sheet.ready))
        .await;
    driver
        .send(UiCommand::SetupSheet {
            action: SheetAction::Save { session: false },
        })
        .await;
    driver
        .until(|event| {
            matches!(event, AppEvent::SettingChanged { key, scope: SettingScope::ProjectLocal, .. }
                if key == "workflow")
        })
        .await;
    let local = driver.repo.root().join(".argonauts/config.local.toml");
    assert!(file_says(&local).contains("workflow = \"council\""));
    driver.exit().await;
}

#[tokio::test]
async fn a_workflow_only_this_repository_has_switches_for_this_project_just_you() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.submit("/workflow new spec-first").await;
    driver
        .until(
            |event| matches!(event, AppEvent::Record { title, .. } if title.contains("spec-first")),
        )
        .await;
    driver.submit("/workflow spec-first").await;
    let changed = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { key, .. } if key == "workflow"))
        .await;
    assert!(
        matches!(
            &changed,
            AppEvent::SettingChanged {
                scope: SettingScope::ProjectLocal,
                why: Some(why),
                ..
            } if why == "since only this project has it"
        ),
        "{changed:?}"
    );
    assert!(file_says(&driver.repo.global_config()).contains("workflow = \"captain\""));
    driver.exit().await;
}

/// A place flag on a workflow's own command, or on one of yours, switches to it there, as the
/// flag on `/workflow <name>` does, and says nothing of why.
#[tokio::test]
async fn a_place_flag_on_a_workflows_command_switches_there() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.submit("/council --session").await;
    let changed = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { key, .. } if key == "workflow"))
        .await;
    assert!(
        matches!(
            &changed,
            AppEvent::SettingChanged {
                scope: SettingScope::Session,
                why: None,
                ..
            }
        ),
        "{changed:?}"
    );
    assert_eq!(
        file_says(&driver.repo.global_config()),
        "workflow = \"captain\"\n",
        "nothing written for this session"
    );

    driver.submit("/orchestrator --local").await;
    let changed = driver
        .until(|event| {
            matches!(event, AppEvent::SettingChanged { key, value, .. }
                if key == "workflow" && value == "\"orchestrator\"")
        })
        .await;
    assert!(
        matches!(
            &changed,
            AppEvent::SettingChanged {
                scope: SettingScope::ProjectLocal,
                ..
            }
        ),
        "{changed:?}"
    );
    driver.exit().await;
}

#[tokio::test]
async fn an_unknown_name_is_refused() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    driver.submit("/workflow nothing-here").await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.contains("no workflow named nothing-here"))
        })
        .await;
    driver.exit().await;
}

mod ready {
    use argonauts_core::runs::events::council::SeatRole;
    use argonauts_core::settings::council::CouncilSeat;

    use super::*;

    fn cli(id: &str, now: Now) -> Cli {
        Cli {
            id: id.to_owned(),
            name: id.to_owned(),
            now,
        }
    }

    fn only_claude() -> Present {
        Present::of(vec![
            cli("claude-code", Now::Ready),
            cli(
                "codex",
                Now::LoggedOut {
                    login: Some("codex login".into()),
                },
            ),
            cli(
                "antigravity-cli",
                Now::NotInstalled {
                    add: "/agents add agy".into(),
                },
            ),
        ])
    }

    fn provider(worker: &ModelRef) -> String {
        worker.agent.clone()
    }

    #[test]
    fn one_model_in_two_seats_makes_a_council() {
        let mut settings = Settings::default();
        let present = only_claude();
        assert!(matches!(
            readiness(&settings, &present, &provider, COUNCIL, None),
            Readiness::Needs(_)
        ));
        settings.council.seats = vec![
            CouncilSeat {
                model: "claude-code/opus".parse().unwrap(),
                role: SeatRole::Proposer,
            },
            CouncilSeat {
                model: "claude-code/opus".parse().unwrap(),
                role: SeatRole::Skeptic,
            },
        ];
        assert_eq!(
            readiness(&settings, &present, &provider, COUNCIL, None),
            Readiness::Ready
        );
        for seat in &mut settings.council.seats {
            seat.role = SeatRole::Skeptic;
        }
        assert_eq!(
            readiness(&settings, &present, &provider, COUNCIL, None),
            Readiness::Needs("every seat is a skeptic, so nobody proposes".to_owned())
        );
    }

    #[test]
    fn captain_needs_its_main_agent_and_a_reviewer_elsewhere_when_it_reviews() {
        let mut settings = Settings::default();
        settings.crew.main = "claude-code".parse().unwrap();
        let present = only_claude();
        assert_eq!(
            readiness(&settings, &present, &provider, CAPTAIN, None),
            Readiness::Ready
        );
        settings.captain.review = true;
        assert!(matches!(
            readiness(&settings, &present, &provider, CAPTAIN, None),
            Readiness::Needs(why) if why.contains("another provider")
        ));
        settings.crew.main = "codex".parse().unwrap();
        settings.captain.review = false;
        assert_eq!(
            readiness(&settings, &present, &provider, CAPTAIN, None),
            Readiness::Needs("it needs the main agent, and codex is logged out".to_owned())
        );
    }

    #[test]
    fn orchestrator_needs_two_reviewers() {
        let settings = Settings::default();
        assert!(matches!(
            readiness(&settings, &only_claude(), &provider, ORCHESTRATOR, None),
            Readiness::Needs(why) if why == "it needs two reviewers for each task, and only one is ready"
        ));
        let everyone = Present::of(vec![
            cli("claude-code", Now::Ready),
            cli("codex", Now::Ready),
        ]);
        assert_eq!(
            readiness(&settings, &everyone, &provider, ORCHESTRATOR, None),
            Readiness::Ready
        );
    }

    #[test]
    fn a_workflow_of_yours_needs_each_role_its_steps_name() {
        let settings = Settings::default();
        let steps = [
            (CrewRole::Worker, None),
            (CrewRole::Reviewer, Some("codex".parse().unwrap())),
        ];
        assert_eq!(
            readiness(
                &settings,
                &only_claude(),
                &provider,
                "spec-first",
                Some(&steps)
            ),
            Readiness::Needs("nobody ready fills its reviewer step's role".to_owned())
        );
        assert_eq!(
            readiness(
                &settings,
                &only_claude(),
                &provider,
                "spec-first",
                Some(&steps[..1])
            ),
            Readiness::Ready
        );
    }

    #[test]
    fn without_doctor_nothing_counts_against_an_agent_that_is_on() {
        let present = Present::default();
        assert!(present.ready("codex"));
        assert!(present.ready("ollama"));
    }
}

/// Every key a sheet writes is one the settings know, in a form they take.
#[test]
fn what_a_sheet_writes_passes_the_settings_check() {
    use argonauts_core::app::setup_sheet::SheetColumn;

    use super::sheet::{Draft, Facts, Model};
    use crate::controller::base::test_support::TestCtx;

    let (parts, _) = TestCtx::new(CliOptions::default());
    let ctx = &parts.ctx;
    let model = |worker: &str, name: &str, provider: &str| Model {
        worker: worker.parse().unwrap(),
        name: name.to_owned(),
        provider: provider.to_owned(),
    };
    let facts = Facts {
        models: vec![
            model("claude-code/claude-opus-5-5", "Opus 5.5", "anthropic"),
            model("codex/gpt-6-luna", "5.6 Luna", "openai"),
        ],
        workflows: vec!["maker-checker".into()],
        ..Facts::default()
    };
    let steps: Vec<(String, CrewRole)> = [
        ("Plan", CrewRole::Orchestrator),
        ("Build", CrewRole::Worker),
        ("Review", CrewRole::Reviewer),
        ("Sign-off", CrewRole::Judge),
    ]
    .map(|(label, role)| (label.to_owned(), role))
    .to_vec();
    let mut orchestrator = Draft::new(ctx.settings(), ORCHESTRATOR, &facts, Some(&steps));
    orchestrator.change("plan", SheetColumn::Who, true, &facts);
    orchestrator.change("build", SheetColumn::Who, false, &facts);
    orchestrator.change("review", SheetColumn::Who, false, &facts);
    orchestrator.change("ways.workflows", SheetColumn::Who, true, &facts);
    orchestrator.toggle("ways.agents", &facts);
    let mut council = Draft::new(ctx.settings(), COUNCIL, &facts, None);
    council.toggle("add", &facts);
    council.change("seat-1", SheetColumn::Role, true, &facts);
    council.toggle("ways.own_session", &facts);
    let mut captain = Draft::new(ctx.settings(), CAPTAIN, &facts, None);
    captain.change("main", SheetColumn::Who, true, &facts);
    let mut keys = Vec::new();
    for draft in [orchestrator, council, captain] {
        for (key, value) in draft.pairs() {
            check_known(ctx, &key, &value).unwrap_or_else(|err| panic!("{key} = {value}: {err}"));
            keys.push(key);
        }
    }
    for wanted in [
        "roles.orchestrator.models",
        "crew.orchestrator_picks.worker",
        "crew.orchestrator_picks.reviewer",
        "crew.judge",
        "crew.ways.workflows",
        "crew.ways.agents",
        "council.seat",
        "council.ways.own_session",
        "crew.main",
    ] {
        assert!(keys.iter().any(|key| key == wanted), "{wanted} in {keys:?}");
    }
}

/// With words, a built-in workflow's command runs that one request with it and switches
/// nothing.
#[tokio::test]
async fn with_words_council_and_orchestrator_run_one_request() {
    for workflow in ["council", "orchestrator"] {
        let driver = TestDriver::start_with(on_captain()).await;
        driver
            .submit(&format!("/{workflow} split the parser"))
            .await;
        let deadline =
            tokio::time::Instant::now() + crate::controller::base::test_support::max_wait();
        while driver.conductor.starts().is_empty() {
            assert!(tokio::time::Instant::now() < deadline, "no run started");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let start = driver.conductor.starts().remove(0);
        assert!(
            start
                .request_settings
                .contains(&format!("workflow=\"{workflow}\"")),
            "{:?}",
            start.request_settings
        );
        assert!(file_says(&driver.repo.global_config()).contains("workflow = \"captain\""));
        driver.exit().await;
    }
}

fn statuses_in(event: &AppEvent) -> Option<BTreeMap<String, WorkflowStatus>> {
    match event {
        AppEvent::WorkflowStatuses { statuses } => Some(statuses.iter().cloned().collect()),
        _ => None,
    }
}

#[tokio::test]
async fn the_slash_menu_hears_how_each_workflow_stands_and_again_after_the_logins() {
    let mut driver = TestDriver::start_with(on_captain()).await;
    // At start, before Doctor has looked, nothing counts against any agent.
    let first = driver.until(|event| statuses_in(event).is_some()).await;
    let first = statuses_in(&first).unwrap();
    assert_eq!(first["captain"], WorkflowStatus::InUse);
    assert_eq!(first["council"], WorkflowStatus::Ready);
    assert!(!first.contains_key("ask"), "{first:?}");

    // Codex is logged out: the logins read for /council say Council needs setting up.
    driver.cli_facts.checked(report(&driver.repo, false));
    driver.submit("/council").await;
    driver
        .until(|event| {
            statuses_in(event).is_some_and(|now| now["council"] == WorkflowStatus::SetUpFirst)
        })
        .await;
    driver.until(|event| sheet_of(event).is_some()).await;
    driver
        .send(UiCommand::SetupSheet {
            action: SheetAction::Close,
        })
        .await;

    // Codex logs in, and the switch to Council moves "in use" there.
    fake_cli(&driver.repo.home().join("bin"), "codex", 0);
    driver.submit("/council").await;
    let switched = driver
        .until(|event| {
            statuses_in(event).is_some_and(|now| now["council"] == WorkflowStatus::InUse)
        })
        .await;
    assert_eq!(
        statuses_in(&switched).unwrap()["captain"],
        WorkflowStatus::Ready
    );
    driver.exit().await;
}

/// A window's reset reads in the computer's own zone, from another thread too.
#[test]
fn a_reset_reads_in_the_clocks_zone() {
    use time::macros::datetime;

    let clock = argonauts_tui::base::format::WallClock::pinned(datetime!(2026-10-26 9:00 +02:00));
    assert_eq!(
        super::reset_time(&clock, datetime!(2026-10-26 13:40 UTC)),
        "3:40 PM"
    );
}
