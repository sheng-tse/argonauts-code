//! The settings commands, through the real controller loop where they can be, and through a
//! context of their own where the loop can't reach: an agent session, and a run's start.

use std::sync::Arc;

use argonauts_core::Paths;
use argonauts_core::app::{
    AnswerTarget, AppChoice, AppEvent, ChoiceKind, SettingRow, UiCommand, ViewData, ViewRequest,
};
use argonauts_core::clock::SystemClock;
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::input::{
    AgentsAction, NarratorChange, PERMISSION_KEYS, PresetAction, ROLE_SANDBOX_KEYS, SlashCommand,
};
use argonauts_core::model_picker::AllHandsToggle;
use argonauts_core::names::{ChoiceId, RoleName, WorkerRef};
use argonauts_core::registry::{ModelEntry, ModelRegistry};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::steps::SchemaStepRunner;
use argonauts_testkit::services::StaticRegistry;
use argonauts_testkit::{FakeDirectory, TempRepo};
use tokio::sync::mpsc;

use super::*;
use crate::app_settings::AppSettings;
use crate::controller::test_support::{FakeConductor, Harness, HarnessOptions};
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
        let settings = AppSettings::load(&env, Vec::new(), interactive).unwrap();
        let (quota, health) = argonauts_conductor::boards(&paths);
        let services = AppServices {
            conductor: Arc::new(FakeConductor::default()),
            approvals: None,
            workers: Arc::new(FakeDirectory::new()),
            steps: Arc::new(SchemaStepRunner),
            registry: crate::controller::test_support::quiet_registry(
                settings.settings(),
                repo.root(),
            ),
            quota,
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

fn harness_options(prepare: impl FnOnce(&TempRepo) + 'static) -> HarnessOptions {
    HarnessOptions {
        prepare: Some(Box::new(prepare)),
        ..HarnessOptions::default()
    }
}

async fn slash(harness: &Harness, command: SlashCommand) {
    harness.send(UiCommand::Slash { command }).await;
}

async fn answer(harness: &Harness, id: &ChoiceId, action: AnswerAction, text: Option<&str>) {
    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: id.clone() },
            action,
            text: text.map(str::to_owned),
            evidence_sha256: None,
            cards: Vec::new(),
            remember: false,
        })
        .await;
}

fn choose(scope: SettingScope) -> AnswerAction {
    choices::choose(choices::scope_value(scope))
}

async fn next_choice(harness: &mut Harness, kind: ChoiceKind) -> AppChoice {
    let event = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == kind))
        .await;
    match event {
        AppEvent::Choice { choice } => choice,
        _ => unreachable!(),
    }
}

async fn next_view(harness: &mut Harness) -> ViewData {
    let event = harness
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await;
    match event {
        AppEvent::View { data } => *data,
        _ => unreachable!(),
    }
}

/// `(key, value, scope, previous, reset)` of the next `SettingChanged`.
async fn next_change(
    harness: &mut Harness,
) -> (String, String, SettingScope, Option<String>, bool) {
    let event = harness
        .until(|event| matches!(event, AppEvent::SettingChanged { .. }))
        .await;
    match event {
        AppEvent::SettingChanged {
            key,
            value,
            scope,
            previous,
            reset,
        } => (key, value, scope, previous, reset),
        _ => unreachable!(),
    }
}

async fn next_notice(harness: &mut Harness) -> String {
    let event = harness
        .until(|event| matches!(event, AppEvent::Notice { .. }))
        .await;
    match event {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
}

fn recommended(choice: &AppChoice) -> AnswerAction {
    choice
        .options
        .iter()
        .find(|option| option.recommended)
        .map(|option| option.action.clone())
        .unwrap()
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

const GLOBAL_WITH_COMMENTS: &str = "# Mine, keep these notes.\n[ui]\nbell = true  # I like it\n";

/// `/style` picks how crew runs work, saved for this project, just you; `/style goal` sets it.
#[tokio::test]
async fn style_picks_workflow_or_goal_for_this_project() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Style { style: None }).await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut harness).await else {
        panic!("/style answers with its row");
    };
    assert_eq!(command.as_deref(), Some("style"));
    let style = row(&rows, "workflows.style");
    assert_eq!(style.value, "\"follow_lead\"");
    assert_eq!(style.default_scope, SettingScope::ProjectLocal);
    assert_eq!(rows.len(), 1);

    slash(
        &harness,
        SlashCommand::Style {
            style: Some("goal".into()),
        },
    )
    .await;
    // Picking a value asks where to save it, this project for you first.
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    assert_eq!(recommended(&question), choose(SettingScope::ProjectLocal));
    answer(
        &harness,
        &question.id,
        choose(SettingScope::ProjectLocal),
        None,
    )
    .await;
    let (key, value, scope, ..) = next_change(&mut harness).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("workflows.style", "\"goal\"", SettingScope::ProjectLocal)
    );
    let local =
        std::fs::read_to_string(harness.repo.root().join(".argonauts/config.local.toml")).unwrap();
    assert!(local.contains("style = \"goal\""), "{local}");
    harness.exit().await;
}

