//! The settings commands, through the real controller loop where they can be, and through a
//! context of their own where the loop can't reach: an agent session, and a run's start.

use std::sync::Arc;

use argonauts_core::Paths;
use argonauts_core::app::{
    AnswerTarget, AppChoice, AppEvent, ChoiceKind, SettingRow, UiCommand, ViewData, ViewRequest,
};
use argonauts_core::clock::SystemClock;
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::input::{
    AgentsAction, NarratorChange, PERMISSION_KEYS, ROLE_SANDBOX_KEYS, SlashCommand, WorkflowVerb,
};
use argonauts_core::model_picker::UltracodeToggle;
use argonauts_core::names::{ChoiceId, ModelRef, RoleName};
use argonauts_core::registry::{ModelEntry, ModelRegistry};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::steps::SchemaStepRunner;
use argonauts_testkit::services::StaticRegistry;
use argonauts_testkit::{FakeDirectory, TempRepo};
use tokio::sync::mpsc;

use super::*;
use crate::app_settings::AppSettings;
use crate::controller::test_support::{CliOptions, FakeConductor, SET_UP_GLOBAL, TestDriver};
use crate::controller::{AppServices, Internal, Outbox};
use crate::env::AppEnv;
use crate::trace::Trace;
use crate::trust::TrustStore;

/// A controller context without the loop, for what the loop can't set up.
pub(super) struct Fixture {
    pub(super) repo: TempRepo,
    pub(super) ctx: Ctx,
    pub(super) state: SettingsState,
    events: mpsc::UnboundedReceiver<AppEvent>,
    pub(super) internal: mpsc::UnboundedReceiver<Internal>,
}

impl Fixture {
    pub(super) fn new(agent_session: bool, prepare: impl FnOnce(&TempRepo)) -> Self {
        Self::with_terminal(agent_session, true, prepare)
    }

    fn with_terminal(
        agent_session: bool,
        interactive: bool,
        prepare: impl FnOnce(&TempRepo),
    ) -> Self {
        let repo = TempRepo::new().unwrap();
        prepare(&repo);
        let paths = Paths::new(repo.home());
        let mut env = AppEnv::for_repo(paths.clone(), repo.root(), interactive).unwrap();
        env.agent_session = agent_session;
        env.user_home = Some(repo.user_home().to_owned());
        let env = Arc::new(env);
        let flags = crate::controller::test_support::quiet_local_servers(Vec::new());
        let settings = AppSettings::load(&env, flags, interactive).unwrap();
        let (plan_usage, health) = argonauts_conductor::boards(&paths);
        let services = AppServices {
            conductor: Arc::new(FakeConductor::default()),
            approvals: None,
            workers: Arc::new(FakeDirectory::new()),
            steps: Arc::new(SchemaStepRunner),
            registry: crate::controller::test_support::quiet_registry(
                settings.settings(),
                repo.root(),
            ),
            plan_usage,
            health,
            clock: Arc::new(SystemClock),
        };
        let (sender, events) = mpsc::unbounded_channel();
        let out = Outbox::new(sender, Arc::new(Trace::off()));
        let (internal_sender, internal) = mpsc::unbounded_channel();
        let ctx = Ctx::new(env, settings, services, out, internal_sender);
        let state = SettingsState::new(&ctx);
        Self {
            repo,
            ctx,
            state,
            events,
            internal,
        }
    }

    pub(super) fn drain(&mut self) -> Vec<AppEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// The one choice among the events sent since the last look.
    fn only_choice(&mut self) -> AppChoice {
        let choices: Vec<AppChoice> = self
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::Choice { choice } => Some(choice),
                _ => None,
            })
            .collect();
        assert_eq!(choices.len(), 1, "{choices:#?}");
        choices.into_iter().next().unwrap()
    }
}

fn cli_options(prepare: impl FnOnce(&TempRepo) + 'static) -> CliOptions {
    CliOptions {
        prepare: Some(Box::new(prepare)),
        ..CliOptions::default()
    }
}

/// Your global file sending changes to `place`, a `save_changes_to` word.
fn saving_to(place: &'static str) -> impl FnOnce(&TempRepo) + 'static {
    move |repo| {
        let text = format!("save_changes_to = \"{place}\"\n{SET_UP_GLOBAL}");
        std::fs::write(repo.global_config(), text).unwrap();
    }
}

async fn slash(driver: &TestDriver, command: SlashCommand) {
    driver.send(UiCommand::Slash { command }).await;
}

async fn answer(driver: &TestDriver, id: &ChoiceId, action: AnswerAction, text: Option<&str>) {
    driver
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: id.clone() },
            action,
            text: text.map(str::to_owned),
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        })
        .await;
}

async fn next_choice(driver: &mut TestDriver, kind: ChoiceKind) -> AppChoice {
    let event = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == kind))
        .await;
    match event {
        AppEvent::Choice { choice } => choice,
        _ => unreachable!(),
    }
}

async fn next_view(driver: &mut TestDriver) -> ViewData {
    let event = driver
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await;
    match event {
        AppEvent::View { data } => *data,
        _ => unreachable!(),
    }
}

/// `(key, value, scope, previous, reset)` of the next `SettingChanged`.
async fn next_change(
    driver: &mut TestDriver,
) -> (String, String, SettingScope, Option<String>, bool) {
    let event = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { .. }))
        .await;
    match event {
        AppEvent::SettingChanged {
            key,
            value,
            scope,
            previous,
            reset,
            ..
        } => (key, value, scope, previous, reset),
        _ => unreachable!(),
    }
}

/// `(key, value, scope, why)` of the next `SettingChanged`.
async fn next_saved(driver: &mut TestDriver) -> (String, String, SettingScope, Option<String>) {
    let event = driver
        .until(|event| matches!(event, AppEvent::SettingChanged { .. }))
        .await;
    match event {
        AppEvent::SettingChanged {
            key,
            value,
            scope,
            why,
            ..
        } => (key, value, scope, why),
        _ => unreachable!(),
    }
}

async fn next_notice(driver: &mut TestDriver) -> String {
    let event = driver
        .until(|event| matches!(event, AppEvent::Notice { .. }))
        .await;
    match event {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
}

fn row<'a>(rows: &'a [SettingRow], key: &str) -> &'a SettingRow {
    rows.iter()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("no row {key} in {rows:#?}"))
}

fn state_dir(repo: &TempRepo) -> std::path::PathBuf {
    AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true)
        .unwrap()
        .state_dir
}

fn registry_with(entries: &[serde_json::Value]) -> Arc<dyn ModelRegistry> {
    let registry = StaticRegistry::default();
    for entry in entries {
        registry.add(serde_json::from_value::<ModelEntry>(entry.clone()).unwrap());
    }
    Arc::new(registry)
}

#[test]
fn the_fixture_asks_no_server_of_yours() {
    let fixture = Fixture::new(false, |_| {});
    assert!(!fixture.ctx.settings().registry.local_servers);
}

const GLOBAL_WITH_COMMENTS: &str = "# Mine, keep these notes.\n[ui]\nbell = true  # I like it\n";

