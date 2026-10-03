//! `run_interactive`: refuse inside an agent, resolve the settings with each project file's
//! trust, start the screen, say hello, and hand over to the controller.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use argonauts_conductor::{AgentCheck, AgentStatus, DoctorReport, doctor};
use argonauts_core::NoteLevel;
use argonauts_core::api::{ConductorApi, RunState};
use argonauts_core::app::{AgentReady, AppEvent};
use argonauts_core::clock::SystemClock;
use argonauts_core::paths::{create_private_dir_all, old_global_config_path, write_private_file};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::FallbackEntry;
use argonauts_core::steps::SchemaStepRunner;
use argonauts_registry::{PresetUpgrade, PresetUpgradeOutcome, upgrade_presets};
use argonauts_tui::{AppIo, UiOptions, run_app, run_screen_reader, stop_signal};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};

use crate::ancestry::{AgentMarkers, check_self};
use crate::app_settings::AppSettings;
use crate::controller::{AppServices, Controller, Ctx, Opening, Outbox, header_agent};
use crate::env::AppEnv;
use crate::example_prompt::example_prompt;
use crate::host::harnesses::RepoModels;
use crate::host::services::{LiveRegistry, SettingsSource};
use crate::trace::Trace;
use crate::{FirstView, InteractiveOptions, builtin_adapters, client, host, needs_guided_setup};

/// Commands the screen may queue before the controller reads them.
const COMMAND_QUEUE: usize = 64;
/// Events on their way to the screen. The controller's own queue is unbounded, so it never
/// waits on a slow terminal.
const EVENT_QUEUE: usize = 256;

/// How long quitting waits for the controller. It is longer than the controller's own wait
/// for a cancelled chat step to stop its vendor process.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// Markers for the one-time start notices, so each shows on one start and not the next.
const OLD_CONFIG_TOLD: &str = "told-old-config";
const TRACKED_LOCAL_TOLD: &str = "told-tracked-local-config";

pub(crate) async fn run(mut options: InteractiveOptions, first: FirstView) -> anyhow::Result<()> {
    let mut first = first;
    // `/cd` closes the screen and asks for it to open again in another repo.
    while let Some(repo) = run_once(&options, first).await? {
        std::env::set_current_dir(&repo)
            .with_context(|| format!("moving to {}", repo.display()))?;
        options.repo = Some(repo);
        options.guided_setup = false;
        first = FirstView::Chat;
    }
    Ok(())
}

/// One screen, from start to close. `Some` is where `/cd` asked the app to open next.
async fn run_once(
    options: &InteractiveOptions,
    first: FirstView,
) -> anyhow::Result<Option<PathBuf>> {
    let env = AppEnv::discover(options.repo.as_deref())?;
    if env.agent_session {
        bail!(
            "this is an agent's session (ARGONAUTS_SESSION is set), so the app won't open here; \
             run argonauts from your own terminal"
        );
    }
    let settings = AppSettings::load(&env, options.overrides.clone(), env.interactive)?;
    check_self(
        &env.paths,
        &AgentMarkers::from_settings(settings.settings()),
    )?;
    let ui = UiOptions::from_settings(&settings.settings().ui);
    let screen_reader =
        options.screen_reader || settings.settings().ui.screen_reader || !env.interactive;
    // `argonauts run` attaches to the run it just started and never opens the setup.
    let guided_setup = options.guided_setup
        || (!matches!(first, FirstView::Attach(_)) && needs_guided_setup(&env.paths));

    let env = Arc::new(env);
    let services = services(&env, &settings)?;
    let trace = Trace::from_env().context("opening the test trace")?;
    let (out, mut outgoing) = mpsc::unbounded_channel();
    let (internal, internal_rx) = mpsc::unbounded_channel();
    let ctx = Ctx::new(
        env,
        settings,
        services,
        Outbox::new(out, Arc::new(trace)),
        internal,
    );
    let reopen = ctx.reopen();

    let (commands, commands_rx) = mpsc::channel(COMMAND_QUEUE);
    let (events, events_rx) = mpsc::channel(EVENT_QUEUE);
    let forward = tokio::spawn(async move {
        while let Some(event) = outgoing.recv().await {
            if events.send(event).await.is_err() {
                break;
            }
        }
    });
    // The screen draws at once; the header and the controller follow on their own task.
    let app = tokio::spawn(async move {
        let mut ctx = ctx;
        greet(&mut ctx).await;
        let controller = Controller::new(ctx, internal_rx)?;
        controller
            .run(
                commands_rx,
                Opening {
                    guided_setup,
                    first,
                },
            )
            .await
    });
    let io = AppIo {
        commands,
        events: events_rx,
    };
    let screen = if screen_reader {
        run_screen_reader(io, ui).await
    } else {
        run_app(io, ui).await
    };
    // The screen dropped its command sender, so the controller's loop ends too, unless a call
    // to a run's supervisor holds it. The run goes on without this app, so quitting doesn't wait.
    // Screen-reader mode never took over the stop signals, so they still end the app at once.
    let mut app = app;
    let stop = async {
        if screen_reader {
            std::future::pending::<()>().await;
        }
        stop_signal().await;
    };
    let app = match wait_for_controller(&mut app, EXIT_GRACE, stop).await {
        Some(joined) => joined.map_err(|err| anyhow!("the app stopped unexpectedly: {err}")),
        None => {
            app.abort();
            tracing::warn!("quit without waiting for the controller's pending call");
            Ok(Ok(()))
        }
    };
    forward.abort();
    screen?;
    app??;
    let next = reopen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    Ok(next)
}