#[tokio::test]
async fn each_command_opens_its_picker_with_its_default_scopes() {
    use SettingScope::*;
    let mut harness = Harness::start().await;

    slash(
        &harness,
        SlashCommand::Model {
            role: None,
            worker: None,
            effort: None,
        },
    )
    .await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut harness).await else {
        panic!("/model answers with rows");
    };
    assert_eq!(command.as_deref(), Some("model"));
    assert_eq!(row(&rows, "default_agent").default_scope, ProjectLocal);
    assert_eq!(row(&rows, "default_agent").value, "\"claude-code\"");
    assert_eq!(
        row(&rows, "roles.reviewer.workers").default_scope,
        ProjectLocal
    );
    assert_eq!(row(&rows, "narrator.model").default_scope, Global);
    assert_eq!(row(&rows, "narrator.model").value, "");
    assert_eq!(
        row(&rows, "approvals.approver.model").default_scope,
        Project
    );

    slash(
        &harness,
        SlashCommand::Model {
            role: Some(RoleName::Reviewer),
            worker: None,
            effort: None,
        },
    )
    .await;
    let ViewData::Models { role, .. } = next_view(&mut harness).await else {
        panic!("/model reviewer lists models");
    };
    assert_eq!(role, Some(RoleName::Reviewer));

    slash(
        &harness,
        SlashCommand::Effort {
            role: None,
            level: None,
            all_hands: None,
        },
    )
    .await;
    let ViewData::Effort { slider } = next_view(&mut harness).await else {
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

    harness
        .send(UiCommand::Request {
            view: ViewRequest::Models { role: None },
        })
        .await;
    let ViewData::Models { choices, role, .. } = next_view(&mut harness).await else {
        panic!("the default agent's models");
    };
    assert_eq!(role, None);
    let current: Vec<String> = choices
        .iter()
        .filter(|choice| choice.current)
        .map(|choice| choice.worker.to_string())
        .collect();
    assert_eq!(current, vec!["claude-code".to_owned()], "{choices:#?}");

    slash(
        &harness,
        SlashCommand::Agents {
            action: AgentsAction::List,
        },
    )
    .await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut harness).await else {
        panic!("/agents answers with rows");
    };
    assert_eq!(command.as_deref(), Some("agents"));
    assert_eq!(row(&rows, "crew").default_scope, Project);
    assert_eq!(row(&rows, "agents.opencode-acp.binary").layer, "built-in");
    assert!(
        !rows.iter().any(|row| row.key.starts_with("agents.hermes.")),
        "Hermes is off until a layer turns it on"
    );

    slash(
        &harness,
        SlashCommand::Preset {
            name: None,
            action: PresetAction::Use,
        },
    )
    .await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut harness).await else {
        panic!("/preset answers with rows");
    };
    assert_eq!(command.as_deref(), Some("preset"));
    let peer = rows
        .iter()
        .find(|row| row.value == "\"peer-crew\"")
        .expect("the built-in peer-crew");
    assert_eq!((peer.key.as_str(), peer.default_scope), ("preset", Project));
    assert!(rows.iter().any(|row| row.value == "\"coordinated-crew\""));

    // With no run on screen, /workflows opens the workflow view's Library tab alone.
    slash(&harness, SlashCommand::Workflows).await;
    let ViewData::WorkflowLibrary { workflows, refresh } = next_view(&mut harness).await else {
        panic!("/workflows answers with the library when no run is on screen");
    };
    for builtin in ["solo", "best-of-n", "draft-and-vote"] {
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
    assert_eq!(
        row(&rows, "approvals.approver.model").default_scope,
        Project
    );

    slash(
        &harness,
        SlashCommand::Budget {
            key: None,
            value: None,
        },
    )
    .await;
    let ViewData::Settings { rows, .. } = next_view(&mut harness).await else {
        panic!("/budget answers with rows");
    };
    assert!(rows.iter().all(|row| row.key.starts_with("budget.")));
    assert_eq!(row(&rows, "budget.run_usd").default_scope, Project);

    slash(
        &harness,
        SlashCommand::Handoff {
            role: Some(RoleName::Reviewer),
            key: None,
            value: None,
        },
    )
    .await;
    let ViewData::Settings { rows, section, .. } = next_view(&mut harness).await else {
        panic!("/handoff answers with rows");
    };
    assert_eq!(section.as_deref(), Some("roles.reviewer.handoff"));
    assert_eq!(
        row(&rows, "roles.reviewer.handoff.mode").default_scope,
        Project
    );

    slash(
        &harness,
        SlashCommand::Permissions {
            role: None,
            key: None,
            value: None,
        },
    )
    .await;
    let ViewData::Settings { rows, .. } = next_view(&mut harness).await else {
        panic!("/permissions answers with rows");
    };
    let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
    // The keys a role can set lead every agent's sandbox, the rest of it follows, then the
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
    assert_eq!(row(&rows, "sandbox.agents_see_other_copies").value, "false");
    assert_eq!(row(&rows, "sandbox.agents_commit").value, "true");
    assert_eq!(row(&rows, "limits.usd_per_day").default_scope, Global);
    // What a direct turn asks you, and where solo works, are here too.
    assert_eq!(row(&rows, "solo.asks").value, "\"like_cli\"");
    assert!(row(&rows, "solo.asks").safety);
    assert_eq!(row(&rows, "solo.style").value, "\"direct\"");
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
        &harness,
        SlashCommand::Permissions {
            role: Some(RoleName::Reviewer),
            key: None,
            value: None,
        },
    )
    .await;
    let ViewData::Settings { rows, section, .. } = next_view(&mut harness).await else {
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

    slash(&harness, SlashCommand::Narrator { change: None }).await;
    let ViewData::Settings { rows, .. } = next_view(&mut harness).await else {
        panic!("/narrator answers with rows");
    };
    assert!(rows.iter().all(|row| row.default_scope == Global));
    assert_eq!(rows.len(), 4);

    slash(&harness, SlashCommand::Theme { name: None }).await;
    let ViewData::Settings { rows, .. } = next_view(&mut harness).await else {
        panic!("/theme answers with rows");
    };
    assert_eq!(row(&rows, "ui.theme").default_scope, Global);

    slash(
        &harness,
        SlashCommand::Config {
            key: None,
            value: None,
            reset: false,
        },
    )
    .await;
    let ViewData::Settings { rows, command, .. } = next_view(&mut harness).await else {
        panic!("/config answers with rows");
    };
    assert_eq!(command.as_deref(), Some("config"));
    assert_eq!(
        row(&rows, "roles.default.effort").default_scope,
        ProjectLocal
    );
    assert_eq!(row(&rows, "ui.keys").default_scope, Global);
    assert_eq!(row(&rows, "ui.keys").layer, "built-in");

    harness
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
    } = next_view(&mut harness).await
    else {
        panic!("a request answers with rows");
    };
    assert_eq!((command, section.as_deref()), (None, Some("ui")));
    assert!(rows.iter().all(|row| row.key.starts_with("ui.")));
    harness.exit().await;
}

#[tokio::test]
async fn each_command_argument_asks_the_scope_then_saves_keeping_comments() {
    use SettingScope::*;
    let codex: WorkerRef = "codex/gpt-5.6".parse().unwrap();
    // The command, the key it sets, the value saved, and the scope it recommends.
    let cases: Vec<(SlashCommand, &str, &str, SettingScope)> = vec![
        (
            SlashCommand::Model {
                role: Some(RoleName::Reviewer),
                worker: Some(codex.clone()),
                effort: None,
            },
            "roles.reviewer.workers",
            "[\"codex/gpt-5.6\"]",
            ProjectLocal,
        ),
        (
            SlashCommand::Effort {
                role: None,
                level: Some("high".to_owned()),
                all_hands: None,
            },
            "roles.default.effort",
            "\"high\"",
            ProjectLocal,
        ),
        (
            SlashCommand::Model {
                role: None,
                worker: Some("codex".parse().unwrap()),
                effort: None,
            },
            "default_agent",
            "\"codex\"",
            ProjectLocal,
        ),
        (
            SlashCommand::Preset {
                name: Some("peer-crew".to_owned()),
                action: PresetAction::Use,
            },
            "preset",
            "\"peer-crew\"",
            Project,
        ),
        (
            SlashCommand::Approvals {
                key: Some("merge.mode".to_owned()),
                value: Some("shadow".to_owned()),
            },
            "approvals.merge.mode",
            "\"shadow\"",
            Project,
        ),
        (
            SlashCommand::Budget {
                key: Some("run_usd".to_owned()),
                value: Some("5.0".to_owned()),
            },
            "budget.run_usd",
            "5.0",
            Project,
        ),
        (
            SlashCommand::Handoff {
                role: Some(RoleName::Reviewer),
                key: Some("mode".to_owned()),
                value: Some("wait".to_owned()),
            },
            "roles.reviewer.handoff.mode",
            "\"wait\"",
            Project,
        ),
        (
            SlashCommand::Handoff {
                role: None,
                key: Some("context".to_owned()),
                value: Some("note+summary".to_owned()),
            },
            "handoff.context",
            "\"note+summary\"",
            Project,
        ),
        (
            SlashCommand::Handoff {
                role: Some(RoleName::Reviewer),
                key: Some("summarizer".to_owned()),
                value: Some("claude-code/haiku".to_owned()),
            },
            "roles.reviewer.handoff.summarizer",
            "\"claude-code/haiku\"",
            Project,
        ),
        (
            SlashCommand::Permissions {
                role: None,
                key: Some("sandbox.network".to_owned()),
                value: Some("true".to_owned()),
            },
            "sandbox.network",
            "true",
            Project,
        ),
        (
            SlashCommand::Permissions {
                role: Some(RoleName::Owner),
                key: Some("mode".to_owned()),
                value: Some("tool".to_owned()),
            },
            "roles.owner.sandbox.mode",
            "\"tool\"",
            Project,
        ),
        (
            SlashCommand::Permissions {
                role: None,
                key: Some("sandbox.fail_closed".to_owned()),
                value: Some("false".to_owned()),
            },
            "sandbox.fail_closed",
            "false",
            Project,
        ),
        (
            SlashCommand::Narrator {
                change: Some(NarratorChange::Mode {
                    mode: NarratorMode::Live,
                }),
            },
            "ui.narrator",
            "\"live\"",
            Global,
        ),
        (
            SlashCommand::Theme {
                name: Some("dark".to_owned()),
            },
            "ui.theme",
            "\"dark\"",
            Global,
        ),
        (
            SlashCommand::Config {
                key: Some("ui.motion".to_owned()),
                value: Some("reduced".to_owned()),
                reset: false,
            },
            "ui.motion",
            "\"reduced\"",
            Global,
        ),
    ];
    let mut harness = Harness::start_with(harness_options(|repo| {
        std::fs::write(repo.global_config(), GLOBAL_WITH_COMMENTS).unwrap();
    }))
    .await;
    let global = harness.repo.global_config();
    for (command, key, value, scope) in cases {
        slash(&harness, command).await;
        let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
        assert_eq!(recommended(&question), choose(scope), "{key}");
        assert_eq!(question.body[0], format!("{key} = {value}"));
        answer(&harness, &question.id, choose(Global), None).await;
        let safety = !argonauts_registry::safety_keys_in(key, value).is_empty();
        if safety {
            let confirm = next_choice(&mut harness, ChoiceKind::ConfirmSafetySetting).await;
            assert!(
                confirm.body[0].starts_with(&format!("{key}: now ")),
                "{confirm:#?}"
            );
            answer(&harness, &confirm.id, AnswerAction::Approve, None).await;
        }
        let (changed, saved, saved_scope, _, reset) = next_change(&mut harness).await;
        assert_eq!(
            (changed.as_str(), saved.as_str(), saved_scope, reset),
            (key, value, Global, false)
        );
        let text = std::fs::read_to_string(&global).unwrap();
        assert!(text.contains("# Mine, keep these notes.\n[ui]\n"), "{text}");
        assert!(text.contains("bell = true  # I like it"), "{text}");
    }
    harness.exit().await;
}