#[tokio::test]
async fn each_command_opens_its_picker_with_its_default_scopes() {
    use SettingScope::*;
    let mut driver = TestDriver::start().await;

    slash(
        &driver,
        SlashCommand::Model {
            role: None,
            worker: None,
            effort: None,
        },
    )
    .await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut driver).await else {
        panic!("/model answers with rows");
    };
    assert_eq!(command.as_deref(), Some("model"));
    assert_eq!(row(&rows, "crew.main").default_scope, ProjectLocal);
    assert_eq!(row(&rows, "crew.main").value, "\"claude-code\"");
    assert_eq!(
        row(&rows, "roles.reviewer.models").default_scope,
        ProjectLocal
    );
    assert_eq!(row(&rows, "narrator.model").default_scope, Global);
    assert_eq!(row(&rows, "narrator.model").value, "");
    assert_eq!(row(&rows, "roles.judge.models").default_scope, ProjectLocal);

    slash(
        &driver,
        SlashCommand::Model {
            role: Some(RoleName::Reviewer),
            worker: None,
            effort: None,
        },
    )
    .await;
    let ViewData::Models { role, .. } = next_view(&mut driver).await else {
        panic!("/model reviewer lists models");
    };
    assert_eq!(role, Some(RoleName::Reviewer));

    slash(
        &driver,
        SlashCommand::Effort {
            role: None,
            level: None,
            ultracode: None,
        },
    )
    .await;
    let ViewData::Effort { slider } = next_view(&mut driver).await else {
        panic!("/effort answers with its slider");
    };
    assert_eq!(slider.role, None);
    assert_eq!(slider.worker, "claude-code".parse().unwrap());
    // The levels of the model Claude Code's default runs, each with what it means, starting
    // where that model starts with no level set: xhigh for Opus 5.5.
    let levels: Vec<&str> = slider
        .stops
        .iter()
        .map(|stop| stop.level.as_str())
        .collect();
    assert_eq!(levels, ["low", "medium", "high", "xhigh", "max"]);
    assert!(slider.stops.iter().all(|stop| !stop.description.is_empty()));
    assert_eq!(slider.level(), Some("xhigh"));
    assert!(slider.is_default() && !slider.cli_default);

    driver
        .send(UiCommand::Request {
            view: ViewRequest::Models { role: None },
        })
        .await;
    let ViewData::Models { choices, role, .. } = next_view(&mut driver).await else {
        panic!("the main agent's models");
    };
    assert_eq!(role, None);
    let current: Vec<String> = choices
        .iter()
        .filter(|choice| choice.current)
        .map(|choice| choice.worker.to_string())
        .collect();
    assert_eq!(current, vec!["claude-code".to_owned()], "{choices:#?}");

    slash(
        &driver,
        SlashCommand::Agents {
            action: AgentsAction::List,
        },
    )
    .await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut driver).await else {
        panic!("/agents answers with rows");
    };
    assert_eq!(command.as_deref(), Some("agents"));
    assert_eq!(row(&rows, "crew.worker").default_scope, Project);
    assert_eq!(row(&rows, "agents.opencode-acp.binary").layer, "built-in");
    assert!(
        !rows.iter().any(|row| row.key.starts_with("agents.hermes.")),
        "Hermes is off until a layer turns it on"
    );

    slash(
        &driver,
        SlashCommand::Workflow {
            name: None,
            action: WorkflowVerb::Use,
        },
    )
    .await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut driver).await else {
        panic!("/workflow answers with rows");
    };
    assert_eq!(command.as_deref(), Some("workflow"));
    let council = rows
        .iter()
        .find(|row| row.value == "\"council\"")
        .expect("the built-in council");
    assert_eq!(
        (council.key.as_str(), council.default_scope),
        ("workflow", Project)
    );
    assert!(rows.iter().any(|row| row.value == "\"orchestrator\""));

    // With no run on screen, /workflows opens the workflow view's Library tab alone.
    slash(&driver, SlashCommand::Workflows).await;
    let ViewData::WorkflowLibrary { workflows, refresh } = next_view(&mut driver).await else {
        panic!("/workflows answers with the library when no run is on screen");
    };
    for builtin in ["captain", "best-of-n", "draft-and-vote"] {
        let found = workflows
            .iter()
            .find(|file| file.name == builtin)
            .unwrap_or_else(|| panic!("the built-in {builtin}"));
        assert_eq!(found.source, "built in");
        assert!(!found.text.is_empty(), "{builtin} carries its text");
    }
    assert_eq!(workflows.iter().filter(|file| file.in_use).count(), 1);
    assert!(!refresh);

    // `/approvals` also sends the approval policy's view, which panics until the policy is
    // built, so its rows are checked on their own.
    let fixture = Fixture::new(false, |_| {});
    let rows = rows::rows(&fixture.ctx.settings, Some("approvals"), None);
    assert!(rows.iter().all(|row| row.key.starts_with("approvals.")));
    assert_eq!(row(&rows, "approvals.merge.mode").default_scope, Project);
    assert_eq!(row(&rows, "approvals.decides").default_scope, Project);
    // Autopilot's settings are all there with their values: the switch, who answers, the
    // second judge, and what a hard limit does at each approval.
    assert_eq!(row(&rows, "approvals.autopilot").value, "false");
    assert!(row(&rows, "approvals.answers").allowed.is_empty());
    assert!(row(&rows, "approvals.judge.second_model").value.is_empty());
    let merge = row(&rows, "approvals.merge.at_hard_limit");
    assert_eq!(merge.value, "\"stop\"");
    assert!(merge.allowed.contains(&"\"open_pull_request\"".to_owned()));
    let plan = row(&rows, "approvals.plan.at_hard_limit");
    assert!(!plan.allowed.contains(&"\"open_pull_request\"".to_owned()));
    assert_eq!(
        row(&rows, "approvals.rules.plan.max_score_spread").value,
        "1.0"
    );

    slash(
        &driver,
        SlashCommand::Limits {
            key: None,
            value: None,
            action: Default::default(),
        },
    )
    .await;
    let ViewData::Settings { rows, .. } = next_view(&mut driver).await else {
        panic!("/limits answers with rows");
    };
    assert!(rows.iter().all(|row| row.key.starts_with("limits.")));
    assert_eq!(row(&rows, "limits.spend_per_run").default_scope, Project);

    slash(
        &driver,
        SlashCommand::Fallback {
            role: Some(RoleName::Reviewer),
            key: None,
            value: None,
        },
    )
    .await;
    let ViewData::Settings { rows, section, .. } = next_view(&mut driver).await else {
        panic!("/fallback answers with rows");
    };
    assert_eq!(section.as_deref(), Some("fallback.reviewer"));
    assert_eq!(row(&rows, "fallback.reviewer.mode").default_scope, Project);

    slash(
        &driver,
        SlashCommand::Permissions {
            role: None,
            key: None,
            value: None,
        },
    )
    .await;
    let ViewData::Settings { rows, .. } = next_view(&mut driver).await else {
        panic!("/permissions answers with rows");
    };
    let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
    // The keys a role can set main agent every agent's sandbox, the rest of it follows, then the
    // limits, then each role's own sandbox.
    assert_eq!(&keys[..4], &PERMISSION_KEYS[..4]);
    assert!(keys.contains(&"sandbox.fail_closed"), "{keys:?}");
    assert!(keys.contains(&"sandbox.proxy_allow"), "{keys:?}");
    for key in PERMISSION_KEYS {
        assert!(keys.contains(key), "{key} is missing: {keys:?}");
    }
    for role in super::rows::ROLES {
        for leaf in ROLE_SANDBOX_KEYS {
            let key = format!("roles.{}.sandbox.{leaf}", role.as_str());
            assert!(keys.contains(&key.as_str()), "{key} is missing");
        }
    }
    assert_eq!(row(&rows, "sandbox.mode").default_scope, Project);
    assert!(row(&rows, "sandbox.mode").safety);
    assert_eq!(row(&rows, "sandbox.mode").value, "\"os\"");
    assert_eq!(row(&rows, "sandbox.network").value, "true");
    assert_eq!(
        row(&rows, "sandbox.agents_see_other_worktrees").value,
        "false"
    );
    assert_eq!(row(&rows, "sandbox.agents_commit").value, "true");
    assert_eq!(row(&rows, "limits.spend_per_day").default_scope, Global);
    // What a direct turn asks you, and where Captain works, are here too.
    assert_eq!(row(&rows, "captain.asks").value, "\"like_cli\"");
    assert!(row(&rows, "captain.asks").safety);
    assert_eq!(row(&rows, "captain.kind").value, "\"direct\"");
    // A role's own key is unset until you set it, and takes the same values.
    let own = row(&rows, "roles.reviewer.sandbox.mode");
    assert_eq!((own.value.as_str(), own.default_scope), ("", Project));
    assert!(own.safety);
    assert_eq!(
        own.allowed,
        ["\"os\"", "\"container\"", "\"tool\"", "\"none\""]
    );

    // A role first shows only that role's sandbox.
    slash(
        &driver,
        SlashCommand::Permissions {
            role: Some(RoleName::Reviewer),
            key: None,
            value: None,
        },
    )
    .await;
    let ViewData::Settings { rows, section, .. } = next_view(&mut driver).await else {
        panic!("/permissions reviewer answers with rows");
    };
    assert_eq!(section.as_deref(), Some("roles.reviewer.sandbox"));
    // With every agent's values, so the role's unset keys can say what they follow.
    let keys: Vec<String> = rows.iter().map(|row| row.key.clone()).collect();
    let mut want: Vec<String> = ROLE_SANDBOX_KEYS
        .iter()
        .map(|leaf| format!("sandbox.{leaf}"))
        .collect();
    want.extend(
        ROLE_SANDBOX_KEYS
            .iter()
            .map(|leaf| format!("roles.reviewer.sandbox.{leaf}")),
    );
    assert_eq!(keys, want);

    slash(&driver, SlashCommand::Narrator { change: None }).await;
    let ViewData::Settings { rows, .. } = next_view(&mut driver).await else {
        panic!("/narrator answers with rows");
    };
    assert!(rows.iter().all(|row| row.default_scope == Global));
    assert_eq!(rows.len(), 4);

    slash(&driver, SlashCommand::Theme { name: None }).await;
    let ViewData::Settings { rows, .. } = next_view(&mut driver).await else {
        panic!("/theme answers with rows");
    };
    assert_eq!(row(&rows, "ui.theme").default_scope, Global);

    slash(
        &driver,
        SlashCommand::Config {
            key: None,
            value: None,
            reset: false,
        },
    )
    .await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut driver).await else {
        panic!("/config answers with rows");
    };
    assert_eq!(command.as_deref(), Some("config"));
    assert_eq!(row(&rows, "roles.main.effort").default_scope, ProjectLocal);
    assert_eq!(row(&rows, "ui.keys").default_scope, Global);
    assert_eq!(row(&rows, "ui.keys").layer, "built-in");

    driver
        .send(UiCommand::Request {
            view: ViewRequest::Settings {
                section: Some("ui".to_owned()),
            },
        })
        .await;
    let ViewData::Settings {
        rows,
        command,
        section,
        ..
    } = next_view(&mut driver).await
    else {
        panic!("a request answers with rows");
    };
    assert_eq!((command, section.as_deref()), (None, Some("ui")));
    assert!(rows.iter().all(|row| row.key.starts_with("ui.")));
    driver.exit().await;
}

#[tokio::test]
async fn each_command_argument_saves_at_once_where_your_changes_go_keeping_comments() {
    let codex: ModelRef = "codex/gpt-5.6".parse().unwrap();
    // The command, the key it sets, and the value saved.
    let cases: Vec<(SlashCommand, &str, &str)> = vec![
        (
            SlashCommand::Model {
                role: Some(RoleName::Reviewer),
                worker: Some(codex.clone()),
                effort: None,
            },
            "roles.reviewer.models",
            "[\"codex/gpt-5.6\"]",
        ),
        (
            SlashCommand::Effort {
                role: None,
                level: Some("high".to_owned()),
                ultracode: None,
            },
            "roles.main.effort",
            "\"high\"",
        ),
        (
            SlashCommand::Model {
                role: None,
                worker: Some("codex".parse().unwrap()),
                effort: None,
            },
            "crew.main",
            "\"codex\"",
        ),
        (
            SlashCommand::Approvals {
                key: Some("merge.mode".to_owned()),
                value: Some("auto_preview".to_owned()),
            },
            "approvals.merge.mode",
            "\"auto_preview\"",
        ),
        (
            SlashCommand::Limits {
                key: Some("spend_per_run".to_owned()),
                value: Some("5.0".to_owned()),
                action: Default::default(),
            },
            "limits.spend_per_run",
            "5.0",
        ),
        (
            SlashCommand::Fallback {
                role: Some(RoleName::Reviewer),
                key: Some("mode".to_owned()),
                value: Some("wait".to_owned()),
            },
            "fallback.reviewer.mode",
            "\"wait\"",
        ),
        (
            SlashCommand::Fallback {
                role: None,
                key: Some("context".to_owned()),
                value: Some("note+summary".to_owned()),
            },
            "fallback.context",
            "\"note+summary\"",
        ),
        (
            SlashCommand::Fallback {
                role: Some(RoleName::Reviewer),
                key: Some("summarizer".to_owned()),
                value: Some("claude-code/haiku".to_owned()),
            },
            "fallback.reviewer.summarizer",
            "\"claude-code/haiku\"",
        ),
        (
            SlashCommand::Permissions {
                role: None,
                key: Some("sandbox.network".to_owned()),
                value: Some("true".to_owned()),
            },
            "sandbox.network",
            "true",
        ),
        (
            SlashCommand::Permissions {
                role: Some(RoleName::Worker),
                key: Some("mode".to_owned()),
                value: Some("tool".to_owned()),
            },
            "roles.worker.sandbox.mode",
            "\"tool\"",
        ),
        (
            SlashCommand::Permissions {
                role: None,
                key: Some("sandbox.fail_closed".to_owned()),
                value: Some("false".to_owned()),
            },
            "sandbox.fail_closed",
            "false",
        ),
        (
            SlashCommand::Narrator {
                change: Some(NarratorChange::Mode {
                    mode: NarratorMode::Live,
                }),
            },
            "ui.narrator",
            "\"live\"",
        ),
        (
            SlashCommand::Theme {
                name: Some("dark".to_owned()),
            },
            "ui.theme",
            "\"dark\"",
        ),
    ];
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        std::fs::write(repo.global_config(), GLOBAL_WITH_COMMENTS).unwrap();
    }))
    .await;
    let global = driver.repo.global_config();
    for (command, key, value) in cases {
        slash(&driver, command).await;
        let safety = !argonauts_registry::safety_keys_in(key, value).is_empty();
        if safety {
            let confirm = next_choice(&mut driver, ChoiceKind::ConfirmSafetySetting).await;
            assert!(
                confirm.body[0].starts_with(&format!("{key}: now ")),
                "{confirm:#?}"
            );
            answer(&driver, &confirm.id, AnswerAction::Approve, None).await;
        }
        let (changed, saved, saved_scope, _, reset) = next_change(&mut driver).await;
        assert_eq!(
            (changed.as_str(), saved.as_str(), saved_scope, reset),
            (key, value, SettingScope::Global, false)
        );
        let text = std::fs::read_to_string(&global).unwrap();
        assert!(text.contains("# Mine, keep these notes.\n[ui]\n"), "{text}");
        assert!(text.contains("bell = true  # I like it"), "{text}");
    }
    driver.exit().await;
}

