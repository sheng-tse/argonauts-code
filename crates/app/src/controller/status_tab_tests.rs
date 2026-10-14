use argonauts_core::api::UsagePeriod;
use argonauts_core::app::{AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::input::{SlashCommand, ViewAction};

use crate::controller::test_support::{CliOptions, TestDriver};

/// A folder without git, as someone's Downloads folder is.
async fn no_git() -> TestDriver {
    TestDriver::start_with(CliOptions {
        prepare: Some(Box::new(|repo| {
            std::fs::remove_dir_all(repo.root().join(".git")).expect("the repo's .git removed");
        })),
        ..CliOptions::default()
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
async fn opening_every_tab_with_nothing_to_show_writes_nothing_into_the_conversation() {
    let mut driver = no_git().await;
    // Whatever the opening says is said by the time its first answer comes.
    driver
        .send(UiCommand::Request {
            view: ViewRequest::StatusTab,
        })
        .await;
    driver.until(is_answer).await;
    let opened = driver.seen().len();
    // Twice over, as the tabs ask again while they stay open.
    for command in every_tab().into_iter().chain(every_tab()) {
        // A live read answers later, through the next numbers the tab asks for.
        let answers = !matches!(
            command,
            UiCommand::Request {
                view: ViewRequest::ReadPlanUsage
            }
        );
        driver.send(command).await;
        if answers {
            driver.until(is_answer).await;
        }
    }
    let after: Vec<AppEvent> = driver.seen()[opened..].to_vec();
    let spoke: Vec<&AppEvent> = after
        .iter()
        .filter(|event| !matches!(event, AppEvent::View { .. } | AppEvent::Status { .. }))
        .collect();
    assert!(
        spoke.is_empty(),
        "the tabs wrote into the conversation: {spoke:#?}"
    );
    let status_tab = after
        .iter()
        .filter(|event| {
            matches!(event, AppEvent::View { data }
                if matches!(data.as_ref(), ViewData::StatusTab { .. }))
        })
        .count();
    assert_eq!(status_tab, 4, "the tab's own read and /status, twice");
    driver.exit().await;
}

#[tokio::test]
async fn the_status_tab_answers_with_no_run_and_no_git() {
    let mut driver = no_git().await;
    driver
        .send(UiCommand::Request {
            view: ViewRequest::StatusTab,
        })
        .await;
    let AppEvent::View { data } = driver.until(is_answer).await else {
        unreachable!()
    };
    let ViewData::StatusTab { view } = *data else {
        panic!("{data:?}")
    };
    assert!(view.runs.is_empty() && view.run.is_none());
    assert!(!view.folder.git);
    assert_eq!(view.version, env!("CARGO_PKG_VERSION"));
    driver.exit().await;
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
            ..super::SignIn::default()
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

/// One answer holds how each CLI stands and how each of its models does. The CLIs are the
/// ones Doctor checked, each with what was made of its version, and those that are installed
/// have a row of health a model, where the one that is out says so.
#[tokio::test]
async fn one_answer_holds_each_clis_standing_and_each_models_health() {
    use argonauts_conductor::{AgentStatus, DoctorReport, GitCheck};
    use argonauts_core::limits::{ModelState, Trouble};
    use argonauts_core::names::ModelRef;
    use argonauts_core::status_tab::{Standing, VersionStanding};

    use crate::controller::conversation::ConversationState;
    use crate::controller::runs::RunsState;
    use crate::controller::test_support::TestCtx;

    let (mut parts, _opening) = TestCtx::new(CliOptions::default());
    parts.ctx.cli_facts.checked(DoctorReport {
        agents: vec![
            check("claude-code", "Claude Code", AgentStatus::Tested),
            check("opencode", "OpenCode", AgentStatus::NotInstalled),
        ],
        git: GitCheck {
            version: None,
            problem: None,
        },
    });
    let opus: ModelRef = "claude-code/claude-opus-5-5".parse().unwrap();
    parts
        .health
        .set_model(&opus, ModelState::Out, Some(Trouble::Overload));
    let conversation = ConversationState::new(&parts.ctx).unwrap();
    let mut runs = RunsState::new(&parts.ctx);
    let view = super::read(&mut parts.ctx, &mut runs, &conversation).await;

    let listed: Vec<&str> = view.clis.iter().map(|cli| cli.id.as_str()).collect();
    assert_eq!(listed, ["claude-code", "opencode"]);
    let claude = &view.clis[0];
    assert!(claude.installed && claude.main, "{claude:?}");
    assert_eq!(claude.version.as_deref(), Some("1.0.0"));
    assert_ne!(claude.check.standing, VersionStanding::Unknown);

    let health: Vec<&str> = view.health.clis.iter().map(|cli| cli.id.as_str()).collect();
    assert_eq!(health, listed, "a row of health for each CLI listed");
    let models = &view.health.clis[0].models;
    let out: Vec<&str> = models
        .iter()
        .filter(|model| model.state == Standing::Overloaded)
        .map(|model| model.name.as_str())
        .collect();
    assert_eq!(out, ["Opus 5.5"], "{models:#?}");
    // The main agent names no model and runs Opus 5.5: that model has one row, which says
    // what the main agent meets, and no row beside it speaks of a default.
    let named: Vec<&str> = models.iter().map(|model| model.name.as_str()).collect();
    assert!(!named.contains(&"its default"), "{named:?}");
    let opus_rows = named.iter().filter(|name| **name == "Opus 5.5").count();
    assert_eq!(opus_rows, 1, "{named:?}");
    assert!(
        view.health.clis[1].models.is_empty(),
        "nothing runs on a CLI that isn't here"
    );
}

#[test]
fn every_cli_doctor_checked_is_listed_the_main_sessions_first() {
    use argonauts_conductor::{AgentStatus, DoctorReport, GitCheck};

    let report = DoctorReport {
        agents: vec![
            check("codex", "Codex", AgentStatus::Tested),
            check("claude-code", "Claude Code", AgentStatus::Newer),
            check("opencode", "OpenCode", AgentStatus::NotInstalled),
            check("opencode-acp", "OpenCode (ACP)", AgentStatus::Untested),
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
            ..super::SignIn::default()
        },
    );
    // OpenCode names no account: it was asked what it can use.
    sign_ins.insert(
        "opencode-acp".to_owned(),
        super::SignIn {
            providers: Some(Vec::new()),
            ..super::SignIn::default()
        },
    );
    // Codex's login check names no account; its live plan reading does.
    let plan_usage = argonauts_testkit::services::MemoryPlanUsageBoard::default();
    let mut live = argonauts_core::plan_usage::LiveUsage::new(
        argonauts_core::names::AgentId("codex".to_owned()),
        argonauts_core::limits::PlanUsageSource::AppServer,
        time::OffsetDateTime::now_utc(),
    );
    live.account = Some("me@work.example".to_owned());
    live.plan = Some("Plus".to_owned());
    argonauts_core::limits::PlanUsageBoard::observe_live(&plan_usage, live);
    use argonauts_core::status_tab::{CliVersionCheck, VersionStanding};
    let checks = std::collections::BTreeMap::from([(
        "claude-code".to_owned(),
        CliVersionCheck {
            standing: VersionStanding::Newer,
            works: true,
            ..CliVersionCheck::default()
        },
    )]);
    let none = std::collections::BTreeSet::new();
    let clis = super::clis(
        &report,
        "claude-code",
        &sign_ins,
        &plan_usage,
        &checks,
        &none,
    );
    let names: Vec<&str> = clis.iter().map(|cli| cli.name.as_str()).collect();
    assert_eq!(
        names,
        ["Claude Code", "Codex", "OpenCode", "OpenCode (ACP)"]
    );
    assert_eq!(clis[0].command, "claude-code");
    assert_eq!(clis[3].providers.as_deref(), Some(&[][..]));
    assert!(clis[0].main);
    assert_eq!(clis[0].account.as_deref(), Some("you@example.com"));
    assert_eq!(clis[1].account.as_deref(), Some("me@work.example"));
    assert_eq!(clis[1].plan.as_deref(), Some("Plus"));
    assert_eq!(clis[0].check.state(), "newer than checked · works");
    assert_eq!(clis[1].check.state(), "unknown version");
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
        crate::controller::conversation::own_mcp_servers(&tools),
        ["github", "linear"]
    );
}

#[tokio::test]
async fn the_status_tab_counts_each_settings_file_and_says_how_the_folder_is_set_up() {
    let mut driver = TestDriver::start_with(CliOptions {
        prepare: Some(Box::new(|repo| {
            std::fs::create_dir_all(repo.home()).unwrap();
            std::fs::write(repo.global_config(), "workflow_size = \"large\"\n").unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    driver
        .send(UiCommand::Request {
            view: ViewRequest::StatusTab,
        })
        .await;
    let AppEvent::View { data } = driver.until(is_answer).await else {
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
    assert_eq!(view.main.as_ref().unwrap().worker.agent, "claude-code");
    // Doctor never ran here, so no CLI is listed and none was asked who it's signed in as.
    assert!(view.clis.is_empty());
    driver.exit().await;
}

/// Doctor says why a CLI's address isn't shown, by what was asked of it.
/// A CLI that keeps providers of its own never reads as an account whose address isn't
/// shown: until it answers nothing is said of it to Doctor, and an answer that couldn't be
/// read says so on the tab and to Doctor alike.
#[test]
fn a_cli_with_providers_of_its_own_is_never_an_unnamed_account() {
    use std::collections::{BTreeMap, BTreeSet};

    use argonauts_conductor::{AgentStatus, DoctorReport, GitCheck};

    let report = DoctorReport {
        agents: vec![check("opencode", "OpenCode", AgentStatus::Tested)],
        git: GitCheck {
            version: None,
            problem: None,
        },
    };
    let board = argonauts_testkit::services::FakeServices::new().plan_usage;
    let listing = BTreeSet::from(["opencode".to_owned()]);
    let row = |sign_ins: &BTreeMap<String, super::SignIn>| {
        super::clis(
            &report,
            "claude-code",
            sign_ins,
            board.as_ref(),
            &BTreeMap::new(),
            &listing,
        )
        .remove(0)
    };

    let asking = row(&BTreeMap::new());
    assert!(asking.lists_providers && !asking.providers_unread);
    assert_eq!(asking.providers, None);

    let unread = super::SignIn {
        providers_unread: true,
        ..super::SignIn::default()
    };
    let unread = row(&BTreeMap::from([("opencode".to_owned(), unread)]));
    assert!(unread.lists_providers && unread.providers_unread);
    assert_eq!(
        argonauts_core::status_tab::providers_unread(&unread.command),
        "couldn't list its providers · opencode auth list shows them"
    );
}

#[test]
fn each_unnamed_account_says_why() {
    assert!(super::unnamed_why("codex", true).starts_with("its login check doesn't print"));
    assert_eq!(
        super::unnamed_why("antigravity-cli", true),
        "its start-up screen was read and showed no address"
    );
    assert_eq!(
        super::unnamed_why("opencode", false),
        "it has no check that names the account"
    );
}

/// What agy's start-up screen said is kept as the tab shows it: the account, signed out, or a
/// folder it hasn't trusted. A screen that never said is one that was read all the same.
#[test]
fn each_thing_agys_screen_says_is_kept_apart() {
    use argonauts_core::adapter::BannerAccount;

    let signed_in = super::sign_in_shown(Some(BannerAccount::SignedIn {
        account: "you@example.com".to_owned(),
        plan: Some("Google AI Pro".to_owned()),
    }));
    assert_eq!(signed_in.account.as_deref(), Some("you@example.com"));
    assert_eq!(signed_in.plan.as_deref(), Some("Google AI Pro"));
    assert!(super::sign_in_shown(Some(BannerAccount::SignedOut)).signed_out);
    let untrusted = super::sign_in_shown(Some(BannerAccount::UntrustedFolder));
    assert!(untrusted.untrusted_folder && !untrusted.signed_out);
    assert_eq!(super::sign_in_shown(None), super::SignIn::default());
}

/// OpenCode is asked what it can use by its own commands, here a fake one that answers each
/// with the made-up lists, and its credentials file is never touched.
#[tokio::test]
async fn opencode_is_asked_for_its_providers_by_its_own_commands() {
    use std::os::unix::fs::PermissionsExt;

    use argonauts_core::status_tab::ProviderBilling;

    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../adapters/opencode/fixtures")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let opencode = dir.path().join("opencode");
    std::fs::write(
        &opencode,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{asked}'\ncase \"$*\" in\n  'auth list') cat \
             '{f}/providers_auth_list.stdout.txt' ;;\n  'models') cat \
             '{f}/providers_models.stdout.txt' ;;\n  'debug config') cat \
             '{f}/providers_config.stdout.json' ;;\n  *) exit 2 ;;\nesac\n",
            asked = dir.path().join("asked").display(),
            f = fixtures.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
    let adapter: std::sync::Arc<dyn argonauts_core::Adapter> =
        std::sync::Arc::new(argonauts_adapter_opencode::OpenCodeAdapter::new());
    let found = super::providers(&opencode, adapter).await.unwrap();
    let providers = found.providers.unwrap();
    let listed: Vec<(&str, ProviderBilling)> = providers
        .iter()
        .map(|provider| (provider.name.as_str(), provider.billing))
        .collect();
    assert_eq!(
        listed,
        [
            ("OpenRouter", ProviderBilling::ApiKey),
            ("Ollama", ProviderBilling::Local)
        ]
    );
    assert_eq!(found.default_model.as_deref(), Some("GLM 5 on OpenRouter"));
    assert!(found.account.is_none() && !found.signed_out);
    let asked = std::fs::read_to_string(dir.path().join("asked")).unwrap();
    assert_eq!(asked, "auth list\nmodels\ndebug config\n");
}