/// The controller as it finished, or `None` once `grace` passes or `stop` comes first, such as
/// another Ctrl+C after the screen closed.
async fn wait_for_controller<T>(
    app: &mut JoinHandle<T>,
    grace: Duration,
    stop: impl Future<Output = ()>,
) -> Option<Result<T, JoinError>> {
    tokio::select! {
        joined = tokio::time::timeout(grace, app) => joined.ok(),
        () = stop => None,
    }
}

/// The registry and the workers follow the settings, so a change made in the app, such as
/// turning an agent on, reaches the next chat turn and picker without a restart.
fn services(env: &Arc<AppEnv>, settings: &AppSettings) -> anyhow::Result<AppServices> {
    let (conductor, approvals) = client::for_app(Arc::clone(env))?;
    let (quota, health) = argonauts_conductor::boards_with(&env.paths, &settings.settings().health);
    let repo_models = RepoModels::new(&env.repo_root, &env.state_dir);
    let registry: Arc<dyn ModelRegistry> = Arc::new(LiveRegistry::new(
        settings.follow(),
        Some(repo_models.clone()),
        Some(env.paths.models_cache()),
    ));
    let workers = host::services::directory(
        SettingsSource::live(settings),
        &env.paths,
        Some(&repo_models),
        Arc::clone(&registry),
        Arc::clone(&quota),
        Arc::clone(&health),
    );
    Ok(AppServices {
        conductor,
        approvals,
        workers,
        steps: Arc::new(SchemaStepRunner),
        registry,
        quota,
        health,
        clock: Arc::new(SystemClock),
    })
}

/// The header first, then the one-time notices. The names go before the header, so it names
/// the agent as the rest of the screen will.
async fn greet(ctx: &mut Ctx) {
    crate::controller::names::send(ctx);
    let going = runs_going(ctx.services.conductor.as_ref()).await;
    let hello = welcome(ctx, going).await;
    ctx.send(hello);
    for (level, text) in start_notices(ctx) {
        ctx.notice(level, text);
    }
}

/// Runs going or waiting in this repo. A list that fails counts none.
pub(crate) async fn runs_going(conductor: &dyn ConductorApi) -> u32 {
    let Ok(runs) = conductor.list().await else {
        return 0;
    };
    let going = runs.iter().filter(|run| {
        matches!(
            run.state,
            RunState::Running | RunState::Pausing | RunState::Parked
        )
    });
    u32::try_from(going.count()).unwrap_or(u32::MAX)
}

/// The startup header: version, repo, preset, each harness's readiness, and runs going.
pub(crate) async fn welcome(ctx: &Ctx, runs_going: u32) -> AppEvent {
    let report = doctor(&builtin_adapters(), ctx.settings()).await;
    welcome_from(ctx, &report, runs_going)
}