/// A registry whose catalog has Claude Code's default running Sonnet 5.5, which starts at
/// high.
struct DefaultRunsSonnet(StaticRegistry);

impl ModelRegistry for DefaultRunsSonnet {
    fn get(&self, worker: &ModelRef) -> Option<ModelEntry> {
        self.0.get(worker)
    }
    fn provider(&self, worker: &ModelRef) -> String {
        self.0.provider(worker)
    }
    fn list(&self) -> Vec<ModelEntry> {
        self.0.list()
    }
    fn estimate(
        &self,
        worker: &ModelRef,
        input: u64,
        output: u64,
    ) -> argonauts_core::limits::CostEstimate {
        self.0.estimate(worker, input, output)
    }
    fn discover(&self) -> futures::future::BoxFuture<'_, Vec<ModelEntry>> {
        self.0.discover()
    }
    fn catalog(&self) -> argonauts_core::model_catalog::ModelCatalog {
        use argonauts_core::model_catalog::{CATALOG_VERSION, CatalogEntry, ModelCatalog};
        let mut default = CatalogEntry::new("claude-code".parse().unwrap());
        default.runs = Some("claude-sonnet-5-5".to_owned());
        let mut sonnet = CatalogEntry::new("claude-code/claude-sonnet-5-5".parse().unwrap());
        sonnet.efforts = Some(
            ["low", "medium", "high", "xhigh", "max"]
                .map(str::to_owned)
                .to_vec(),
        );
        sonnet.default_effort = Some("high".to_owned());
        ModelCatalog {
            version: CATALOG_VERSION,
            models: vec![default, sonnet],
            ..ModelCatalog::default()
        }
    }
}

/// With no effort set, `/effort` on a CLI's default starts where the model it runs starts:
/// high for Sonnet 5.5.
#[tokio::test]
async fn the_effort_slider_starts_where_the_clis_default_model_does() {
    let mut driver = TestDriver::start_with(CliOptions {
        registry: Some(Arc::new(DefaultRunsSonnet(StaticRegistry::default()))),
        ..CliOptions::default()
    })
    .await;
    slash(
        &driver,
        SlashCommand::Effort {
            role: None,
            level: None,
            ultracode: None,
        },
    )
    .await;
    let ViewData::Effort { slider } = next_view(&mut driver).await else {
        panic!("/effort answers with its slider");
    };
    assert_eq!(slider.worker, "claude-code".parse().unwrap());
    assert_eq!(slider.level(), Some("high"));
    assert!(slider.is_default() && !slider.cli_default);
    driver.exit().await;
}

#[tokio::test]
async fn an_effort_level_the_model_lacks_is_refused_naming_the_ones_it_takes() {
    let registry = registry_with(&[serde_json::json!({
        "id": "claude-code/haiku",
        "provider": "anthropic",
        "kind": "cli",
        "access": "subscription",
        "cost": { "unit": "plan_usage" },
        "efforts": ["low", "medium", "high"],
    })]);
    let mut driver = TestDriver::start_with(CliOptions {
        registry: Some(registry),
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(repo.global_config(), "crew.main = \"claude-code/haiku\"\n").unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    slash(
        &driver,
        SlashCommand::Effort {
            role: None,
            level: Some("max".to_owned()),
            ultracode: None,
        },
    )
    .await;
    let refusal = next_notice(&mut driver).await;
    assert_eq!(
        refusal,
        "claude-code/haiku doesn't take max effort; it takes low, medium, high"
    );

    slash(
        &driver,
        SlashCommand::Effort {
            role: None,
            level: None,
            ultracode: None,
        },
    )
    .await;
    let ViewData::Effort { slider } = next_view(&mut driver).await else {
        panic!("/effort answers with its slider");
    };
    let levels: Vec<&str> = slider
        .stops
        .iter()
        .map(|stop| stop.level.as_str())
        .collect();
    assert_eq!(levels, ["low", "medium", "high"]);
    assert_eq!(slider.worker, "claude-code/haiku".parse().unwrap());
    driver.exit().await;
}

/// agy's level-named models take their own level only, so another is refused with why.
#[tokio::test]
async fn another_level_for_a_model_fixed_at_one_is_refused_saying_so() {
    let registry = registry_with(&[serde_json::json!({
        "id": "antigravity-cli/gemini-3.8-flash-high",
        "provider": "google",
        "kind": "cli",
        "access": "subscription",
        "cost": { "unit": "plan_usage" },
        "efforts": ["high"],
        "default_effort": "high",
    })]);
    let mut driver = TestDriver::start_with(CliOptions {
        registry: Some(registry),
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(
                repo.global_config(),
                "crew.main = \"antigravity-cli/gemini-3.8-flash-high\"\n",
            )
            .unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    slash(
        &driver,
        SlashCommand::Effort {
            role: None,
            level: Some("low".to_owned()),
            ultracode: None,
        },
    )
    .await;
    assert_eq!(
        next_notice(&mut driver).await,
        "antigravity-cli/gemini-3.8-flash-high takes only high effort, fixed by the model"
    );
    driver.exit().await;
}

#[tokio::test]
async fn a_model_picked_with_an_effort_saves_both_at_once() {
    let registry = registry_with(&[serde_json::json!({
        "id": "claude-code/haiku",
        "provider": "anthropic",
        "kind": "cli",
        "access": "subscription",
        "cost": { "unit": "plan_usage" },
        "efforts": ["low", "medium", "high"],
    })]);
    let mut driver = TestDriver::start_with(CliOptions {
        registry: Some(registry),
        ..CliOptions::default()
    })
    .await;
    let haiku: ModelRef = "claude-code/haiku".parse().unwrap();
    let pick = |effort: &str| SlashCommand::Model {
        role: None,
        worker: Some(haiku.clone()),
        effort: Some(effort.to_owned()),
    };
    slash(&driver, pick("max")).await;
    assert_eq!(
        next_notice(&mut driver).await,
        "claude-code/haiku doesn't take max effort; it takes low, medium, high"
    );

    slash(&driver, pick("high")).await;
    let mut saved = Vec::new();
    for _ in 0..2 {
        let (key, value, scope, _, _) = next_change(&mut driver).await;
        assert_eq!(scope, SettingScope::Global);
        saved.push((key, value));
    }
    saved.sort();
    assert_eq!(
        saved,
        [
            ("crew.main".to_owned(), "\"claude-code/haiku\"".to_owned()),
            ("roles.main.effort".to_owned(), "\"high\"".to_owned()),
        ]
    );
    let global = std::fs::read_to_string(driver.repo.global_config()).unwrap();
    // The key is written under its help line, as the catalog words it.
    let help = argonauts_registry::setting_meta("crew.main")
        .expect("a catalog entry")
        .description;
    assert!(
        global.contains(&format!("[crew]\n# {help}\nmain = \"claude-code/haiku\"")),
        "{global}"
    );
    assert!(global.contains("effort = \"high\""), "{global}");

    // The models level starts each model's effort line on the level now set.
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Models { role: None },
        })
        .await;
    let ViewData::Models { choices, .. } = next_view(&mut driver).await else {
        panic!("the main agent's models");
    };
    assert!(
        choices
            .iter()
            .all(|choice| choice.effort.as_deref() == Some("high")),
        "{choices:#?}"
    );
    driver.exit().await;
}

/// `/effort`'s Ultracode toggle. The main agent's slider offers it as it is now and a role's
/// doesn't. The toggle alone saves at once and writes only `ultracode`, which the status then
/// carries for the input rule; with a level, both are saved together. `/config` lists it.
#[tokio::test]
async fn ultracode_from_the_effort_slider_saves_at_once() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        let reviewer = "[roles.reviewer]\nmodels = [\"claude-code\"]\n";
        std::fs::write(repo.global_config(), reviewer).unwrap();
    }))
    .await;
    let effort = |role: Option<RoleName>, level: Option<&str>, ultracode: Option<bool>| {
        SlashCommand::Effort {
            role,
            level: level.map(str::to_owned),
            ultracode,
        }
    };
    slash(&driver, effort(None, None, None)).await;
    let ViewData::Effort { slider } = next_view(&mut driver).await else {
        panic!("/effort answers with its slider");
    };
    assert_eq!(slider.ultracode, Some(UltracodeToggle::new(false)));
    slash(&driver, effort(Some(RoleName::Reviewer), None, None)).await;
    let ViewData::Effort { slider } = next_view(&mut driver).await else {
        panic!("/effort reviewer answers with its slider");
    };
    assert_eq!(slider.ultracode, None, "a role's slider has no toggle");

    slash(&driver, effort(None, None, Some(true))).await;
    let (key, value, scope, _, _) = next_change(&mut driver).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("ultracode", "true", SettingScope::Global)
    );
    let event = driver
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    let AppEvent::Status { status } = event else {
        unreachable!()
    };
    assert!(status.ultracode, "the input rule's label shows it");
    let global = std::fs::read_to_string(driver.repo.global_config()).unwrap();
    assert!(global.contains("ultracode = true"), "{global}");
    assert!(!global.contains("effort"), "{global}");

    slash(&driver, effort(None, None, None)).await;
    let ViewData::Effort { slider } = next_view(&mut driver).await else {
        panic!("/effort answers with its slider");
    };
    assert_eq!(slider.ultracode, Some(UltracodeToggle::new(true)));

    slash(&driver, effort(None, Some("high"), Some(false))).await;
    let mut saved = Vec::new();
    for _ in 0..2 {
        let (key, value, _, _, _) = next_change(&mut driver).await;
        saved.push((key, value));
    }
    saved.sort();
    assert_eq!(
        saved,
        [
            ("roles.main.effort".to_owned(), "\"high\"".to_owned()),
            ("ultracode".to_owned(), "false".to_owned()),
        ]
    );
    driver.exit().await;
}