/// A registry whose catalog has Claude Code's default running Sonnet 5.5, which starts at
/// high.
struct DefaultRunsSonnet(StaticRegistry);

impl ModelRegistry for DefaultRunsSonnet {
    fn get(&self, worker: &WorkerRef) -> Option<ModelEntry> {
        self.0.get(worker)
    }
    fn family(&self, worker: &WorkerRef) -> String {
        self.0.family(worker)
    }
    fn list(&self) -> Vec<ModelEntry> {
        self.0.list()
    }
    fn estimate(
        &self,
        worker: &WorkerRef,
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
            efforts: Vec::new(),
        }
    }
}

/// With no effort set, `/effort` on a CLI's default starts where the model it runs starts:
/// high for Sonnet 5.5.
#[tokio::test]
async fn the_effort_slider_starts_where_the_clis_default_model_does() {
    let mut harness = Harness::start_with(HarnessOptions {
        registry: Some(Arc::new(DefaultRunsSonnet(StaticRegistry::default()))),
        ..HarnessOptions::default()
    })
    .await;
    slash(
        &harness,
        SlashCommand::Effort {
            role: None,
            level: None,
            all_hands: None,
        },
    )
    .await;
    let ViewData::Effort { slider } = next_view(&mut harness).await else {
        panic!("/effort answers with its slider");
    };
    assert_eq!(slider.worker, "claude-code".parse().unwrap());
    assert_eq!(slider.level(), Some("high"));
    assert!(slider.is_default() && !slider.cli_default);
    harness.exit().await;
}

#[tokio::test]
async fn an_effort_level_the_model_lacks_is_refused_naming_the_ones_it_takes() {
    let registry = registry_with(&[serde_json::json!({
        "id": "claude-code/haiku",
        "family": "claude",
        "kind": "harness",
        "access": "subscription",
        "cost": { "unit": "quota" },
        "efforts": ["low", "medium", "high"],
    })]);
    let mut harness = Harness::start_with(HarnessOptions {
        registry: Some(registry),
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(
                repo.global_config(),
                "default_agent = \"claude-code/haiku\"\n",
            )
            .unwrap();
        })),
        ..HarnessOptions::default()
    })
    .await;
    slash(
        &harness,
        SlashCommand::Effort {
            role: None,
            level: Some("max".to_owned()),
            all_hands: None,
        },
    )
    .await;
    let refusal = next_notice(&mut harness).await;
    assert_eq!(
        refusal,
        "claude-code/haiku doesn't take max effort; it takes low, medium, high"
    );

    slash(
        &harness,
        SlashCommand::Effort {
            role: None,
            level: None,
            all_hands: None,
        },
    )
    .await;
    let ViewData::Effort { slider } = next_view(&mut harness).await else {
        panic!("/effort answers with its slider");
    };
    let levels: Vec<&str> = slider
        .stops
        .iter()
        .map(|stop| stop.level.as_str())
        .collect();
    assert_eq!(levels, ["low", "medium", "high"]);
    assert_eq!(slider.worker, "claude-code/haiku".parse().unwrap());
    harness.exit().await;
}

/// agy's level-named models take their own level only, so another is refused with why.
#[tokio::test]
async fn another_level_for_a_model_fixed_at_one_is_refused_saying_so() {
    let registry = registry_with(&[serde_json::json!({
        "id": "antigravity-cli/gemini-3.8-flash-high",
        "family": "gemini",
        "kind": "harness",
        "access": "subscription",
        "cost": { "unit": "quota" },
        "efforts": ["high"],
        "default_effort": "high",
    })]);
    let mut harness = Harness::start_with(HarnessOptions {
        registry: Some(registry),
        prepare: Some(Box::new(|repo: &TempRepo| {
            std::fs::write(
                repo.global_config(),
                "default_agent = \"antigravity-cli/gemini-3.8-flash-high\"\n",
            )
            .unwrap();
        })),
        ..HarnessOptions::default()
    })
    .await;
    slash(
        &harness,
        SlashCommand::Effort {
            role: None,
            level: Some("low".to_owned()),
            all_hands: None,
        },
    )
    .await;
    assert_eq!(
        next_notice(&mut harness).await,
        "antigravity-cli/gemini-3.8-flash-high takes only high effort, fixed by the model"
    );
    harness.exit().await;
}