pub(crate) fn welcome_from(ctx: &Ctx, report: &DoctorReport, runs_going: u32) -> AppEvent {
    let settings = ctx.settings();
    let (default_agent, effort) = header_agent(ctx);
    AppEvent::Welcome {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        repo: ctx.env.display_repo(),
        preset: settings.preset.clone(),
        agents: readiness(report, settings),
        runs_going,
        example_prompt: example_prompt(&ctx.env.repo_root),
        default_agent: Some(default_agent),
        effort,
    }
}

/// A CLI you don't have and your settings never name stays out, so the header has no note
/// about it.
fn readiness(report: &DoctorReport, settings: &Settings) -> Vec<AgentReady> {
    report
        .agents
        .iter()
        .filter(|check| check.status != AgentStatus::NotInstalled || uses(settings, &check.id))
        .map(|check| agent_ready(check, settings))
        .collect()
}

/// The chat's agent, the crew, and every worker a role or a fallback names.
fn uses(settings: &Settings, id: &str) -> bool {
    let roles = settings.roles.values();
    let fallbacks = roles
        .clone()
        .flat_map(|role| &role.handoff.fallback)
        .chain(&settings.handoff.fallback)
        .map(FallbackEntry::worker);
    std::iter::once(&settings.default_agent)
        .chain(&settings.crew)
        .chain(roles.flat_map(|role| &role.workers))
        .chain(fallbacks)
        .any(|worker| worker.worker == id)
}

/// Ready is installed and turned on. An untested version is ready with a warning, since it may
/// still work.
fn agent_ready(check: &AgentCheck, settings: &Settings) -> AgentReady {
    let enabled = settings
        .agents
        .get(&check.id)
        .is_none_or(|agent| agent.enabled);
    let (ready, note) = if !enabled {
        (false, Some("turned off".to_owned()))
    } else {
        match check.status {
            AgentStatus::NotInstalled => (false, Some("not installed".to_owned())),
            AgentStatus::Tested => (true, None),
            AgentStatus::Untested => (true, Some("untested version".to_owned())),
            AgentStatus::UnknownVersion => (
                true,
                Some(
                    check
                        .problem
                        .clone()
                        .unwrap_or_else(|| "unknown version".to_owned()),
                ),
            ),
        }
    };
    AgentReady {
        id: check.id.clone(),
        version: check.version.clone(),
        ready,
        note,
    }
}

/// What resolving the settings warned of, then the one-time notices: an edited preset file, the
/// old settings file, a committed local file.
pub(crate) fn start_notices(ctx: &mut Ctx) -> Vec<(NoteLevel, String)> {
    let env = &ctx.env;
    let mut notices = settings_warnings(&ctx.settings.resolved().warnings);
    notices.extend(preset_notices(&env.paths.global_presets()));
    if let Ok(old) = old_global_config_path()
        && let Some(notice) = old_config_notice(&old, env)
    {
        notices.push(notice);
    }
    if let Some(notice) = tracked_local_notice(env) {
        notices.push(notice);
    }
    notices
}

/// Each warning on every start, since each one is a setting you wrote that does nothing, such as
/// a typo or a safety key in a file you haven't trusted.
pub(crate) fn settings_warnings(warnings: &[String]) -> Vec<(NoteLevel, String)> {
    warnings
        .iter()
        .map(|warning| (NoteLevel::Warn, sentence(warning)))
        .collect()
}

/// `resolve` words its warnings for a log line, starting in lowercase.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Only `~/.argonauts/presets/` is upgraded: a project's presets are the team's, reviewed and
/// committed, and never rewritten.
fn preset_notices(dir: &Path) -> Vec<(NoteLevel, String)> {
    if !dir.is_dir() {
        return Vec::new();
    }
    match upgrade_presets(dir) {
        Ok(upgrades) => upgrades.iter().filter_map(edited_preset_notice).collect(),
        Err(err) => vec![(
            NoteLevel::Warn,
            format!("Couldn't check your preset files for this version: {err}"),
        )],
    }
}