#[tokio::test]
async fn config_lists_ultracode_off_by_default() {
    let fixture = Fixture::new(false, |_| {});
    let rows = rows::rows(&fixture.ctx.settings, Some("config"), None);
    let ultracode = row(&rows, "ultracode");
    assert_eq!(
        (ultracode.value.as_str(), ultracode.layer.as_str()),
        ("false", "built-in")
    );
    assert_eq!(ultracode.allowed, ["true", "false"]);
    assert!(
        ultracode
            .description
            .contains("a workflow for every real request"),
        "{}",
        ultracode.description
    );
}

#[tokio::test]
async fn a_reset_falls_back_a_layer() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.toml", "[ui]\ntheme = \"light\"\n")
            .unwrap();
        repo.write(
            ".argonauts/config.local.toml",
            "# just me\n[ui]\ntheme = \"dark\"\nmotion = \"reduced\"\n",
        )
        .unwrap();
    }))
    .await;
    slash(
        &driver,
        SlashCommand::Config {
            key: Some("ui.theme".to_owned()),
            value: None,
            reset: true,
        },
    )
    .await;
    let (key, value, scope, previous, reset) = next_change(&mut driver).await;
    assert_eq!(
        (
            key.as_str(),
            value.as_str(),
            scope,
            previous.as_deref(),
            reset
        ),
        (
            "ui.theme",
            "\"light\"",
            SettingScope::ProjectLocal,
            Some("\"dark\""),
            true
        )
    );
    let local =
        std::fs::read_to_string(driver.repo.root().join(".argonauts/config.local.toml")).unwrap();
    assert!(!local.contains("theme"), "{local}");
    assert!(
        local.contains("# just me") && local.contains("motion"),
        "{local}"
    );
    let ui = driver
        .until(|event| matches!(event, AppEvent::UiSettings { .. }))
        .await;
    let AppEvent::UiSettings { ui: Some(ui), .. } = ui else {
        panic!("the whole ui section");
    };
    assert_eq!(
        toml::Value::try_from(ui.theme).unwrap().to_string(),
        "\"light\""
    );

    // A picker's Reset row names its layer; the shared file's value goes too, to the built-in.
    driver
        .send(UiCommand::ResetSetting {
            key: "ui.theme".to_owned(),
            scope: SettingScope::Project,
        })
        .await;
    let (_, value, scope, _, reset) = next_change(&mut driver).await;
    assert_eq!(
        (value.as_str(), scope, reset),
        ("\"auto\"", SettingScope::Project, true)
    );

    // Nothing sets it now but the built-in.
    slash(
        &driver,
        SlashCommand::Config {
            key: Some("ui.theme".to_owned()),
            value: None,
            reset: true,
        },
    )
    .await;
    assert_eq!(
        next_notice(&mut driver).await,
        "ui.theme is already the built-in default."
    );
    driver.exit().await;
}

#[tokio::test]
async fn vim_is_saved_globally() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        std::fs::write(repo.global_config(), GLOBAL_WITH_COMMENTS).unwrap();
    }))
    .await;
    slash(&driver, SlashCommand::Vim).await;
    let (key, value, scope, _, _) = next_change(&mut driver).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("ui.keys", "\"vim\"", SettingScope::Global)
    );
    let AppEvent::UiSettings { vim, .. } = driver
        .until(|event| matches!(event, AppEvent::UiSettings { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(vim);
    let text = std::fs::read_to_string(driver.repo.global_config()).unwrap();
    assert!(
        text.contains("keys = \"vim\"") && text.contains("# I like it"),
        "{text}"
    );

    // `/vim` again turns it off.
    slash(&driver, SlashCommand::Vim).await;
    let (_, value, _, previous, _) = next_change(&mut driver).await;
    assert_eq!(
        (value.as_str(), previous.as_deref()),
        ("\"default\"", Some("\"vim\""))
    );

    // `/verbosity quiet`, then `/verbosity normal`.
    let level = |level| SlashCommand::Verbosity { level: Some(level) };
    slash(&driver, level(Verbosity::Quiet)).await;
    let (key, value, ..) = next_change(&mut driver).await;
    assert_eq!(
        (key.as_str(), value.as_str()),
        ("ui.verbosity", "\"quiet\"")
    );
    slash(&driver, level(Verbosity::Normal)).await;
    let (_, value, ..) = next_change(&mut driver).await;
    assert_eq!(value, "\"normal\"");
    driver.exit().await;
}

/// The team's file would hide a change saved everywhere, so it saves where it takes effect,
/// in your own file for this project, and the line says why. The team's file isn't touched.
#[tokio::test]
async fn vim_saves_just_for_you_when_the_team_file_sets_it() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.toml", "[ui]\nkeys = \"default\"\n")
            .unwrap();
    }))
    .await;
    slash(&driver, SlashCommand::Vim).await;
    let (key, value, scope, why) = next_saved(&mut driver).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("ui.keys", "\"vim\"", SettingScope::ProjectLocal)
    );
    assert_eq!(why.as_deref(), Some("since the team's file sets it"));
    assert_eq!(
        std::fs::read_to_string(driver.repo.global_config()).unwrap(),
        SET_UP_GLOBAL
    );
    let shared = driver.repo.root().join(".argonauts/config.toml");
    assert_eq!(
        std::fs::read_to_string(shared).unwrap(),
        "[ui]\nkeys = \"default\"\n"
    );

    // Your own file holds it now, and the next toggle goes there too.
    slash(&driver, SlashCommand::Vim).await;
    let (_, value, scope, why) = next_saved(&mut driver).await;
    assert_eq!(
        (value.as_str(), scope),
        ("\"default\"", SettingScope::ProjectLocal)
    );
    assert_eq!(why.as_deref(), Some("since it's set there already"));
    driver.exit().await;
}

#[tokio::test]
async fn write_to_untrusted_project_file_keeps_it_untrusted() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        let just_you = format!("save_changes_to = \"just_you\"\n{SET_UP_GLOBAL}");
        std::fs::write(repo.global_config(), just_you).unwrap();
        repo.write(
            ".argonauts/config.local.toml",
            "[sandbox]\nmode = \"container\"\n",
        )
        .unwrap();
    }))
    .await;
    slash(
        &driver,
        SlashCommand::Effort {
            role: None,
            level: Some("high".to_owned()),
            ultracode: None,
        },
    )
    .await;
    // The question at start comes first; the one about this write says the change is saved.
    let event = driver
        .until(|event| {
            matches!(event, AppEvent::Choice { choice }
                if choice.kind == ChoiceKind::TrustProject
                    && choice.body.iter().any(|line| line.contains("saved either way")))
        })
        .await;
    let AppEvent::Choice { choice: trust } = event else {
        unreachable!()
    };
    assert!(
        trust
            .body
            .iter()
            .any(|line| line.trim() == "sandbox.mode = \"container\""),
        "{trust:#?}"
    );
    answer(&driver, &trust.id, AnswerAction::Cancel, None).await;
    let (key, value, scope, ..) = next_change(&mut driver).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("roles.main.effort", "\"high\"", SettingScope::ProjectLocal)
    );

    let file = driver.repo.root().join(".argonauts/config.local.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("mode = \"container\"") && text.contains("effort = \"high\""),
        "{text}"
    );
    assert!(!TrustStore::load(&state_dir(&driver.repo)).is_trusted(&file));
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Config,
        })
        .await;
    let ViewData::Config { entries } = next_view(&mut driver).await else {
        panic!("/config's plain list");
    };
    let mode = entries
        .iter()
        .find(|entry| entry.key == "sandbox.mode")
        .unwrap();
    assert_ne!(mode.value, "\"container\"");
    assert_eq!(mode.layer, "built-in");
    let effort = entries
        .iter()
        .find(|entry| entry.key == "roles.main.effort")
        .unwrap();
    assert_eq!(effort.layer, "project-local");
    driver.exit().await;
}

async fn next_closed(driver: &mut TestDriver, id: &ChoiceId) -> String {
    let event = driver
        .until(|event| matches!(event, AppEvent::ChoiceClosed { id: closed, .. } if closed == id))
        .await;
    match event {
        AppEvent::ChoiceClosed { outcome, .. } => outcome,
        _ => unreachable!(),
    }
}

async fn sandbox_mode(driver: &mut TestDriver) -> (String, String) {
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Config,
        })
        .await;
    let ViewData::Config { entries } = next_view(driver).await else {
        panic!("/config's plain list");
    };
    let mode = entries
        .into_iter()
        .find(|entry| entry.key == "sandbox.mode")
        .unwrap();
    (mode.value, mode.layer)
}

const UNSAFE_PROJECT: &str = "[sandbox]\nmode = \"container\"\n\n[roles.main]\neffort = \"low\"\n";