#[tokio::test]
async fn a_model_picked_with_an_effort_saves_both_where_you_choose() {
    let registry = registry_with(&[serde_json::json!({
        "id": "claude-code/haiku",
        "family": "claude",
        "kind": "harness",
        "access": "subscription",
        "cost": { "unit": "quota" },
        "efforts": ["low", "medium", "high"],
    })]);
    let mut harness = Harness::start_with(HarnessOptions {
        registry: Some(registry),
        ..HarnessOptions::default()
    })
    .await;
    let haiku: WorkerRef = "claude-code/haiku".parse().unwrap();
    let pick = |effort: &str| SlashCommand::Model {
        role: None,
        worker: Some(haiku.clone()),
        effort: Some(effort.to_owned()),
    };
    slash(&harness, pick("max")).await;
    assert_eq!(
        next_notice(&mut harness).await,
        "claude-code/haiku doesn't take max effort; it takes low, medium, high"
    );

    slash(&harness, pick("high")).await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    assert_eq!(question.body[0], "default_agent = \"claude-code/haiku\"");
    assert_eq!(question.body[1], "roles.default.effort = \"high\"");
    answer(&harness, &question.id, choose(SettingScope::Global), None).await;
    let mut saved = Vec::new();
    for _ in 0..2 {
        let (key, value, scope, _, _) = next_change(&mut harness).await;
        assert_eq!(scope, SettingScope::Global);
        saved.push((key, value));
    }
    saved.sort();
    assert_eq!(
        saved,
        [
            (
                "default_agent".to_owned(),
                "\"claude-code/haiku\"".to_owned()
            ),
            ("roles.default.effort".to_owned(), "\"high\"".to_owned()),
        ]
    );
    let global = std::fs::read_to_string(harness.repo.global_config()).unwrap();
    assert!(
        global.contains("default_agent = \"claude-code/haiku\""),
        "{global}"
    );
    assert!(global.contains("effort = \"high\""), "{global}");

    // The models level starts each model's effort line on the level now set.
    harness
        .send(UiCommand::Request {
            view: ViewRequest::Models { role: None },
        })
        .await;
    let ViewData::Models { choices, .. } = next_view(&mut harness).await else {
        panic!("the default agent's models");
    };
    assert!(
        choices
            .iter()
            .all(|choice| choice.effort.as_deref() == Some("high")),
        "{choices:#?}"
    );
    harness.exit().await;
}

/// `/effort`'s All hands toggle. The default agent's slider offers it as it is now and a role's
/// doesn't. The toggle alone asks where to save and writes only `triage.all_hands`, which the
/// status then carries for the input rule; with a level, both are saved together. `/config`
/// lists it.
#[tokio::test]
async fn all_hands_from_the_effort_slider_saves_where_you_choose() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        let reviewer = "[roles.reviewer]\nworkers = [\"claude-code\"]\n";
        std::fs::write(repo.global_config(), reviewer).unwrap();
    }))
    .await;
    let effort = |role: Option<RoleName>, level: Option<&str>, all_hands: Option<bool>| {
        SlashCommand::Effort {
            role,
            level: level.map(str::to_owned),
            all_hands,
        }
    };
    slash(&harness, effort(None, None, None)).await;
    let ViewData::Effort { slider } = next_view(&mut harness).await else {
        panic!("/effort answers with its slider");
    };
    assert_eq!(slider.all_hands, Some(AllHandsToggle::new(false)));
    slash(&harness, effort(Some(RoleName::Reviewer), None, None)).await;
    let ViewData::Effort { slider } = next_view(&mut harness).await else {
        panic!("/effort reviewer answers with its slider");
    };
    assert_eq!(slider.all_hands, None, "a role's slider has no toggle");

    slash(&harness, effort(None, None, Some(true))).await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    assert_eq!(question.body[0], "triage.all_hands = true");
    assert!(
        !question.body.iter().any(|line| line.contains("effort")),
        "{question:?}"
    );
    assert_eq!(
        recommended(&question),
        choose(SettingScope::ProjectLocal),
        "saved for just you, as /effort saves"
    );
    answer(&harness, &question.id, choose(SettingScope::Project), None).await;
    let (key, value, scope, _, _) = next_change(&mut harness).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("triage.all_hands", "true", SettingScope::Project)
    );
    let event = harness
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    let AppEvent::Status { status } = event else {
        unreachable!()
    };
    assert!(status.all_hands, "the input rule's label shows it");
    let project =
        std::fs::read_to_string(harness.repo.root().join(".argonauts/config.toml")).unwrap();
    assert!(project.contains("all_hands = true"), "{project}");
    assert!(!project.contains("effort"), "{project}");

    slash(&harness, effort(None, None, None)).await;
    let ViewData::Effort { slider } = next_view(&mut harness).await else {
        panic!("/effort answers with its slider");
    };
    assert_eq!(slider.all_hands, Some(AllHandsToggle::new(true)));

    slash(&harness, effort(None, Some("high"), Some(false))).await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    assert_eq!(
        question.body[..2],
        [
            "roles.default.effort = \"high\"".to_owned(),
            "triage.all_hands = false".to_owned()
        ]
    );
    answer(&harness, &question.id, choose(SettingScope::Project), None).await;
    let mut saved = Vec::new();
    for _ in 0..2 {
        let (key, value, _, _, _) = next_change(&mut harness).await;
        saved.push((key, value));
    }
    saved.sort();
    assert_eq!(
        saved,
        [
            ("roles.default.effort".to_owned(), "\"high\"".to_owned()),
            ("triage.all_hands".to_owned(), "false".to_owned()),
        ]
    );
    harness.exit().await;
}

#[tokio::test]
async fn config_lists_all_hands_off_by_default() {
    let fixture = Fixture::new(false, |_| {});
    let rows = rows::rows(&fixture.ctx.settings, Some("config"), None);
    let all_hands = row(&rows, "triage.all_hands");
    assert_eq!(
        (all_hands.value.as_str(), all_hands.layer.as_str()),
        ("false", "built-in")
    );
    assert_eq!(all_hands.allowed, ["true", "false"]);
    assert!(
        all_hands
            .description
            .contains("every task goes to the crew"),
        "{}",
        all_hands.description
    );
}

#[tokio::test]
async fn a_reset_falls_back_a_layer() {
    let mut harness = Harness::start_with(harness_options(|repo| {
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
        &harness,
        SlashCommand::Config {
            key: Some("ui.theme".to_owned()),
            value: None,
            reset: true,
        },
    )
    .await;
    let (key, value, scope, previous, reset) = next_change(&mut harness).await;
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
        std::fs::read_to_string(harness.repo.root().join(".argonauts/config.local.toml")).unwrap();
    assert!(!local.contains("theme"), "{local}");
    assert!(
        local.contains("# just me") && local.contains("motion"),
        "{local}"
    );
    let ui = harness
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
    harness
        .send(UiCommand::ResetSetting {
            key: "ui.theme".to_owned(),
            scope: SettingScope::Project,
        })
        .await;
    let (_, value, scope, _, reset) = next_change(&mut harness).await;
    assert_eq!(
        (value.as_str(), scope, reset),
        ("\"auto\"", SettingScope::Project, true)
    );

    // Nothing sets it now but the built-in.
    slash(
        &harness,
        SlashCommand::Config {
            key: Some("ui.theme".to_owned()),
            value: None,
            reset: true,
        },
    )
    .await;
    assert_eq!(
        next_notice(&mut harness).await,
        "ui.theme is already the built-in default."
    );
    harness.exit().await;
}

#[tokio::test]
async fn vim_is_saved_globally() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        std::fs::write(repo.global_config(), GLOBAL_WITH_COMMENTS).unwrap();
    }))
    .await;
    slash(&harness, SlashCommand::Vim).await;
    let (key, value, scope, _, _) = next_change(&mut harness).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("ui.keys", "\"vim\"", SettingScope::Global)
    );
    let AppEvent::UiSettings { vim, .. } = harness
        .until(|event| matches!(event, AppEvent::UiSettings { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(vim);
    let text = std::fs::read_to_string(harness.repo.global_config()).unwrap();
    assert!(
        text.contains("keys = \"vim\"") && text.contains("# I like it"),
        "{text}"
    );

    // `/vim` again turns it off.
    slash(&harness, SlashCommand::Vim).await;
    let (_, value, _, previous, _) = next_change(&mut harness).await;
    assert_eq!(
        (value.as_str(), previous.as_deref()),
        ("\"default\"", Some("\"vim\""))
    );

    // `/verbosity quiet`, then `/verbosity normal`.
    let level = |level| SlashCommand::Verbosity { level: Some(level) };
    slash(&harness, level(Verbosity::Quiet)).await;
    let (key, value, ..) = next_change(&mut harness).await;
    assert_eq!(
        (key.as_str(), value.as_str()),
        ("ui.verbosity", "\"quiet\"")
    );
    slash(&harness, level(Verbosity::Normal)).await;
    let (_, value, ..) = next_change(&mut harness).await;
    assert_eq!(value, "\"normal\"");
    harness.exit().await;
}

#[tokio::test]
async fn vim_is_held_for_the_session_when_the_project_file_sets_it() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(".argonauts/config.toml", "[ui]\nkeys = \"default\"\n")
            .unwrap();
    }))
    .await;
    slash(&harness, SlashCommand::Vim).await;
    let (key, value, scope, ..) = next_change(&mut harness).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("ui.keys", "\"vim\"", SettingScope::Session)
    );
    let notice = next_notice(&mut harness).await;
    assert!(
        notice.starts_with("This project's file sets ui.keys"),
        "{notice}"
    );
    assert!(!harness.repo.global_config().exists());

    // The session holds it, and the next toggle still knows the project file is under it.
    slash(&harness, SlashCommand::Vim).await;
    let (_, value, scope, ..) = next_change(&mut harness).await;
    assert_eq!(
        (value.as_str(), scope),
        ("\"default\"", SettingScope::Session)
    );
    harness.exit().await;
}