/// The registry tells only the first time it keeps an edited file, so this shows once.
fn edited_preset_notice(upgrade: &PresetUpgrade) -> Option<(NoteLevel, String)> {
    if upgrade.outcome != (PresetUpgradeOutcome::KeptEdited { tell: true }) {
        return None;
    }
    let name = &upgrade.name;
    Some((
        NoteLevel::Info,
        format!("Your {name} preset differs from this version's · /preset diff {name}"),
    ))
}

fn old_config_notice(old: &Path, env: &AppEnv) -> Option<(NoteLevel, String)> {
    if !old.is_file() || !first_time(env.paths.home(), OLD_CONFIG_TOLD) {
        return None;
    }
    Some((
        NoteLevel::Warn,
        format!(
            "Nothing reads {} anymore. Move its keys into {}.",
            env.display(old),
            env.display(&env.global_config())
        ),
    ))
}

fn tracked_local_notice(env: &AppEnv) -> Option<(NoteLevel, String)> {
    let local = env.project_local_config();
    let relative = local.strip_prefix(&env.repo_root).ok()?;
    if !crate::git::tracks(&env.repo_root, relative).unwrap_or(false)
        || !first_time(&env.state_dir, TRACKED_LOCAL_TOLD)
    {
        return None;
    }
    Some((
        NoteLevel::Warn,
        format!(
            "Git tracks {0}, so .git/info/exclude can't hide it and your \"just me\" settings \
             reach your team. To stop that: git rm --cached {0}",
            relative.display()
        ),
    ))
}

/// True once per marker in `dir`. A marker that can't be written still counts as told, so a
/// notice never repeats on every start.
fn first_time(dir: &Path, marker: &str) -> bool {
    let path = dir.join(marker);
    if path.exists() {
        return false;
    }
    let _ = create_private_dir_all(dir).and_then(|()| write_private_file(&path, b""));
    true
}

#[cfg(test)]
mod tests {
    use argonauts_conductor::GitCheck;
    use argonauts_core::Paths;
    use argonauts_core::future::BoxFuture;
    use argonauts_core::limits::CostEstimate;
    use argonauts_core::model_catalog::{CATALOG_VERSION, CatalogEntry, ModelCatalog};
    use argonauts_core::names::{RoleName, WorkerRef};
    use argonauts_core::registry::ModelEntry;
    use argonauts_testkit::TempRepo;

    use super::*;
    use crate::controller::test_support::{HarnessOptions, TestCtx};

    /// Quitting waits for the controller's last call only up to the grace, and not at all once
    /// a stop signal comes, as one does when Ctrl+C is pressed again after the screen closed.
    #[tokio::test]
    async fn quitting_waits_for_the_controller_until_the_grace_or_a_stop() {
        let mut done = tokio::spawn(async { 7 });
        let joined =
            wait_for_controller(&mut done, Duration::from_secs(60), std::future::pending()).await;
        assert_eq!(joined.map(Result::unwrap), Some(7));

        let started = std::time::Instant::now();
        let mut stuck = tokio::spawn(std::future::pending::<()>());
        let stopped = wait_for_controller(&mut stuck, Duration::from_secs(60), async {}).await;
        assert!(stopped.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));