#[tokio::test]
async fn untrusted_safety_keys_are_asked_about_at_start() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.toml", UNSAFE_PROJECT)
            .unwrap();
    }))
    .await;
    let trust = next_choice(&mut driver, ChoiceKind::TrustProject).await;
    assert_eq!(trust.title, "Trust .argonauts/config.toml?");
    assert!(
        trust
            .body
            .iter()
            .any(|line| line.trim() == "sandbox.mode = \"container\""),
        "{trust:#?}"
    );
    answer(&driver, &trust.id, AnswerAction::Cancel, None).await;
    let outcome = next_closed(&mut driver, &trust.id).await;
    assert!(outcome.contains("stays untrusted"), "{outcome}");
    let file = driver.repo.root().join(".argonauts/config.toml");
    assert!(!TrustStore::load(&state_dir(&driver.repo)).is_trusted(&file));
    assert_eq!(sandbox_mode(&mut driver).await.1, "built-in");
    driver.exit().await;

    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.toml", UNSAFE_PROJECT)
            .unwrap();
    }))
    .await;
    let trust = next_choice(&mut driver, ChoiceKind::TrustProject).await;
    answer(&driver, &trust.id, AnswerAction::Approve, None).await;
    let outcome = next_closed(&mut driver, &trust.id).await;
    assert_eq!(outcome, "Trusted .argonauts/config.toml.");
    let file = driver.repo.root().join(".argonauts/config.toml");
    assert!(TrustStore::load(&state_dir(&driver.repo)).is_trusted(&file));
    assert_eq!(
        sandbox_mode(&mut driver).await,
        ("\"container\"".to_owned(), "project".to_owned())
    );
    driver.exit().await;
}

#[tokio::test]
async fn a_file_changed_after_the_start_question_stays_untrusted() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.local.toml", UNSAFE_PROJECT)
            .unwrap();
    }))
    .await;
    let trust = next_choice(&mut driver, ChoiceKind::TrustProject).await;
    let file = driver.repo.root().join(".argonauts/config.local.toml");
    std::fs::write(&file, "[sandbox]\nmode = \"container\"\nnetwork = true\n").unwrap();
    answer(&driver, &trust.id, AnswerAction::Approve, None).await;
    let outcome = next_closed(&mut driver, &trust.id).await;
    assert!(
        outcome.contains("changed since you were asked"),
        "{outcome}"
    );
    assert!(!TrustStore::load(&state_dir(&driver.repo)).is_trusted(&file));
    driver.exit().await;
}

#[tokio::test]
async fn files_without_untrusted_safety_keys_ask_nothing_at_start() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.toml", "[roles.main]\neffort = \"low\"\n")
            .unwrap();
    }))
    .await;
    driver
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    let events = driver.exit().await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "{events:#?}"
    );
}

/// The file the decision was made from: a check written as a string, beside a safety key.
const CHECK_AS_STRING: &str =
    "[limits]\nspend_per_run = 1.0\n\n[checks]\nrun = [\"python3 -m unittest -q\"]\n";

#[tokio::test]
async fn a_file_that_needs_a_fix_says_what_to_fix_instead_of_asking_for_trust() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.local.toml", CHECK_AS_STRING)
            .unwrap();
    }))
    .await;
    driver
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    let file = driver.repo.root().join(".argonauts/config.local.toml");
    assert!(!TrustStore::load(&state_dir(&driver.repo)).is_trusted(&file));
    let events = driver.exit().await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "{events:#?}"
    );
    let notices: Vec<(NoteLevel, &str)> = events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { level, text } => Some((*level, text.as_str())),
            _ => None,
        })
        .collect();
    let fix = ".argonauts/config.local.toml needs a fix before you can trust it: checks.run \
               can't be a list of strings; it takes a list of checks, each a [[checks.run]] table \
               with a name and a command. Write it like this:\n\
               [[checks.run]]\n\
               name = \"python3\"\n\
               command = [\"python3\", \"-m\", \"unittest\", \"-q\"]";
    assert!(notices.contains(&(NoteLevel::Warn, fix)), "{notices:#?}");
    assert!(
        notices
            .iter()
            .all(|(_, text)| !text.contains("stays untrusted") && !text.contains("invalid type")),
        "{notices:#?}"
    );
}

#[tokio::test]
async fn a_yes_that_cannot_resolve_keeps_nothing_trusted_and_says_why_once() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.toml", UNSAFE_PROJECT)
            .unwrap();
    }))
    .await;
    let trust = next_choice(&mut driver, ChoiceKind::TrustProject).await;
    // Your global file breaks while the question is open.
    let global = driver.repo.global_config();
    std::fs::write(&global, "[ui\n").unwrap();
    answer(&driver, &trust.id, AnswerAction::Approve, None).await;
    let outcome = next_closed(&mut driver, &trust.id).await;
    assert_eq!(outcome, NOT_CHANGED);
    let notice = next_notice(&mut driver).await;
    assert_eq!(
        notice,
        format!(
            ".argonauts/config.toml can't be trusted until {} is fixed: line 1 isn't valid TOML \
             (unclosed table, expected `]`): [ui",
            global.display()
        )
    );
    let file = driver.repo.root().join(".argonauts/config.toml");
    assert!(!TrustStore::load(&state_dir(&driver.repo)).is_trusted(&file));
    std::fs::remove_file(&global).unwrap();
    let events = driver.exit().await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AppEvent::Notice { text, .. } if text.contains("stays untrusted")
        )),
        "{events:#?}"
    );
}

#[tokio::test]
async fn a_trusted_or_new_project_file_stays_trusted_after_a_write() {
    let mut driver = TestDriver::start_with(cli_options(saving_to("this_project"))).await;
    slash(
        &driver,
        SlashCommand::Theme {
            name: Some("dark".to_owned()),
        },
    )
    .await;
    next_change(&mut driver).await;
    let file = driver.repo.root().join(".argonauts/config.toml");
    assert!(TrustStore::load(&state_dir(&driver.repo)).is_trusted(&file));

    // A confirmed safety key lands in the file you trusted and applies.
    slash(
        &driver,
        SlashCommand::Permissions {
            role: None,
            key: Some("sandbox.network".to_owned()),
            value: Some("true".to_owned()),
        },
    )
    .await;
    let confirm = next_choice(&mut driver, ChoiceKind::ConfirmSafetySetting).await;
    answer(&driver, &confirm.id, AnswerAction::Approve, None).await;
    let (key, value, ..) = next_change(&mut driver).await;
    assert_eq!((key.as_str(), value.as_str()), ("sandbox.network", "true"));
    assert!(TrustStore::load(&state_dir(&driver.repo)).is_trusted(&file));
    driver.exit().await;
}

#[tokio::test]
async fn a_safety_confirm_declined_changes_nothing() {
    let mut driver = TestDriver::start().await;
    slash(
        &driver,
        SlashCommand::Config {
            key: Some("approvals.merge.mode".to_owned()),
            value: Some("auto".to_owned()),
            reset: false,
        },
    )
    .await;
    let confirm = next_choice(&mut driver, ChoiceKind::ConfirmSafetySetting).await;
    assert!(
        confirm.body[0].starts_with("approvals.merge.mode: now "),
        "{confirm:#?}"
    );
    answer(&driver, &confirm.id, AnswerAction::Cancel, None).await;
    let closed = driver
        .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if *id == confirm.id))
        .await;
    assert!(matches!(closed, AppEvent::ChoiceClosed { outcome, .. } if outcome == "Not changed."));
    assert_eq!(
        std::fs::read_to_string(driver.repo.global_config()).unwrap(),
        SET_UP_GLOBAL
    );
    driver.exit().await;
}

/// A flag puts one change where it says: for this session, in a file the team's file still
/// wins over (and the line says so), and never a key only your global file holds elsewhere.
#[tokio::test]
async fn a_flag_saves_one_change_where_it_says() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        repo.write(".argonauts/config.toml", "[ui]\ntheme = \"light\"\n")
            .unwrap();
    }))
    .await;
    let saved_in = |scope, change| SlashCommand::SavedIn {
        scope,
        change: Box::new(change),
    };
    let effort = |level: &str| SlashCommand::Effort {
        role: None,
        level: Some(level.to_owned()),
        ultracode: None,
    };
    slash(&driver, effort("high")).await;
    next_saved(&mut driver).await;
    slash(&driver, saved_in(SettingScope::Session, effort("max"))).await;
    let (key, value, scope, previous, _) = next_change(&mut driver).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope, previous.as_deref()),
        (
            "roles.main.effort",
            "\"max\"",
            SettingScope::Session,
            Some("\"high\"")
        )
    );
    let global = std::fs::read_to_string(driver.repo.global_config()).unwrap();
    assert!(global.contains("effort = \"high\""), "{global}");

    // A plain change after it replaces this session's, in the file.
    slash(&driver, effort("low")).await;
    let (_, value, scope, _) = next_saved(&mut driver).await;
    assert_eq!((value.as_str(), scope), ("\"low\"", SettingScope::Global));
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Config,
        })
        .await;
    let ViewData::Config { entries } = next_view(&mut driver).await else {
        panic!("/config's plain list");
    };
    let effort_now = entries
        .iter()
        .find(|entry| entry.key == "roles.main.effort")
        .unwrap();
    assert_eq!(
        (effort_now.value.as_str(), effort_now.layer.as_str()),
        ("\"low\"", "global")
    );

    let theme = SlashCommand::Theme {
        name: Some("dark".to_owned()),
    };
    slash(&driver, saved_in(SettingScope::Global, theme)).await;
    let (_, _, scope, why) = next_saved(&mut driver).await;
    assert_eq!(scope, SettingScope::Global);
    assert_eq!(why.as_deref(), Some("though the team's file wins here"));

    let place = SlashCommand::Config {
        key: Some("save_changes_to".to_owned()),
        value: Some("this_project".to_owned()),
        reset: false,
    };
    slash(&driver, saved_in(SettingScope::ProjectLocal, place)).await;
    let refused = driver
        .until(|event| matches!(event, AppEvent::SettingRefused { .. }))
        .await;
    let AppEvent::SettingRefused { why, .. } = refused else {
        unreachable!()
    };
    assert!(why.contains("only in your global file"), "{why}");
    driver.exit().await;
}