#[tokio::test]
async fn write_to_untrusted_project_file_keeps_it_untrusted() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(
            ".argonauts/config.local.toml",
            "[sandbox]\nmode = \"container\"\n",
        )
        .unwrap();
    }))
    .await;
    slash(
        &harness,
        SlashCommand::Effort {
            role: None,
            level: Some("high".to_owned()),
            all_hands: None,
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    assert_eq!(recommended(&question), choose(SettingScope::ProjectLocal));
    answer(
        &harness,
        &question.id,
        choose(SettingScope::ProjectLocal),
        None,
    )
    .await;
    let trust = next_choice(&mut harness, ChoiceKind::TrustProject).await;
    assert!(
        trust
            .body
            .iter()
            .any(|line| line.trim() == "sandbox.mode = \"container\""),
        "{trust:#?}"
    );
    answer(&harness, &trust.id, AnswerAction::Cancel, None).await;
    let (key, value, scope, ..) = next_change(&mut harness).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        (
            "roles.default.effort",
            "\"high\"",
            SettingScope::ProjectLocal
        )
    );

    let file = harness.repo.root().join(".argonauts/config.local.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("mode = \"container\"") && text.contains("effort = \"high\""),
        "{text}"
    );
    assert!(!TrustStore::load(&state_dir(&harness.repo)).is_trusted(&file));
    harness
        .send(UiCommand::Request {
            view: ViewRequest::Config,
        })
        .await;
    let ViewData::Config { entries } = next_view(&mut harness).await else {
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
        .find(|entry| entry.key == "roles.default.effort")
        .unwrap();
    assert_eq!(effort.layer, "project-local");
    harness.exit().await;
}

async fn next_closed(harness: &mut Harness, id: &ChoiceId) -> String {
    let event = harness
        .until(|event| matches!(event, AppEvent::ChoiceClosed { id: closed, .. } if closed == id))
        .await;
    match event {
        AppEvent::ChoiceClosed { outcome, .. } => outcome,
        _ => unreachable!(),
    }
}

async fn sandbox_mode(harness: &mut Harness) -> (String, String) {
    harness
        .send(UiCommand::Request {
            view: ViewRequest::Config,
        })
        .await;
    let ViewData::Config { entries } = next_view(harness).await else {
        panic!("/config's plain list");
    };
    let mode = entries
        .into_iter()
        .find(|entry| entry.key == "sandbox.mode")
        .unwrap();
    (mode.value, mode.layer)
}

const UNSAFE_PROJECT: &str =
    "[sandbox]\nmode = \"container\"\n\n[roles.default]\neffort = \"low\"\n";

#[tokio::test]
async fn untrusted_safety_keys_are_asked_about_at_start() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(".argonauts/config.toml", UNSAFE_PROJECT)
            .unwrap();
    }))
    .await;
    let trust = next_choice(&mut harness, ChoiceKind::TrustProject).await;
    assert_eq!(trust.title, "Trust .argonauts/config.toml?");
    assert!(
        trust
            .body
            .iter()
            .any(|line| line.trim() == "sandbox.mode = \"container\""),
        "{trust:#?}"
    );
    answer(&harness, &trust.id, AnswerAction::Cancel, None).await;
    let outcome = next_closed(&mut harness, &trust.id).await;
    assert!(outcome.contains("stays untrusted"), "{outcome}");
    let file = harness.repo.root().join(".argonauts/config.toml");
    assert!(!TrustStore::load(&state_dir(&harness.repo)).is_trusted(&file));
    assert_eq!(sandbox_mode(&mut harness).await.1, "built-in");
    harness.exit().await;

    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(".argonauts/config.toml", UNSAFE_PROJECT)
            .unwrap();
    }))
    .await;
    let trust = next_choice(&mut harness, ChoiceKind::TrustProject).await;
    answer(&harness, &trust.id, AnswerAction::Approve, None).await;
    let outcome = next_closed(&mut harness, &trust.id).await;
    assert_eq!(outcome, "Trusted .argonauts/config.toml.");
    let file = harness.repo.root().join(".argonauts/config.toml");
    assert!(TrustStore::load(&state_dir(&harness.repo)).is_trusted(&file));
    assert_eq!(
        sandbox_mode(&mut harness).await,
        ("\"container\"".to_owned(), "project".to_owned())
    );
    harness.exit().await;
}

#[tokio::test]
async fn a_file_changed_after_the_start_question_stays_untrusted() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(".argonauts/config.local.toml", UNSAFE_PROJECT)
            .unwrap();
    }))
    .await;
    let trust = next_choice(&mut harness, ChoiceKind::TrustProject).await;
    let file = harness.repo.root().join(".argonauts/config.local.toml");
    std::fs::write(&file, "[sandbox]\nmode = \"container\"\nnetwork = true\n").unwrap();
    answer(&harness, &trust.id, AnswerAction::Approve, None).await;
    let outcome = next_closed(&mut harness, &trust.id).await;
    assert!(
        outcome.contains("changed since you were asked"),
        "{outcome}"
    );
    assert!(!TrustStore::load(&state_dir(&harness.repo)).is_trusted(&file));
    harness.exit().await;
}

#[tokio::test]
async fn files_without_untrusted_safety_keys_ask_nothing_at_start() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(
            ".argonauts/config.toml",
            "[roles.default]\neffort = \"low\"\n",
        )
        .unwrap();
    }))
    .await;
    harness
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    let events = harness.exit().await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "{events:#?}"
    );
}

/// The file the decision was made from: a gate written as a string, beside a safety key.
const GATE_AS_STRING: &str =
    "[budget]\nrun_usd = 1.0\n\n[gates]\nrun = [\"python3 -m unittest -q\"]\n";