        let waited = wait_for_controller(
            &mut stuck,
            Duration::from_millis(20),
            std::future::pending(),
        )
        .await;
        assert!(waited.is_none());
        stuck.abort();
    }

    fn check(id: &str, status: AgentStatus) -> AgentCheck {
        AgentCheck {
            id: id.to_owned(),
            display_name: id.to_owned(),
            binary: id.to_owned(),
            path: None,
            version: (status != AgentStatus::NotInstalled).then(|| "1.0.0".to_owned()),
            status,
            tested_versions: vec!["1.0.0".to_owned()],
            cheapest_model: "small".to_owned(),
            cheapest_effort: None,
            problem: None,
            enabled: true,
        }
    }

    fn env_for(repo: &TempRepo) -> AppEnv {
        let mut env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap();
        env.user_home = Some(repo.user_home().to_owned());
        env
    }

    #[test]
    fn readiness_follows_install_and_enabled() {
        let mut settings = Settings::default();
        if let Some(codex) = settings.agents.get_mut("codex") {
            codex.enabled = false;
        }
        let report = DoctorReport {
            agents: vec![
                check("claude-code", AgentStatus::Tested),
                check("codex", AgentStatus::Tested),
                check("antigravity-cli", AgentStatus::Untested),
                check("opencode", AgentStatus::NotInstalled),
            ],
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let ready: Vec<(bool, Option<String>)> = readiness(&report, &settings)
            .into_iter()
            .map(|agent| (agent.ready, agent.note))
            .collect();
        assert_eq!(
            ready,
            vec![
                (true, None),
                (false, Some("turned off".to_owned())),
                (true, Some("untested version".to_owned())),
            ]
        );
    }

    #[test]
    fn a_missing_cli_is_noted_only_when_your_settings_use_it() {
        let report = |missing: &str| DoctorReport {
            agents: vec![
                check("claude-code", AgentStatus::Tested),
                check(missing, AgentStatus::NotInstalled),
            ],
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let noted = |settings: &Settings, missing: &str| {
            readiness(&report(missing), settings)
                .iter()
                .any(|agent| agent.id == missing)
        };
        let mut settings = Settings::default();
        assert!(!noted(&settings, "opencode"), "nothing names opencode");
        assert!(noted(&settings, "codex"), "codex is in the crew");

        settings.crew.push("opencode".parse().unwrap());
        assert!(noted(&settings, "opencode"), "in the crew");

        let settings = Settings {
            default_agent: "opencode/glm-5".parse().unwrap(),
            ..Settings::default()
        };
        assert!(noted(&settings, "opencode"), "the chat's agent");

        let mut settings = Settings::default();
        let mut role = settings
            .roles
            .get(&RoleName::Default)
            .cloned()
            .unwrap_or_default();
        role.handoff.fallback = vec![FallbackEntry::Worker("opencode".parse().unwrap())];
        settings.roles.insert(RoleName::Default, role);
        assert!(noted(&settings, "opencode"), "a fallback");
    }

    #[test]
    fn the_welcome_names_the_chats_agent_model_and_effort() {
        let (parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(
                    repo.global_config(),
                    "default_agent = \"codex\"\n\n[agents.codex]\nmodel = \"gpt-5.6-luna\"\n\
                     effort = \"low\"\n",
                )
                .unwrap();
            })),
            ..HarnessOptions::default()
        });
        let report = DoctorReport {
            agents: Vec::new(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let AppEvent::Welcome {
            default_agent,
            effort,
            ..
        } = welcome_from(&parts.ctx, &report, 0)
        else {
            panic!("a welcome");
        };
        assert_eq!(
            default_agent.map(|agent| agent.to_string()).as_deref(),
            Some("codex/gpt-5.6-luna")
        );
        assert_eq!(effort.as_deref(), Some("low"));
    }

    /// With nothing set, the header still names the model and effort: those of the shipped
    /// catalog's default, since this registry's CLIs mark none.
    #[test]
    fn the_welcome_names_a_model_and_effort_you_never_set() {
        let (parts, _) = TestCtx::new(HarnessOptions::default());
        let report = DoctorReport {
            agents: Vec::new(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let AppEvent::Welcome {
            default_agent,
            effort,
            ..
        } = welcome_from(&parts.ctx, &report, 0)
        else {
            panic!("a welcome");
        };
        let default_agent = default_agent.unwrap();
        let worker = &parts.ctx.settings().default_agent.worker;
        assert_eq!(default_agent.worker, *worker);
        let shipped = argonauts_registry::models::shipped_catalog();
        let harness = WorkerRef::new(worker.clone(), None);
        let runs = shipped.entry(&harness).and_then(|entry| entry.runs.clone());
        assert!(runs.is_some(), "the catalog names what {worker} runs");
        assert_eq!(default_agent.model, runs);
        let starts_at = shipped
            .entry(&default_agent)
            .and_then(|entry| entry.default_effort.clone());
        assert!(
            starts_at.is_some(),
            "the catalog says where {default_agent} starts"
        );
        assert_eq!(effort, starts_at);
    }

    /// What `codex debug models` lists, in its order: gpt-6.1-sol has the lowest priority.
    fn codex_catalog() -> Arc<dyn ModelRegistry> {
        let registry = argonauts_testkit::services::StaticRegistry::default();
        let json = r#"{"models": [
            {"slug": "gpt-6-astra", "priority": 2, "default_reasoning_level": "medium"},
            {"slug": "gpt-6.1-sol", "priority": 1, "default_reasoning_level": "low"},
            {"slug": "gpt-5.5", "priority": 13, "default_reasoning_level": "high"}
        ]}"#;
        let entries = argonauts_registry::models::discovery::parse_codex_models(json).unwrap();
        for entry in entries {
            registry.add(entry);
        }
        Arc::new(registry)
    }

    /// A registry whose catalog has Claude Code's "Default (recommended)" running Opus 5.5,
    /// which starts at xhigh.
    struct WithCatalog(Arc<dyn ModelRegistry>);

    impl ModelRegistry for WithCatalog {
        fn get(&self, worker: &WorkerRef) -> Option<ModelEntry> {
            self.0.get(worker)
        }
        fn family(&self, worker: &WorkerRef) -> String {
            self.0.family(worker)
        }
        fn list(&self) -> Vec<ModelEntry> {
            self.0.list()
        }
        fn estimate(&self, worker: &WorkerRef, input: u64, output: u64) -> CostEstimate {
            self.0.estimate(worker, input, output)
        }
        fn discover(&self) -> BoxFuture<'_, Vec<ModelEntry>> {
            self.0.discover()
        }
        fn catalog(&self) -> ModelCatalog {
            let mut default = CatalogEntry::new("claude-code".parse().unwrap());
            default.recommended = Some(true);
            default.runs = Some("claude-opus-5-5".to_owned());
            let mut opus = CatalogEntry::new("claude-code/claude-opus-5-5".parse().unwrap());
            opus.default_effort = Some("xhigh".to_owned());
            ModelCatalog {
                version: CATALOG_VERSION,
                models: vec![default, opus],
                efforts: Vec::new(),
            }
        }
    }

    fn header_of(config: &'static str) -> (Option<String>, Option<String>) {
        let (parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(move |repo: &TempRepo| {
                std::fs::write(repo.global_config(), config).unwrap();
            })),
            registry: Some(Arc::new(WithCatalog(codex_catalog()))),
            ..HarnessOptions::default()
        });
        let report = DoctorReport {
            agents: Vec::new(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let AppEvent::Welcome {
            default_agent,
            effort,
            ..
        } = welcome_from(&parts.ctx, &report, 0)
        else {
            panic!("a welcome");
        };
        (default_agent.map(|agent| agent.to_string()), effort)
    }

    #[test]
    fn the_welcome_names_the_model_a_cli_runs_when_none_is_set() {
        let (agent, effort) = header_of("default_agent = \"codex\"\n");
        assert_eq!(agent.as_deref(), Some("codex/gpt-6.1-sol"));
        assert_eq!(effort.as_deref(), Some("low"));

        let (agent, effort) =
            header_of("default_agent = \"codex\"\n\n[agents.codex]\neffort = \"xhigh\"\n");
        assert_eq!(agent.as_deref(), Some("codex/gpt-6.1-sol"));
        assert_eq!(effort.as_deref(), Some("xhigh"), "the effort you set wins");

        let (agent, effort) =
            header_of("default_agent = \"codex\"\n\n[agents.codex]\nmodel = \"gpt-5.5\"\n");
        assert_eq!(agent.as_deref(), Some("codex/gpt-5.5"));
        assert_eq!(
            effort.as_deref(),
            Some("high"),
            "the level Codex starts gpt-5.5 at"
        );
    }

    /// Claude Code's list is its aliases, none of them marked, so the catalog's default says
    /// which model it runs and the level that model starts at.
    #[test]
    fn the_welcome_names_the_catalogs_default_when_a_cli_marks_none() {
        let (agent, effort) = header_of("default_agent = \"claude-code\"\n");
        assert_eq!(agent.as_deref(), Some("claude-code/claude-opus-5-5"));
        assert_eq!(effort.as_deref(), Some("xhigh"));

        let (agent, effort) = header_of(
            "default_agent = \"claude-code\"\n\n[agents.claude-code]\neffort = \"low\"\n",
        );
        assert_eq!(agent.as_deref(), Some("claude-code/claude-opus-5-5"));
        assert_eq!(effort.as_deref(), Some("low"), "the effort you set wins");

        // A CLI's own mark wins over the catalog, and one the catalog doesn't know stays bare.
        let (agent, _) = header_of("default_agent = \"codex\"\n");
        assert_eq!(agent.as_deref(), Some("codex/gpt-6.1-sol"));
        let (agent, effort) = header_of("default_agent = \"antigravity-cli\"\n");
        assert_eq!(agent.as_deref(), Some("antigravity-cli"));
        assert_eq!(effort, None);
    }

    #[test]
    fn the_welcome_carries_an_example_from_recent_commits() {
        let (parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                repo.write("export.rs", "fn main() {}\n").unwrap();
                repo.commit_all("fix(export): fix the flaky export test")
                    .unwrap();
            })),
            ..HarnessOptions::default()
        });
        let report = DoctorReport {
            agents: Vec::new(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let AppEvent::Welcome { example_prompt, .. } = welcome_from(&parts.ctx, &report, 0) else {
            panic!("a welcome");
        };
        assert_eq!(
            example_prompt.as_deref(),
            Some("add a regression test for the flaky export test")
        );
    }

    #[test]
    fn each_settings_warning_is_a_notice_at_start() {
        let (mut parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(repo.global_config(), "[ui]\nthme = \"dark\"\n").unwrap();
            })),
            ..HarnessOptions::default()
        });
        let notices = start_notices(&mut parts.ctx);
        assert!(
            notices.iter().any(|(level, text)| *level == NoteLevel::Warn
                && text.starts_with("Unknown setting ui.thme ")),
            "{notices:?}"
        );
    }

    #[test]
    fn the_old_settings_file_is_told_once() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo);
        let old = repo.config_home().join("argonauts").join("config.toml");
        assert!(old_config_notice(&old, &env).is_none());
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "default_agent = \"codex\"\n").unwrap();
        let (level, text) = old_config_notice(&old, &env).unwrap();
        assert_eq!(level, NoteLevel::Warn);
        assert!(text.starts_with("Nothing reads "), "{text}");
        assert!(text.contains("config.toml"), "{text}");
        assert!(old_config_notice(&old, &env).is_none());
    }

    #[test]
    fn a_tracked_local_file_is_warned_once() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo);
        assert!(tracked_local_notice(&env).is_none());
        repo.write(".argonauts/config.local.toml", "[ui]\nbell = false\n")
            .unwrap();
        assert!(tracked_local_notice(&env).is_none(), "not tracked yet");
        repo.commit_all("share my settings").unwrap();
        let (_, text) = tracked_local_notice(&env).unwrap();
        assert!(text.contains(".argonauts/config.local.toml"), "{text}");
        assert!(text.contains(".git/info/exclude"), "{text}");
        assert!(tracked_local_notice(&env).is_none());
    }

    #[test]
    fn an_edited_preset_is_told_only_when_the_registry_says_so() {
        let upgrade = |outcome| PresetUpgrade {
            name: "peer-crew".to_owned(),
            path: "/home/presets/peer-crew.toml".into(),
            outcome,
        };
        let told = edited_preset_notice(&upgrade(PresetUpgradeOutcome::KeptEdited { tell: true }));
        assert_eq!(
            told,
            Some((
                NoteLevel::Info,
                "Your peer-crew preset differs from this version's · /preset diff peer-crew"
                    .to_owned()
            ))
        );
        for quiet in [
            PresetUpgradeOutcome::KeptEdited { tell: false },
            PresetUpgradeOutcome::Updated,
            PresetUpgradeOutcome::Current,
        ] {
            assert_eq!(edited_preset_notice(&upgrade(quiet)), None);
        }
    }

    #[test]
    fn presets_are_checked_only_in_the_global_folder_you_have() {
        let dir = tempfile::tempdir().unwrap();
        assert!(preset_notices(&dir.path().join("presets")).is_empty());
    }
}