/// A change the team's file would hide saves just for you, a `--set` flag's for this session,
/// and each line says why.
#[tokio::test]
async fn a_change_a_layer_above_would_hide_saves_where_it_takes_effect() {
    let mut driver = TestDriver::start_with(CliOptions {
        flags: vec!["ui.tips=false".to_owned()],
        ..cli_options(|repo| {
            repo.write(".argonauts/config.toml", "[ui]\ntheme = \"light\"\n")
                .unwrap();
        })
    })
    .await;
    let config = |key: &str, value: &str| SlashCommand::Config {
        key: Some(key.to_owned()),
        value: Some(value.to_owned()),
        reset: false,
    };
    slash(&driver, config("ui.theme", "dark")).await;
    let (key, _, scope, why) = next_saved(&mut driver).await;
    assert_eq!(
        (key.as_str(), scope),
        ("ui.theme", SettingScope::ProjectLocal)
    );
    assert_eq!(why.as_deref(), Some("since the team's file sets it"));
    let shared = std::fs::read_to_string(driver.repo.root().join(".argonauts/config.toml"));
    assert_eq!(shared.unwrap(), "[ui]\ntheme = \"light\"\n");

    slash(&driver, config("ui.tips", "true")).await;
    let (key, _, scope, why) = next_saved(&mut driver).await;
    assert_eq!((key.as_str(), scope), ("ui.tips", SettingScope::Session));
    assert_eq!(why.as_deref(), Some("since a --set flag sets it"));
    driver.exit().await;
}

/// With no settings file naming a workflow, Council runs on its own built-in settings, and
/// Config's Workflow row says so.
#[test]
fn the_workflow_row_names_council_when_nothing_names_a_workflow() {
    let fixture = Fixture::new(false, |_| {});
    let row = super::rows::row(
        &fixture.ctx.settings,
        "workflow".to_owned(),
        SettingScope::Global,
    );
    assert_eq!(row.value, "\"council\"");
}

/// Each Config row reads a key the settings have, so no row hides for a misspelled key.
#[test]
fn every_config_row_names_a_real_key() {
    for key in argonauts_tui::overview::config::row_keys() {
        assert!(
            argonauts_registry::setting_meta(key).is_some(),
            "Config names {key}, which no setting is"
        );
    }
}

#[test]
fn the_orchestrator_pick_carries_the_workflow() {
    let worker: ModelRef = "codex/gpt-5.6".parse().unwrap();
    assert_eq!(
        orchestrator_change(&worker),
        Change::Set(vec![
            ("workflow".to_owned(), "\"orchestrator\"".to_owned()),
            (
                "roles.orchestrator.models".to_owned(),
                "[\"codex/gpt-5.6\"]".to_owned()
            ),
        ])
    );
}

#[tokio::test]
async fn set_setting_safety_key_refused_in_agent_session() {
    let mut fixture = Fixture::new(true, |repo| {
        std::fs::write(repo.global_config(), "[sandbox]\nnetwork = true\n").unwrap();
    });
    let refused = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "sandbox.network".to_owned(),
        "false".to_owned(),
        Some(SettingScope::Global),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        refused.contains("agent session") && refused.contains("sandbox.network"),
        "{refused}"
    );

    let refused = reset(
        &mut fixture.ctx,
        &mut fixture.state,
        "sandbox.network".to_owned(),
        SettingScope::Global,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(refused.contains("agent session"), "{refused}");

    // A table value that holds a safety key counts too.
    let refused = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "endpoints.ollama".to_owned(),
        "{ base_url = \"http://evil\" }".to_owned(),
        Some(SettingScope::Session),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(refused.contains("endpoints.ollama.base_url"), "{refused}");
    let text = std::fs::read_to_string(fixture.repo.global_config()).unwrap();
    assert_eq!(text, "[sandbox]\nnetwork = true\n");
    assert!(
        fixture
            .drain()
            .iter()
            .all(|event| !matches!(event, AppEvent::SettingChanged { .. }))
    );

    // Keys that aren't safety keys still change.
    let effects = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "ui.theme".to_owned(),
        "\"dark\"".to_owned(),
        Some(SettingScope::Session),
    )
    .await
    .unwrap();
    assert!(matches!(
        effects.last(),
        Some(Effect::SettingsApplied { .. })
    ));
}

/// The plain app, with no terminal, refuses every safety change: an argument, a picker's
/// pick, `/config`, and a reset.
#[tokio::test]
async fn safety_changes_are_refused_without_a_terminal() {
    let mut driver = TestDriver::start_with(CliOptions {
        interactive: false,
        ..cli_options(|repo| {
            std::fs::write(repo.global_config(), "[sandbox]\nnetwork = true\n").unwrap();
        })
    })
    .await;
    // A typed `/config key=value` says it as the command's own answer, the rest as a notice.
    let refused = |event: &AppEvent| {
        matches!(
            event,
            AppEvent::Notice { level: NoteLevel::Error, text }
                | AppEvent::SettingRefused { why: text, .. }
                if text.contains("needs you at a terminal")
        )
    };
    let refusal = async |driver: &mut TestDriver| match driver.until(refused).await {
        AppEvent::Notice { text, .. } | AppEvent::SettingRefused { why: text, .. } => text,
        _ => unreachable!(),
    };

    slash(
        &driver,
        SlashCommand::Permissions {
            role: None,
            key: Some("sandbox.network".to_owned()),
            value: Some("false".to_owned()),
        },
    )
    .await;
    assert!(refusal(&mut driver).await.contains("sandbox.network"));
    driver
        .send(UiCommand::SetSetting {
            key: "sandbox.network".to_owned(),
            value: "false".to_owned(),
            scope: Some(SettingScope::Session),
        })
        .await;
    assert!(refusal(&mut driver).await.contains("sandbox.network"));
    slash(
        &driver,
        SlashCommand::Config {
            key: Some("approvals.merge.mode".to_owned()),
            value: Some("auto".to_owned()),
            reset: false,
        },
    )
    .await;
    assert!(refusal(&mut driver).await.contains("approvals.merge.mode"));
    driver
        .send(UiCommand::ResetSetting {
            key: "sandbox.network".to_owned(),
            scope: SettingScope::Global,
        })
        .await;
    assert!(refusal(&mut driver).await.contains("sandbox.network"));

    // Keys that aren't safety keys still change.
    slash(
        &driver,
        SlashCommand::Theme {
            name: Some("dark".to_owned()),
        },
    )
    .await;
    let (key, ..) = next_change(&mut driver).await;
    assert_eq!(key, "ui.theme");
    let global = std::fs::read_to_string(driver.repo.global_config()).unwrap();
    assert!(global.contains("[sandbox]\nnetwork = true\n"), "{global}");
    let events = driver.exit().await;
    assert!(
        !events.iter().any(|event| match event {
            AppEvent::SettingChanged { key, .. } => key != "ui.theme",
            AppEvent::Choice { choice } => choice.kind == ChoiceKind::ConfirmSafetySetting,
            _ => false,
        }),
        "{events:#?}"
    );
}

#[tokio::test]
async fn checks_are_not_offered_without_a_terminal() {
    let mut fixture = Fixture::with_terminal(false, false, |repo| {
        repo.write("Cargo.toml", "[package]\nname = \"x\"\n")
            .unwrap();
    });
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    assert!(
        fixture
            .drain()
            .iter()
            .all(|event| !matches!(event, AppEvent::Choice { .. }))
    );
}

/// On autopilot the checks found in the folder aren't offered: one line names them, once, and
/// the question is still there for a run started off autopilot.
#[tokio::test]
async fn checks_are_named_and_not_offered_on_autopilot() {
    let mut fixture = Fixture::new(false, |repo| {
        repo.write("Cargo.toml", "[package]\nname = \"x\"\n")
            .unwrap();
    });
    assert!(follow_mode(&mut fixture.state, true), "autopilot came on");
    for _ in 0..2 {
        offer_checks(&mut fixture.ctx, &mut fixture.state)
            .await
            .unwrap();
    }
    let said: Vec<String> = fixture
        .drain()
        .into_iter()
        .map(|event| match event {
            AppEvent::Notice { text, .. } => text,
            other => panic!("only a line is said: {other:?}"),
        })
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with(CHECKS_ON_AUTOPILOT), "{said:?}");
    assert!(said[0].contains("cargo test"), "{said:?}");

    assert!(!follow_mode(&mut fixture.state, false));
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    assert_eq!(fixture.only_choice().kind, ChoiceKind::DetectedChecks);
    // Coming on is seen each time, and staying on isn't.
    assert!(follow_mode(&mut fixture.state, true));
    assert!(!follow_mode(&mut fixture.state, true));
}

/// The offer open when autopilot comes on closes, since saving check commands is yours to
/// confirm, and a run started off autopilot is offered them again.
#[tokio::test]
async fn autopilot_coming_on_closes_the_checks_offer_for_a_later_run() {
    let mut fixture = Fixture::new(false, |repo| {
        repo.write("Cargo.toml", "[package]\nname = \"x\"\n")
            .unwrap();
    });
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    let offer = fixture.only_choice();
    settle_on_autopilot(&fixture.ctx, &mut fixture.state);
    let closed = fixture.drain();
    assert!(
        matches!(closed.as_slice(), [AppEvent::ChoiceClosed { id, outcome }]
            if id == &offer.id && outcome.starts_with("Decided on autopilot · not saved")),
        "{closed:?}"
    );
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    assert_eq!(fixture.only_choice().kind, ChoiceKind::DetectedChecks);
}

#[tokio::test]
async fn checks_are_offered_once() {
    let mut fixture = Fixture::new(false, |repo| {
        repo.write("Cargo.toml", "[package]\nname = \"x\"\n")
            .unwrap();
    });
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    let offer = fixture.only_choice();
    assert_eq!(offer.kind, ChoiceKind::DetectedChecks);
    assert!(offer.body.iter().any(|line| line.trim() == "cargo test"));
    assert!(
        offer
            .body
            .iter()
            .any(|line| line.contains("safety setting"))
    );
    // Not again while it's open.
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    assert!(fixture.drain().is_empty());

    // Esc leaves it for the next run.
    set_aside(&mut fixture.ctx, &mut fixture.state, offer.id.clone()).unwrap();
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    let offer = fixture.only_choice();

    // Edited, the list goes to the shared project file, which is new and so trusted.
    answer_choice(
        &mut fixture.ctx,
        &mut fixture.state,
        offer.id.clone(),
        AnswerAction::ApproveWithEdits,
        Some("cargo test --workspace\n".to_owned()),
    )
    .await
    .unwrap();
    let file = fixture.repo.root().join(".argonauts/config.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"--workspace\""), "{text}");
    assert_eq!(fixture.ctx.settings().checks.run.len(), 1);
    assert_eq!(
        fixture.ctx.settings().checks.run[0].command,
        vec!["cargo", "test", "--workspace"]
    );
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    assert!(
        fixture
            .drain()
            .iter()
            .all(|event| !matches!(event, AppEvent::Choice { .. }))
    );
}