#[tokio::test]
async fn a_file_that_needs_a_fix_says_what_to_fix_instead_of_asking_for_trust() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(".argonauts/config.local.toml", GATE_AS_STRING)
            .unwrap();
    }))
    .await;
    harness
        .until(|event| matches!(event, AppEvent::Status { .. }))
        .await;
    let file = harness.repo.root().join(".argonauts/config.local.toml");
    assert!(!TrustStore::load(&state_dir(&harness.repo)).is_trusted(&file));
    let events = harness.exit().await;
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
    let fix = ".argonauts/config.local.toml needs a fix before you can trust it: gates.run \
               can't be a list of strings; it takes a list of checks, each a [[gates.run]] table \
               with a name and a command. Write it like this:\n\
               [[gates.run]]\n\
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
    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(".argonauts/config.toml", UNSAFE_PROJECT)
            .unwrap();
    }))
    .await;
    let trust = next_choice(&mut harness, ChoiceKind::TrustProject).await;
    // Your global file breaks while the question is open.
    let global = harness.repo.global_config();
    std::fs::write(&global, "[ui\n").unwrap();
    answer(&harness, &trust.id, AnswerAction::Approve, None).await;
    let outcome = next_closed(&mut harness, &trust.id).await;
    assert_eq!(outcome, NOT_CHANGED);
    let notice = next_notice(&mut harness).await;
    assert_eq!(
        notice,
        format!(
            ".argonauts/config.toml can't be trusted until {} is fixed: line 1 isn't valid TOML \
             (unclosed table, expected `]`): [ui",
            global.display()
        )
    );
    let file = harness.repo.root().join(".argonauts/config.toml");
    assert!(!TrustStore::load(&state_dir(&harness.repo)).is_trusted(&file));
    std::fs::remove_file(&global).unwrap();
    let events = harness.exit().await;
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
    let mut harness = Harness::start().await;
    slash(
        &harness,
        SlashCommand::Theme {
            name: Some("dark".to_owned()),
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    answer(&harness, &question.id, choose(SettingScope::Project), None).await;
    next_change(&mut harness).await;
    let file = harness.repo.root().join(".argonauts/config.toml");
    assert!(TrustStore::load(&state_dir(&harness.repo)).is_trusted(&file));

    // A confirmed safety key lands in the file you trusted and applies.
    slash(
        &harness,
        SlashCommand::Permissions {
            role: None,
            key: Some("sandbox.network".to_owned()),
            value: Some("true".to_owned()),
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    answer(&harness, &question.id, choose(SettingScope::Project), None).await;
    let confirm = next_choice(&mut harness, ChoiceKind::ConfirmSafetySetting).await;
    answer(&harness, &confirm.id, AnswerAction::Approve, None).await;
    let (key, value, ..) = next_change(&mut harness).await;
    assert_eq!((key.as_str(), value.as_str()), ("sandbox.network", "true"));
    assert!(TrustStore::load(&state_dir(&harness.repo)).is_trusted(&file));
    harness.exit().await;
}

#[tokio::test]
async fn a_safety_confirm_declined_changes_nothing() {
    let mut harness = Harness::start().await;
    slash(
        &harness,
        SlashCommand::Config {
            key: Some("approvals.merge.mode".to_owned()),
            value: Some("auto".to_owned()),
            reset: false,
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    answer(&harness, &question.id, choose(SettingScope::Global), None).await;
    let confirm = next_choice(&mut harness, ChoiceKind::ConfirmSafetySetting).await;
    assert!(
        confirm.body[0].starts_with("approvals.merge.mode: now "),
        "{confirm:#?}"
    );
    answer(&harness, &confirm.id, AnswerAction::Cancel, None).await;
    let closed = harness
        .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if *id == confirm.id))
        .await;
    assert!(matches!(closed, AppEvent::ChoiceClosed { outcome, .. } if outcome == "Not changed."));
    assert!(!harness.repo.global_config().exists());

    // Esc on the scope question closes it the same way.
    slash(
        &harness,
        SlashCommand::Theme {
            name: Some("dark".to_owned()),
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    harness
        .send(UiCommand::SetAside {
            to: AnswerTarget::Choice {
                id: question.id.clone(),
            },
        })
        .await;
    let closed = harness
        .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if *id == question.id))
        .await;
    assert!(matches!(closed, AppEvent::ChoiceClosed { outcome, .. } if outcome == "Not changed."));
    harness.exit().await;
}

#[tokio::test]
async fn the_scope_question_warns_when_a_higher_layer_wins() {
    let mut harness = Harness::start_with(harness_options(|repo| {
        repo.write(".argonauts/config.toml", "[ui]\ntheme = \"light\"\n")
            .unwrap();
    }))
    .await;
    slash(
        &harness,
        SlashCommand::Theme {
            name: Some("dark".to_owned()),
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    let everywhere = question
        .options
        .iter()
        .find(|option| option.key == 'e')
        .unwrap();
    assert_eq!(
        everywhere.detail.as_deref(),
        Some("This project's file sets this, so saving it everywhere won't change this repo.")
    );
    assert!(
        question
            .options
            .iter()
            .filter(|option| option.detail.is_some())
            .count()
            == 1
    );
    assert_eq!(
        question.body[1],
        "Now \"light\", from this project's shared file."
    );
    harness.exit().await;
}

#[tokio::test]
async fn the_coordinator_is_asked_for_and_saved_with_the_preset() {
    let mut harness = Harness::start().await;
    slash(
        &harness,
        SlashCommand::Preset {
            name: Some("coordinated-crew".to_owned()),
            action: PresetAction::Use,
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    answer(&harness, &question.id, choose(SettingScope::Project), None).await;
    let ViewData::Models { role, .. } = next_view(&mut harness).await else {
        panic!("the coordinator's picker");
    };
    assert_eq!(role, Some(RoleName::Coordinator));

    // The pick saves both keys to the preset's scope without asking again.
    let worker: WorkerRef = "codex/gpt-5.6".parse().unwrap();
    slash(
        &harness,
        SlashCommand::Model {
            role: Some(RoleName::Coordinator),
            worker: Some(worker.clone()),
            effort: None,
        },
    )
    .await;
    let (key, value, scope, ..) = next_change(&mut harness).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("preset", "\"coordinated-crew\"", SettingScope::Project)
    );
    let (key, value, scope, ..) = next_change(&mut harness).await;
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        (
            "roles.coordinator.workers",
            "[\"codex/gpt-5.6\"]",
            SettingScope::Project
        )
    );
    let saved =
        std::fs::read_to_string(harness.repo.root().join(".argonauts/config.toml")).unwrap();
    assert!(
        saved.contains("coordinated-crew") && saved.contains("codex/gpt-5.6"),
        "{saved}"
    );
    let questions = harness
        .seen()
        .iter()
        .filter(|event| matches!(event, AppEvent::Choice { .. }))
        .count();
    assert_eq!(questions, 1, "no second question: {:#?}", harness.seen());

    harness.exit().await;

    // Anything else first drops the hold, so a later coordinator pick is a plain /model. A
    // fresh repo, since this one now has its coordinator.
    let mut harness = Harness::start().await;
    slash(
        &harness,
        SlashCommand::Preset {
            name: Some("coordinated-crew".to_owned()),
            action: PresetAction::Use,
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    answer(&harness, &question.id, choose(SettingScope::Project), None).await;
    next_view(&mut harness).await;
    slash(&harness, SlashCommand::Theme { name: None }).await;
    next_view(&mut harness).await;
    slash(
        &harness,
        SlashCommand::Model {
            role: Some(RoleName::Coordinator),
            worker: Some(worker),
            effort: None,
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    assert_eq!(
        question.body[0],
        "roles.coordinator.workers = [\"codex/gpt-5.6\"]"
    );
    harness.exit().await;
}

#[tokio::test]
async fn a_command_outside_settings_drops_the_coordinator_hold() {
    let mut harness = Harness::start().await;
    slash(
        &harness,
        SlashCommand::Preset {
            name: Some("coordinated-crew".to_owned()),
            action: PresetAction::Use,
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    answer(&harness, &question.id, choose(SettingScope::Project), None).await;
    next_view(&mut harness).await;

    // The screen answers /help itself, so the controller only sees that you moved on.
    slash(&harness, SlashCommand::Help).await;
    slash(
        &harness,
        SlashCommand::Model {
            role: Some(RoleName::Coordinator),
            worker: Some("codex/gpt-5.6".parse().unwrap()),
            effort: None,
        },
    )
    .await;
    let question = next_choice(&mut harness, ChoiceKind::SettingScope).await;
    assert_eq!(
        question.body[0],
        "roles.coordinator.workers = [\"codex/gpt-5.6\"]"
    );
    harness.exit().await;
}

#[test]
fn the_coordinator_pick_carries_the_preset() {
    let worker: WorkerRef = "codex/gpt-5.6".parse().unwrap();
    assert_eq!(
        coordinator_change(&worker),
        Change::Set(vec![
            ("preset".to_owned(), "\"coordinated-crew\"".to_owned()),
            (
                "roles.coordinator.workers".to_owned(),
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
        SettingScope::Global,
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
        SettingScope::Session,
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
        SettingScope::Session,
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
    let mut harness = Harness::start_with(HarnessOptions {
        interactive: false,
        ..harness_options(|repo| {
            std::fs::write(repo.global_config(), "[sandbox]\nnetwork = true\n").unwrap();
        })
    })
    .await;
    let refused = |event: &AppEvent| {
        matches!(
            event,
            AppEvent::Notice { level: NoteLevel::Error, text }
                if text.contains("needs you at a terminal")
        )
    };
    let refusal = async |harness: &mut Harness| match harness.until(refused).await {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    };

    slash(
        &harness,
        SlashCommand::Permissions {
            role: None,
            key: Some("sandbox.network".to_owned()),
            value: Some("false".to_owned()),
        },
    )
    .await;
    assert!(refusal(&mut harness).await.contains("sandbox.network"));
    harness
        .send(UiCommand::SetSetting {
            key: "sandbox.network".to_owned(),
            value: "false".to_owned(),
            scope: SettingScope::Session,
        })
        .await;
    assert!(refusal(&mut harness).await.contains("sandbox.network"));
    slash(
        &harness,
        SlashCommand::Config {
            key: Some("approvals.merge.mode".to_owned()),
            value: Some("auto".to_owned()),
            reset: false,
        },
    )
    .await;
    assert!(refusal(&mut harness).await.contains("approvals.merge.mode"));
    harness
        .send(UiCommand::ResetSetting {
            key: "sandbox.network".to_owned(),
            scope: SettingScope::Global,
        })
        .await;
    assert!(refusal(&mut harness).await.contains("sandbox.network"));

    // Keys that aren't safety keys still change.
    slash(
        &harness,
        SlashCommand::Theme {
            name: Some("dark".to_owned()),
        },
    )
    .await;
    next_choice(&mut harness, ChoiceKind::SettingScope).await;
    let global = std::fs::read_to_string(harness.repo.global_config()).unwrap();
    assert_eq!(global, "[sandbox]\nnetwork = true\n");
    let events = harness.exit().await;
    assert!(
        !events.iter().any(|event| match event {
            AppEvent::SettingChanged { .. } => true,
            AppEvent::Choice { choice } => choice.kind == ChoiceKind::ConfirmSafetySetting,
            _ => false,
        }),
        "{events:#?}"
    );
}

#[tokio::test]
async fn gates_are_not_offered_without_a_terminal() {
    let mut fixture = Fixture::with_terminal(false, false, |repo| {
        repo.write("Cargo.toml", "[package]\nname = \"x\"\n")
            .unwrap();
    });
    offer_gates(&mut fixture.ctx, &mut fixture.state)
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
async fn gates_are_offered_once() {
    let mut fixture = Fixture::new(false, |repo| {
        repo.write("Cargo.toml", "[package]\nname = \"x\"\n")
            .unwrap();
    });
    offer_gates(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    let offer = fixture.only_choice();
    assert_eq!(offer.kind, ChoiceKind::DetectedGates);
    assert!(offer.body.iter().any(|line| line.trim() == "cargo test"));
    assert!(
        offer
            .body
            .iter()
            .any(|line| line.contains("safety setting"))
    );
    // Not again while it's open.
    offer_gates(&mut fixture.ctx, &mut fixture.state)
        .await
        .unwrap();
    assert!(fixture.drain().is_empty());

    // Esc leaves it for the next run.
    set_aside(&mut fixture.ctx, &mut fixture.state, offer.id.clone()).unwrap();
    offer_gates(&mut fixture.ctx, &mut fixture.state)
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
    assert_eq!(fixture.ctx.settings().gates.run.len(), 1);
    assert_eq!(
        fixture.ctx.settings().gates.run[0].command,
        vec!["cargo", "test", "--workspace"]
    );
    offer_gates(&mut fixture.ctx, &mut fixture.state)
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
async fn skipping_the_gates_offer_is_remembered_in_the_repo() {
    let mut fixture = Fixture::new(false, |repo| {
        repo.write("pyproject.toml", "").unwrap();
    });
    offer_gates(&mut fixture.ctx, &mut fixture.state)
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
    offer_gates(&mut fixture.ctx, &mut state).await.unwrap();
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
    assert_eq!(
        fixture.ctx.settings.run_overrides(),
        vec!["ui.narrator=\"live\"".to_owned()]
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
async fn a_new_default_agent_starts_a_fresh_chat() {
    let mut fixture = Fixture::new(false, |_| {});
    let effects = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "default_agent".to_owned(),
        "\"codex\"".to_owned(),
        SettingScope::Session,
    )
    .await
    .unwrap();
    assert!(
        matches!(effects.first(), Some(Effect::FreshChat)),
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
        "budget.soft_stop".to_owned(),
        "\"lots\"".to_owned(),
        SettingScope::Project,
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
        "handoff.strategi".to_owned(),
        "spread".to_owned(),
        SettingScope::Project,
    )
    .await
    .unwrap_err();
    // The resolver names the key and the one you likely meant.
    let err = err.to_string();
    assert!(
        err.contains("handoff.strategi isn't a setting") && err.contains("handoff.strategy"),
        "{err}"
    );
    assert!(!file.exists());
    let err = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "handoff.strategi".to_owned(),
        "spread".to_owned(),
        SettingScope::Session,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("isn't a setting"), "{err}");
    assert!(fixture.ctx.settings.session_overrides().is_empty());

    set(
        &mut fixture.ctx,
        &mut fixture.state,
        "handoff.strategy".to_owned(),
        "spread".to_owned(),
        SettingScope::Project,
    )
    .await
    .unwrap();
    assert_eq!(
        fixture
            .ctx
            .settings
            .entry("handoff.strategy")
            .unwrap()
            .value,
        "\"spread\""
    );
    let rows = rows::rows(&fixture.ctx.settings, Some("handoff"), None);
    let strategy = row(&rows, "handoff.strategy");
    assert_eq!(strategy.layer, "project");
    assert!(strategy.allowed.contains(&"\"race\"".to_owned()));
}

/// Sets `key` through the same path as a picker's scope answer.
async fn set_at(fixture: &mut Fixture, key: &str, value: &str, scope: SettingScope) {
    set(
        &mut fixture.ctx,
        &mut fixture.state,
        key.to_owned(),
        value.to_owned(),
        scope,
    )
    .await
    .unwrap();
}

fn keys_of(rows: &[SettingRow]) -> Vec<&str> {
    rows.iter().map(|row| row.key.as_str()).collect()
}

#[tokio::test]
async fn handoff_shows_what_the_replacement_is_told_and_a_summarizer_once_it_needs_one() {
    let mut fixture = Fixture::new(false, |_| {});
    let handoff = |fixture: &Fixture, section: Option<&str>| {
        rows::rows(&fixture.ctx.settings, Some("handoff"), section)
    };

    let rows = handoff(&fixture, None);
    let context = row(&rows, "handoff.context");
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
        context
            .description
            .starts_with("What the replacement is told"),
        "{}",
        context.description
    );
    assert_eq!(context.default_scope, SettingScope::Project);
    assert!(keys_of(&rows).contains(&"roles.default.handoff.context"));
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
        "handoff.context",
        "note+summary",
        SettingScope::Project,
    )
    .await;
    let rows = handoff(&fixture, None);
    let summarizer = row(&rows, "handoff.summarizer");
    assert_eq!(summarizer.value, "");
    assert!(summarizer.allowed.is_empty());
    assert_eq!(summarizer.default_scope, SettingScope::Project);
    assert!(keys_of(&rows).contains(&"roles.default.handoff.summarizer"));
    let reviewer = handoff(&fixture, Some("roles.reviewer.handoff"));
    assert!(keys_of(&reviewer).contains(&"roles.reviewer.handoff.summarizer"));
    assert!(keys_of(&reviewer).contains(&"roles.reviewer.handoff.context"));

    // One role told note+summary needs its own, and the top-level one that serves it.
    set_at(
        &mut fixture,
        "handoff.context",
        "note",
        SettingScope::Project,
    )
    .await;
    set_at(
        &mut fixture,
        "roles.reviewer.handoff.context",
        "note+summary",
        SettingScope::Project,
    )
    .await;
    let reviewer = handoff(&fixture, Some("roles.reviewer.handoff"));
    assert!(keys_of(&reviewer).contains(&"roles.reviewer.handoff.summarizer"));
    let rows = handoff(&fixture, None);
    assert!(keys_of(&rows).contains(&"handoff.summarizer"));
    assert!(!keys_of(&rows).contains(&"roles.default.handoff.summarizer"));

    // So does a vendor's replacements being told note+summary.
    set_at(
        &mut fixture,
        "roles.reviewer.handoff.context",
        "fresh",
        SettingScope::Project,
    )
    .await;
    let rows = handoff(&fixture, None);
    assert!(!keys_of(&rows).contains(&"handoff.summarizer"));
    set_at(
        &mut fixture,
        "handoff.replace.claude-code",
        "{ with = [\"codex/gpt-5.6\"], context = \"note+summary\" }",
        SettingScope::Project,
    )
    .await;
    let rows = handoff(&fixture, None);
    assert!(keys_of(&rows).contains(&"handoff.summarizer"));

    // A summarizer you set stays listed, so you can change or reset it.
    set_at(
        &mut fixture,
        "handoff.replace.claude-code.context",
        "note",
        SettingScope::Project,
    )
    .await;
    set_at(
        &mut fixture,
        "handoff.summarizer",
        "claude-code/haiku",
        SettingScope::Project,
    )
    .await;
    let rows = handoff(&fixture, None);
    let summarizer = row(&rows, "handoff.summarizer");
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
async fn the_handoff_context_saves_at_each_scope() {
    use SettingScope::*;
    let mut fixture = Fixture::new(false, |_| {});
    let layer = |fixture: &Fixture, key: &str| {
        let rows = rows::rows(&fixture.ctx.settings, Some("handoff"), None);
        let row = row(&rows, key);
        (row.value.clone(), row.layer.clone())
    };
    assert_eq!(
        layer(&fixture, "handoff.context"),
        ("\"note+transcript\"".to_owned(), "built-in".to_owned())
    );
    let cases = [
        (Global, "fresh", "global"),
        (Project, "note", "project"),
        (ProjectLocal, "note+summary", "project-local"),
        (Session, "note+transcript", "session"),
    ];
    for (scope, value, expected) in cases {
        set_at(&mut fixture, "handoff.context", value, scope).await;
        assert_eq!(
            layer(&fixture, "handoff.context"),
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
    set_at(
        &mut fixture,
        "roles.reviewer.handoff.context",
        "fresh",
        Project,
    )
    .await;
    set_at(&mut fixture, "handoff.summarizer", "codex/gpt-5.6", Session).await;
    let rows = rows::rows(
        &fixture.ctx.settings,
        Some("handoff"),
        Some("roles.reviewer.handoff"),
    );
    let context = row(&rows, "roles.reviewer.handoff.context");
    assert_eq!(
        (context.value.as_str(), context.layer.as_str()),
        ("\"fresh\"", "project")
    );
    assert_eq!(
        layer(&fixture, "handoff.summarizer"),
        ("\"codex/gpt-5.6\"".to_owned(), "session".to_owned())
    );

    // A value the picker doesn't offer is refused, naming the four.
    let err = set(
        &mut fixture.ctx,
        &mut fixture.state,
        "handoff.context".to_owned(),
        "summary".to_owned(),
        Project,
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
async fn config_lists_the_handoff_context_and_summarizer_with_their_layers() {
    let mut fixture = Fixture::new(false, |_| {});
    let config = |fixture: &Fixture| rows::rows(&fixture.ctx.settings, Some("config"), None);
    let rows = config(&fixture);
    let context = row(&rows, "handoff.context");
    assert_eq!(
        (context.value.as_str(), context.layer.as_str()),
        ("\"note+transcript\"", "built-in")
    );
    let summarizer = row(&rows, "handoff.summarizer");
    assert_eq!(
        (summarizer.value.as_str(), summarizer.layer.as_str()),
        ("", "built-in")
    );
    assert!(!summarizer.description.is_empty());

    set_at(
        &mut fixture,
        "handoff.summarizer",
        "claude-code/haiku",
        SettingScope::ProjectLocal,
    )
    .await;
    set_at(
        &mut fixture,
        "roles.owner.handoff.context",
        "fresh",
        SettingScope::Project,
    )
    .await;
    let rows = config(&fixture);
    let summarizer = row(&rows, "handoff.summarizer");
    assert_eq!(
        (summarizer.value.as_str(), summarizer.layer.as_str()),
        ("\"claude-code/haiku\"", "project-local")
    );
    let owner = row(&rows, "roles.owner.handoff.context");
    assert_eq!(
        (owner.value.as_str(), owner.layer.as_str()),
        ("\"fresh\"", "project")
    );
    assert!(keys_of(&rows).contains(&"roles.owner.handoff.summarizer"));
    // `/config handoff` is the section on its own.
    let section = rows::rows(&fixture.ctx.settings, Some("config"), Some("handoff"));
    assert!(keys_of(&section).contains(&"handoff.context"));
    assert!(keys_of(&section).contains(&"handoff.summarizer"));
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
    let mut harness = Harness::start_with(HarnessOptions {
        flags,
        ..HarnessOptions::default()
    })
    .await;
    slash(
        &harness,
        SlashCommand::Agents {
            action: AgentsAction::List,
        },
    )
    .await;
    let ViewData::Settings { not_installed, .. } = next_view(&mut harness).await else {
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
        &harness,
        SlashCommand::Preset {
            name: None,
            action: PresetAction::Use,
        },
    )
    .await;
    let ViewData::Settings { not_installed, .. } = next_view(&mut harness).await else {
        panic!("/preset answers with rows");
    };
    assert!(not_installed.is_empty());
    harness.exit().await;
}