#[tokio::test]
async fn skipping_the_checks_offer_is_remembered_in_the_repo() {
    let mut fixture = Fixture::new(false, |repo| {
        repo.write("pyproject.toml", "").unwrap();
    });
    offer_checks(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    let offer = fixture.only_choice();
    answer_choice(
        &mut fixture.ctx,
        &mut fixture.state,
        offer.id,
        AnswerAction::Cancel,
        None,
    )
    .await
    .unwrap();
    // A new app session in the same repo doesn't ask again.
    let mut state = SettingsState::new(&fixture.ctx);
    offer_checks(&mut fixture.ctx, &mut state).await.unwrap();
    assert!(
        fixture
            .drain()
            .iter()
            .all(|event| !matches!(event, AppEvent::Choice { .. }))
    );
}

#[tokio::test]
async fn a_session_value_joins_the_app_and_later_runs() {
    let mut fixture = Fixture::new(false, |_| {});
    let effects = narrator_mode(&mut fixture.ctx, &mut fixture.state, NarratorMode::Live)
        .await
        .unwrap();
    assert!(matches!(
        effects.last(),
        Some(Effect::SettingsApplied { .. })
    ));
    assert_eq!(fixture.ctx.settings().ui.narrator, NarratorMode::Live);
    assert_eq!(fixture.ctx.settings.layer("ui.narrator"), "session");
    // Past the flag every test app starts with.
    assert_eq!(
        fixture.ctx.settings.run_overrides(),
        [
            "registry.local_servers=false".to_owned(),
            "ui.narrator=\"live\"".to_owned()
        ]
    );
    let events = fixture.drain();
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::SettingChanged {
            scope: SettingScope::Session,
            ..
        }
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppEvent::UiSettings { ui: Some(_), .. }))
    );

    // A picker's Reset row for the session drops the override.
    reset(
        &mut fixture.ctx,
        &mut fixture.state,
        "ui.narrator".to_owned(),
        SettingScope::Session,
    )
    .await
    .unwrap();
    assert_eq!(fixture.ctx.settings.layer("ui.narrator"), "built-in");
}

#[tokio::test]
async fn a_new_main_agent_starts_a_fresh_conversation() {
    let mut fixture = Fixture::new(false, |_| {});
    let effects = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "crew.main".to_owned(),
        "\"codex\"".to_owned(),
        Some(SettingScope::Session),
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.first(), Some(Effect::FreshConversation)),
        "{effects:?}"
    );
}

#[tokio::test]
async fn a_value_that_doesnt_resolve_is_put_back() {
    let mut fixture = Fixture::new(false, |repo| {
        repo.write(".argonauts/config.toml", "# team\n").unwrap();
    });
    let file = fixture.repo.root().join(".argonauts/config.toml");
    let err = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "limits.soft_stop".to_owned(),
        "\"lots\"".to_owned(),
        Some(SettingScope::Project),
    )
    .await
    .unwrap_err();
    assert!(!err.to_string().is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# team\n");
    assert_eq!(fixture.ctx.settings.trust().recorded(&file), None);
    assert!(fixture.drain().is_empty());
}

#[tokio::test]
async fn a_key_the_catalog_lacks_is_kept_only_when_resolve_knows_it() {
    let mut fixture = Fixture::new(false, |_| {});
    let file = fixture.repo.root().join(".argonauts/config.toml");
    let err = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "fallback.strategi".to_owned(),
        "spread".to_owned(),
        Some(SettingScope::Project),
    )
    .await
    .unwrap_err();
    // The resolver names the key and the one you likely meant.
    let err = err.to_string();
    assert!(
        err.contains("fallback.strategi isn't a setting") && err.contains("fallback.strategy"),
        "{err}"
    );
    assert!(!file.exists());
    let err = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "fallback.strategi".to_owned(),
        "spread".to_owned(),
        Some(SettingScope::Session),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("isn't a setting"), "{err}");
    assert!(fixture.ctx.settings.session_overrides().is_empty());

    set(
        &mut fixture.ctx,
        &mut fixture.state,
        "fallback.strategy".to_owned(),
        "spread".to_owned(),
        Some(SettingScope::Project),
    )
    .await
    .unwrap();
    assert_eq!(
        fixture
            .ctx
            .settings
            .entry("fallback.strategy")
            .unwrap()
            .value,
        "\"spread\""
    );
    let rows = rows::rows(&fixture.ctx.settings, Some("fallback"), None);
    let strategy = row(&rows, "fallback.strategy");
    assert_eq!(strategy.layer, "project");
    assert_eq!(strategy.allowed, ["\"first\"", "\"spread\""]);
    // Several at once is `together`, and `pick` says how their results combine.
    let pick = row(&rows, "fallback.pick");
    assert_eq!(
        pick.allowed,
        ["\"first_passing\"", "\"judge\"", "\"merge\""]
    );
    assert!(rows.iter().any(|row| row.key == "fallback.together"));
    assert!(rows.iter().any(|row| row.key == "fallback.reviewer.pick"));
}

/// Sets `key` in `scope` through the same path as a pick with a place named on purpose.
async fn set_at(fixture: &mut Fixture, key: &str, value: &str, scope: SettingScope) {
    set(
        &mut fixture.ctx,
        &mut fixture.state,
        key.to_owned(),
        value.to_owned(),
        Some(scope),
    )
    .await
    .unwrap();
}

fn keys_of(rows: &[SettingRow]) -> Vec<&str> {
    rows.iter().map(|row| row.key.as_str()).collect()
}

#[tokio::test]
async fn fallback_shows_what_a_fallback_is_told_and_a_summarizer_once_it_needs_one() {
    let mut fixture = Fixture::new(false, |_| {});
    let fallback = |fixture: &Fixture, section: Option<&str>| {
        rows::rows(&fixture.ctx.settings, Some("fallback"), section)
    };

    let rows = fallback(&fixture, None);
    let context = row(&rows, "fallback.context");
    assert_eq!(context.value, "\"note+transcript\"");
    assert_eq!(
        context.allowed,
        [
            "\"fresh\"",
            "\"note\"",
            "\"note+transcript\"",
            "\"note+summary\""
        ]
    );
    assert!(
        context.description.starts_with("What the fallback is told"),
        "{}",
        context.description
    );
    assert_eq!(context.default_scope, SettingScope::Project);
    assert!(keys_of(&rows).contains(&"fallback.main.context"));
    assert!(
        !keys_of(&rows)
            .iter()
            .any(|key| key.ends_with(".summarizer")),
        "{:?}",
        keys_of(&rows)
    );

    // Picking note+summary brings the summarizer in, for every role without its own.
    set_at(
        &mut fixture,
        "fallback.context",
        "note+summary",
        SettingScope::Project,
    )
    .await;
    let rows = fallback(&fixture, None);
    let summarizer = row(&rows, "fallback.summarizer");
    assert_eq!(summarizer.value, "");
    assert!(summarizer.allowed.is_empty());
    assert_eq!(summarizer.default_scope, SettingScope::Project);
    assert!(keys_of(&rows).contains(&"fallback.main.summarizer"));
    let reviewer = fallback(&fixture, Some("fallback.reviewer"));
    assert!(keys_of(&reviewer).contains(&"fallback.reviewer.summarizer"));
    assert!(keys_of(&reviewer).contains(&"fallback.reviewer.context"));

    // One role told note+summary needs its own, and the top-level one that serves it.
    set_at(
        &mut fixture,
        "fallback.context",
        "note",
        SettingScope::Project,
    )
    .await;
    set_at(
        &mut fixture,
        "fallback.reviewer.context",
        "note+summary",
        SettingScope::Project,
    )
    .await;
    let reviewer = fallback(&fixture, Some("fallback.reviewer"));
    assert!(keys_of(&reviewer).contains(&"fallback.reviewer.summarizer"));
    let rows = fallback(&fixture, None);
    assert!(keys_of(&rows).contains(&"fallback.summarizer"));
    assert!(!keys_of(&rows).contains(&"fallback.main.summarizer"));

    // So does an agent's fallbacks being told note+summary.
    set_at(
        &mut fixture,
        "fallback.reviewer.context",
        "fresh",
        SettingScope::Project,
    )
    .await;
    let rows = fallback(&fixture, None);
    assert!(!keys_of(&rows).contains(&"fallback.summarizer"));
    set_at(
        &mut fixture,
        "fallback.replace.claude-code",
        "{ with = [\"codex/gpt-5.6\"], context = \"note+summary\" }",
        SettingScope::Project,
    )
    .await;
    let rows = fallback(&fixture, None);
    assert!(keys_of(&rows).contains(&"fallback.summarizer"));

    // A summarizer you set stays listed, so you can change or reset it.
    set_at(
        &mut fixture,
        "fallback.replace.claude-code.context",
        "note",
        SettingScope::Project,
    )
    .await;
    set_at(
        &mut fixture,
        "fallback.summarizer",
        "claude-code/haiku",
        SettingScope::Project,
    )
    .await;
    let rows = fallback(&fixture, None);
    let summarizer = row(&rows, "fallback.summarizer");
    assert_eq!(
        (summarizer.value.as_str(), summarizer.layer.as_str()),
        ("\"claude-code/haiku\"", "project")
    );
    let file = std::fs::read_to_string(fixture.repo.root().join(".argonauts/config.toml")).unwrap();
    assert!(
        file.contains("summarizer = \"claude-code/haiku\""),
        "{file}"
    );
    assert!(file.contains("context = \"note\""), "{file}");
}

#[tokio::test]
async fn the_fallback_context_saves_at_each_scope() {
    use SettingScope::*;
    let mut fixture = Fixture::new(false, |_| {});
    let layer = |fixture: &Fixture, key: &str| {
        let rows = rows::rows(&fixture.ctx.settings, Some("fallback"), None);
        let row = row(&rows, key);
        (row.value.clone(), row.layer.clone())
    };
    assert_eq!(
        layer(&fixture, "fallback.context"),
        ("\"note+transcript\"".to_owned(), "built-in".to_owned())
    );
    let cases = [
        (Global, "fresh", "global"),
        (Project, "note", "project"),
        (ProjectLocal, "note+summary", "project-local"),
        (Session, "note+transcript", "session"),
    ];
    for (scope, value, expected) in cases {
        set_at(&mut fixture, "fallback.context", value, scope).await;
        assert_eq!(
            layer(&fixture, "fallback.context"),
            (format!("\"{value}\""), expected.to_owned()),
            "{scope:?}"
        );
    }
    let global = std::fs::read_to_string(fixture.repo.global_config()).unwrap();
    assert!(global.contains("context = \"fresh\""), "{global}");
    let local =
        std::fs::read_to_string(fixture.repo.root().join(".argonauts/config.local.toml")).unwrap();
    assert!(local.contains("context = \"note+summary\""), "{local}");

    // A role's own, and the summarizer, the same way.
    set_at(&mut fixture, "fallback.reviewer.context", "fresh", Project).await;
    set_at(
        &mut fixture,
        "fallback.summarizer",
        "codex/gpt-5.6",
        Session,
    )
    .await;
    let rows = rows::rows(
        &fixture.ctx.settings,
        Some("fallback"),
        Some("fallback.reviewer"),
    );
    let context = row(&rows, "fallback.reviewer.context");
    assert_eq!(
        (context.value.as_str(), context.layer.as_str()),
        ("\"fresh\"", "project")
    );
    assert_eq!(
        layer(&fixture, "fallback.summarizer"),
        ("\"codex/gpt-5.6\"".to_owned(), "session".to_owned())
    );

    // A value the picker doesn't offer is refused, naming the four.
    let err = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "fallback.context".to_owned(),
        "summary".to_owned(),
        Some(Project),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("\"note+summary\"") && err.contains("\"fresh\""),
        "{err}"
    );
}

#[tokio::test]
async fn config_lists_the_fallback_context_and_summarizer_with_their_layers() {
    let mut fixture = Fixture::new(false, |_| {});
    let config = |fixture: &Fixture| rows::rows(&fixture.ctx.settings, Some("config"), None);
    let rows = config(&fixture);
    let context = row(&rows, "fallback.context");
    assert_eq!(
        (context.value.as_str(), context.layer.as_str()),
        ("\"note+transcript\"", "built-in")
    );
    let summarizer = row(&rows, "fallback.summarizer");
    assert_eq!(
        (summarizer.value.as_str(), summarizer.layer.as_str()),
        ("", "built-in")
    );
    assert!(!summarizer.description.is_empty());

    set_at(
        &mut fixture,
        "fallback.summarizer",
        "claude-code/haiku",
        SettingScope::ProjectLocal,
    )
    .await;
    set_at(
        &mut fixture,
        "fallback.worker.context",
        "fresh",
        SettingScope::Project,
    )
    .await;
    let rows = config(&fixture);
    let summarizer = row(&rows, "fallback.summarizer");
    assert_eq!(
        (summarizer.value.as_str(), summarizer.layer.as_str()),
        ("\"claude-code/haiku\"", "project-local")
    );
    let worker = row(&rows, "fallback.worker.context");
    assert_eq!(
        (worker.value.as_str(), worker.layer.as_str()),
        ("\"fresh\"", "project")
    );
    assert!(keys_of(&rows).contains(&"fallback.worker.summarizer"));
    // `/config fallback` is the section on its own.
    let section = rows::rows(&fixture.ctx.settings, Some("config"), Some("fallback"));
    assert!(keys_of(&section).contains(&"fallback.context"));
    assert!(keys_of(&section).contains(&"fallback.summarizer"));
}

/// `/agents` names each CLI it can't find, with what installs it, and none it can.
#[tokio::test]
async fn agents_lists_the_clis_that_are_not_installed_with_how_to_add_them() {
    let present = std::env::current_exe().unwrap();
    let mut flags: Vec<String> = ["claude-code", "antigravity-cli", "opencode"]
        .iter()
        .map(|id| format!(r#"agents.{id}.binary="/nowhere/{id}""#))
        .collect();
    flags.push(format!(r#"agents.codex.binary="{}""#, present.display()));
    let mut driver = TestDriver::start_with(CliOptions {
        flags,
        ..CliOptions::default()
    })
    .await;
    slash(
        &driver,
        SlashCommand::Agents {
            action: AgentsAction::List,
        },
    )
    .await;
    let ViewData::Settings { not_installed, .. } = next_view(&mut driver).await else {
        panic!("/agents answers with rows");
    };
    let ids: Vec<&str> = not_installed.iter().map(|cli| cli.id.as_str()).collect();
    for id in ["claude-code", "antigravity-cli", "opencode"] {
        assert!(ids.contains(&id), "{id}: {not_installed:?}");
    }
    assert!(!ids.contains(&"codex"), "{not_installed:?}");
    let opencode = not_installed
        .iter()
        .find(|cli| cli.id == "opencode")
        .unwrap();
    assert!(
        opencode
            .install
            .as_deref()
            .is_some_and(|install| install.contains("opencode")),
        "{opencode:?}"
    );

    // Other settings commands carry none.
    slash(
        &driver,
        SlashCommand::Workflow {
            name: None,
            action: WorkflowVerb::Use,
        },
    )
    .await;
    let ViewData::Settings { not_installed, .. } = next_view(&mut driver).await else {
        panic!("/workflow answers with rows");
    };
    assert!(not_installed.is_empty());
    driver.exit().await;
}

/// `/config key=value` saves a key of yours at once to your global file, as Claude Code's does,
/// with no question of where.
#[tokio::test]
async fn config_saves_a_key_of_yours_at_once() {
    let mut driver = TestDriver::start_with(cli_options(|repo| {
        std::fs::write(repo.global_config(), GLOBAL_WITH_COMMENTS).unwrap();
    }))
    .await;
    slash(
        &driver,
        SlashCommand::Config {
            key: Some("ui.motion".to_owned()),
            value: Some("reduced".to_owned()),
            reset: false,
        },
    )
    .await;
    let (changed, saved, scope, _, reset) = next_change(&mut driver).await;
    assert_eq!(
        (changed.as_str(), saved.as_str(), scope, reset),
        ("ui.motion", "\"reduced\"", SettingScope::Global, false)
    );
    let text = std::fs::read_to_string(driver.repo.global_config()).unwrap();
    assert!(text.contains("motion = \"reduced\""), "{text}");
}

/// `/config key=value` that can't be saved says why as the command's own answer, which the
/// screen prints under the command and the conversation's record keeps there, and saves
/// nothing.
#[tokio::test]
async fn config_answers_a_value_it_cant_save_with_why() {
    let mut driver = TestDriver::start_with(cli_options(|_| {})).await;
    for (key, value, why) in [
        (
            "ui.tips",
            "maybe",
            "ui.tips takes true, false, not \"maybe\"",
        ),
        (
            "approvals.start_mode",
            "sometimes",
            "approvals.start_mode takes ",
        ),
        (
            "no.such.key",
            "1",
            "no.such.key isn't a setting; /config lists them",
        ),
    ] {
        slash(
            &driver,
            SlashCommand::Config {
                key: Some(key.to_owned()),
                value: Some(value.to_owned()),
                reset: false,
            },
        )
        .await;
        let refused = driver
            .until(|event| matches!(event, AppEvent::SettingRefused { .. }))
            .await;
        let AppEvent::SettingRefused {
            key: said,
            why: text,
        } = refused
        else {
            unreachable!()
        };
        assert_eq!(said, key);
        assert!(text.starts_with(why), "{text}");
        let noticed = driver
            .seen()
            .iter()
            .any(|event| matches!(event, AppEvent::Notice { text: n, .. } if *n == text));
        assert!(!noticed, "said once, as the command's answer: {text}");
    }
    let text = std::fs::read_to_string(driver.repo.global_config()).unwrap();
    for left_out in ["maybe", "sometimes", "no.such", "[no]"] {
        assert!(!text.contains(left_out), "{text}");
    }
}

/// `/config all` lists every key in All settings, as `/config <key>` lists those under one.
#[tokio::test]
async fn config_all_lists_every_key() {
    let mut driver = TestDriver::start_with(cli_options(|_| {})).await;
    slash(
        &driver,
        SlashCommand::Config {
            key: Some("all".to_owned()),
            value: None,
            reset: false,
        },
    )
    .await;
    let ViewData::Settings {
        rows,
        command,
        section,
        ..
    } = next_view(&mut driver).await
    else {
        panic!("All settings");
    };
    assert_eq!((command.as_deref(), section), (Some("config"), None));
    assert!(rows.len() > 400, "{}", rows.len());
    assert!(rows.iter().any(|row| row.key == "approvals.start_mode"));
}

/// Once a deeper Behaviors map is made, the Config tab names the summarizer and the embedder
/// auto came to, read from the map itself; before, it names neither.
#[tokio::test]
async fn the_config_tab_names_what_the_behaviors_maps_auto_came_to() {
    use crate::behaviors::deep::prompt::PROMPT_VERSION;
    use crate::behaviors::deep::store::{Layout, MapMaker, Store};

    let mut driver = TestDriver::start().await;
    let behaviors = ViewRequest::Settings {
        section: Some("behaviors".to_owned()),
    };
    let picked = |rows: &[SettingRow], key: &str| row(rows, key).picked.clone();
    driver
        .send(UiCommand::Request {
            view: behaviors.clone(),
        })
        .await;
    let ViewData::Settings { rows, .. } = next_view(&mut driver).await else {
        panic!("a request answers with rows");
    };
    assert_eq!(picked(&rows, "behaviors.summarizer"), None);
    assert_eq!(picked(&rows, "behaviors.embedder"), None);

    let store = Store::new(&Paths::new(driver.repo.home()));
    store
        .save_layout(&Layout {
            maker: MapMaker {
                summarizer: "ollama/qwen3-coder:30b-64k".to_owned(),
                prompt: PROMPT_VERSION.to_owned(),
                embedder: "ollama/nomic-embed-text".to_owned(),
            },
            frame: None,
            points: Vec::new(),
        })
        .unwrap();
    driver.send(UiCommand::Request { view: behaviors }).await;
    let ViewData::Settings { rows, .. } = next_view(&mut driver).await else {
        panic!("a request answers with rows");
    };
    assert_eq!(
        picked(&rows, "behaviors.summarizer").as_deref(),
        Some("Qwen3 Coder 30B on Ollama")
    );
    assert_eq!(
        picked(&rows, "behaviors.embedder").as_deref(),
        Some("nomic-embed-text on Ollama")
    );
    assert_eq!(
        picked(&rows, "behaviors.map"),
        None,
        "a value that says itself"
    );
    driver.exit().await;
}
