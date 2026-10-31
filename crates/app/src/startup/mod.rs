//! `run_interactive`: refuse inside an agent, resolve the settings with each project file's
//! trust, start the screen, say hello, and hand over to the controller.

pub(crate) mod ancestry;
pub(crate) mod app_settings;
pub(crate) mod env;
pub(crate) mod session_id;
pub(crate) mod trust;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use argonauts_conductor::boards::cli_versions::CliVersions;
use argonauts_conductor::{AgentCheck, AgentStatus, DoctorReport, doctor};
use argonauts_core::NoteLevel;
use argonauts_core::Paths;
use argonauts_core::app::{AgentReady, AppEvent};
use argonauts_core::base::clock::SystemClock;
use argonauts_core::base::paths::{
    create_private_dir_all, old_global_config_path, write_private_file,
};
use argonauts_core::base::text::sentence_case;
use argonauts_core::models::registry::ModelRegistry;
use argonauts_core::runs::api::ConductorApi;
use argonauts_core::settings::Settings;
use argonauts_core::settings::fallback::FallbackEntry;
use argonauts_core::workflows::steps::SchemaStepRunner;
use argonauts_registry::{
    SettingsWarning, WorkflowUpgrade, WorkflowUpgradeOutcome, upgrade_workflows,
};
use argonauts_tui::base::theme::{ColorEnv, Theme};
use argonauts_tui::cells::welcome;
use argonauts_tui::{AppIo, UiOptions, history_file_in, run_app, run_screen_reader, stop_signal};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};

use crate::controller::{AppServices, Controller, Ctx, Opening, Outbox, header_agent};
use crate::conversation::example_prompt::example_prompt;
use crate::host::clis::RepoModels;
use crate::host::services::{LiveRegistry, SettingsSource};
use crate::startup::ancestry::{AgentMarkers, check_self};
use crate::startup::app_settings::AppSettings;
use crate::startup::env::{AppEnv, FolderGit};
use crate::testing::trace::Trace;
use crate::{FirstView, InteractiveOptions, builtin_adapters, client, host, needs_guided_setup};

/// Commands the screen may queue before the controller reads them.
const COMMAND_QUEUE: usize = 64;
/// Events on their way to the screen. The controller's own queue is unbounded, so it never
/// waits on a slow terminal.
const EVENT_QUEUE: usize = 256;

/// How long quitting waits for the controller. It is longer than the controller's own wait
/// for a cancelled conversation step to stop its agent process.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// Markers for the one-time start notices, so each shows on one start and not the next.
const OLD_CONFIG_TOLD: &str = "told-old-config";
const TRACKED_LOCAL_TOLD: &str = "told-tracked-local-config";
const NO_GIT_TOLD: &str = "told-no-git";

pub(crate) async fn run(mut options: InteractiveOptions, first: FirstView) -> anyhow::Result<()> {
    let mut first = first;
    // A hold `/awake` asked for lasts across `/cd` and ends when the app does.
    let awake = crate::host::keep_awake::YourHold::default();
    // `/cd` closes the screen and asks for it to open again in another repo.
    while let Some(repo) = run_once(&options, first, &awake).await? {
        std::env::set_current_dir(&repo)
            .with_context(|| format!("moving to {}", repo.display()))?;
        options.repo = Some(repo);
        options.guided_setup = false;
        first = FirstView::Conversation;
    }
    Ok(())
}

/// One screen, from start to close. `Some` is where `/cd` asked the app to open next.
async fn run_once(
    options: &InteractiveOptions,
    first: FirstView,
    awake: &crate::host::keep_awake::YourHold,
) -> anyhow::Result<Option<PathBuf>> {
    let opened = std::time::Instant::now();
    let env = AppEnv::discover(options.repo.as_deref())?;
    crate::prompts::catalog::use_files_of(&env);
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
    let mut ui = UiOptions::from_settings(&settings.settings().ui);
    ui.history_file = Some(history_file_in(&env.state_dir));
    ui.ultracode_trigger = settings.settings().workflows.ultracode_trigger;
    ui.ask_for_pictures = settings.settings().behaviors.view
        == argonauts_core::settings::behaviors::BehaviorView::World;
    let screen_reader =
        options.screen_reader || settings.settings().ui.screen_reader || !env.interactive;
    // `argonauts run` attaches to the run it just started and never opens the setup.
    let guided_setup = options.guided_setup
        || (!matches!(first, FirstView::Attach(_)) && needs_guided_setup(&env.paths));
    // The usage line shows once, at the first start, before any report goes.
    let usage_line = crate::telemetry::notice::claim(&env.paths, settings.settings());
    let var = |name: &str| std::env::var(name).ok();
    let reader = screen_reader || ui.screen_reader;
    let top = if may_greet(guided_setup, reader, &var) && claim_greeting(&env.paths) {
        ui.greeting = Some(greeting(usage_line));
        Top::Greeting
    } else {
        Top::Header { usage_line }
    };

    let env = Arc::new(env);
    let services = services(&env, &settings)?;
    let trace = Trace::from_env().context("opening the test trace")?;
    let (out, mut outgoing) = mpsc::unbounded_channel();
    let (internal, internal_rx) = mpsc::unbounded_channel();
    let mut ctx = Ctx::new(
        env,
        settings,
        services,
        Outbox::new(out, Arc::new(trace)),
        internal,
    );
    ctx.awake = crate::host::keep_awake::KeepAwake::system().sharing(awake.clone());
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
        greet(&mut ctx, top, opened).await;
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
    // to a run's run host holds it. The run goes on without this app, so quitting doesn't wait.
    // Screen-reader mode never caught the stop signals, so they still end the app at once.
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
/// turning an agent on, reaches the next conversation turn and picker without a restart.
fn services(env: &Arc<AppEnv>, settings: &AppSettings) -> anyhow::Result<AppServices> {
    let (conductor, approvals) = client::for_app(Arc::clone(env))?;
    let repo_models = RepoModels::new(&env.repo_root, &env.state_dir);
    let registry: Arc<dyn ModelRegistry> = Arc::new(LiveRegistry::new(
        settings.follow(),
        Some(repo_models.clone()),
        Some(env.paths.models_cache()),
    ));
    let (plan_usage, health) =
        argonauts_conductor::boards_with(&env.paths, &settings.settings().health, registry.clone());
    let workers = host::services::directory(
        SettingsSource::live(settings),
        &env.paths,
        Some(&repo_models),
        Arc::clone(&registry),
        Arc::clone(&plan_usage),
        Arc::clone(&health),
    );
    Ok(AppServices {
        conductor,
        approvals,
        workers,
        steps: Arc::new(SchemaStepRunner),
        registry,
        plan_usage,
        health,
        clock: Arc::new(SystemClock),
    })
}

/// What a start shows on top, before the one-time notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Top {
    /// The header, with the usage line under it when this start shows it.
    Header { usage_line: bool },
    /// The greeting the screen printed, which holds the usage line. The header follows the
    /// setup, which sends it as it saves.
    Greeting,
}

/// The first start in a terminal greets you before its setup, never in CI.
fn may_greet(
    guided_setup: bool,
    screen_reader: bool,
    var: &dyn Fn(&str) -> Option<String>,
) -> bool {
    guided_setup && !screen_reader && !in_ci(var)
}

/// The greeting, with the usage line under it when this start shows that too.
fn greeting(usage_line: bool) -> welcome::Greeting {
    use crate::telemetry::notice::{LINE, STOP_COMMAND};
    welcome::Greeting {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        usage: usage_line.then(|| (LINE.to_owned(), STOP_COMMAND.to_owned())),
        picture: false,
    }
}

/// The header first, unless the greeting stands on top, then the usage line when this start
/// shows it, then the notices that read only local files, so they come with the first frame.
/// The CLI checks run after: what they find goes into the header already on screen, and the
/// notices about the CLIs come last. The names go before the header, so it names the agent as
/// the rest of the screen will.
async fn greet(ctx: &mut Ctx, top: Top, opened: std::time::Instant) {
    crate::controller::clis::names::send(ctx);
    if let Top::Header { usage_line } = top {
        ctx.send(header_from_settings(ctx, Vec::new(), RunCounts::default()));
        if usage_line {
            ctx.notice(NoteLevel::Quiet, crate::telemetry::notice::LINE);
        }
    }
    say_it_updated(ctx);
    for (level, text) in crate::self_update::daily::at_start(&ctx.env.paths, ctx.settings()) {
        ctx.notice(level, text);
    }
    for (level, text) in start_notices(ctx) {
        ctx.notice(level, text);
    }
    let report = doctor(&builtin_adapters(), ctx.settings()).await;
    let found = crate::telemetry::found(Some(&ctx.env), &report);
    crate::telemetry::send_when_due(&ctx.env.paths, ctx.settings(), found);
    ctx.cli_facts.checked(report.clone());
    crate::telemetry::count(ctx, crate::telemetry::Use::started(opened.elapsed()));
    if matches!(top, Top::Header { .. }) {
        let runs = run_counts(ctx.services.conductor.as_ref()).await;
        ctx.send(welcome_from(ctx, &report, runs));
    }
    for text in newer_cli_notices(ctx, &report) {
        ctx.notice(NoteLevel::Info, text);
    }
    for (level, text) in crate::controller::clis::cli_updates::start_lines(ctx, &report) {
        ctx.notice(level, text);
    }
    crate::controller::clis::cli_updates::ask_newest(ctx, &report);
}

/// The first start after a newer version took over says so above the input, once.
fn say_it_updated(ctx: &Ctx) {
    if let Some(words) = crate::self_update::daily::updated_row(&ctx.env.paths) {
        ctx.send(AppEvent::Updated { words });
    }
}

/// Once per new version, each installed CLI newer than the newest one its adapter was checked
/// with. Only start-up says it: a session never does, so nothing about versions is said while
/// work runs.
fn newer_cli_notices(ctx: &Ctx, report: &DoctorReport) -> Vec<String> {
    if !ctx.settings().ui.newer_cli_notice {
        return Vec::new();
    }
    let path = ctx.env.paths.cli_versions();
    let adapters = builtin_adapters();
    report
        .agents
        .iter()
        // Later than every recording, as /doctor judges it: a later patch, minor or major.
        .filter(|check| matches!(check.status, AgentStatus::Newer | AgentStatus::Untested))
        .filter_map(|check| {
            let adapter = adapters.iter().find(|adapter| adapter.id() == check.id)?;
            CliVersions::notice_once(&path, adapter.as_ref(), check.version.as_deref()?)
        })
        .collect()
}

/// This repo's runs the header names: those that go on without you, and those that wait on you.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RunCounts {
    pub going: u32,
    pub need_you: u32,
}

/// This repo's runs going or waiting on you, in any conversation. A new conversation's panel
/// holds only its own runs, so the header and /resume are where earlier ones show. A list that
/// fails counts none.
pub(crate) async fn run_counts(conductor: &dyn ConductorApi) -> RunCounts {
    let Ok(runs) = conductor.list().await else {
        return RunCounts::default();
    };
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let shown: Vec<_> = runs
        .iter()
        .filter(|run| crate::controller::runs::going_or_waiting(run))
        .collect();
    let need_you = shown.iter().filter(|run| run.needs_you()).count();
    RunCounts {
        going: count(shown.len() - need_you),
        need_you: count(need_you),
    }
}

/// The startup header: version, repo, workflow, each CLI's readiness, and the runs going or
/// waiting on you.
pub(crate) fn welcome_from(ctx: &Ctx, report: &DoctorReport, runs: RunCounts) -> AppEvent {
    header_from_settings(ctx, readiness(report, ctx.settings()), runs)
}

/// The header from the settings, with the CLIs' readiness and the runs as far as they're known.
fn header_from_settings(ctx: &Ctx, agents: Vec<AgentReady>, runs: RunCounts) -> AppEvent {
    let settings = ctx.settings();
    let (main_agent, effort) = header_agent(ctx);
    AppEvent::Welcome {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        repo: ctx.env.display_repo(),
        workflow: settings.workflow.clone(),
        agents,
        runs_going: runs.going,
        runs_need_you: runs.need_you,
        example_prompt: example_prompt(&ctx.env.repo_root),
        main_agent: Some(main_agent),
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

/// The conversation's agent, the crew, and every worker a role or a fallback names.
fn uses(settings: &Settings, id: &str) -> bool {
    let roles = settings.roles.values();
    let fallbacks = settings
        .fallback
        .roles
        .values()
        .flat_map(|role| &role.order)
        .chain(&settings.fallback.order)
        .map(FallbackEntry::model);
    let members = settings.crew.members();
    std::iter::once(&settings.crew.main)
        .chain(&members)
        .chain(roles.flat_map(|role| &role.models))
        .chain(fallbacks)
        .any(|worker| worker.agent == id)
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
            // A later patch should work, and the notice says so once, not every start.
            AgentStatus::Tested | AgentStatus::Newer => (true, None),
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

/// What resolving the settings warned of, then the one-time notices: an edited workflow file, the
/// old settings file, a committed local file.
pub(crate) fn start_notices(ctx: &Ctx) -> Vec<(NoteLevel, String)> {
    let env = &ctx.env;
    let warnings = ctx.settings.resolved().warnings.iter();
    let mut notices = settings_warnings(
        warnings.filter(|warning| !crate::controller::settings::said_by_a_question(ctx, warning)),
    );
    notices.extend(workflow_notices(&env.paths.global_workflows()));
    notices.extend(crate::mods::stats_starter::opens_on_all(env));
    if let Ok(old) = old_global_config_path()
        && let Some(notice) = old_config_notice(&old, env)
    {
        notices.push(notice);
    }
    if let Some(notice) = tracked_local_notice(env) {
        notices.push(notice);
    }
    if let Some(notice) = no_git_notice(env) {
        notices.push(notice);
    }
    notices
}

/// Each warning on every start, since each one is a setting you wrote that does nothing, such as
/// a typo or a safety key in a file you haven't trusted.
pub(crate) fn settings_warnings<'a>(
    warnings: impl IntoIterator<Item = &'a SettingsWarning>,
) -> Vec<(NoteLevel, String)> {
    warnings
        .into_iter()
        // `resolve` words its warnings for a log line, starting in lowercase, often with the
        // key, which keeps its spelling.
        .map(|warning| (NoteLevel::Warn, sentence_case(warning)))
        .collect()
}

/// Only `~/.argonauts/workflows/` is upgraded: a project's workflows are the team's, reviewed and
/// committed, and never rewritten.
fn workflow_notices(dir: &Path) -> Vec<(NoteLevel, String)> {
    if !dir.is_dir() {
        return Vec::new();
    }
    match upgrade_workflows(dir) {
        Ok(upgrades) => upgrades.iter().filter_map(edited_workflow_notice).collect(),
        Err(err) => vec![(
            NoteLevel::Warn,
            format!("Couldn't check your workflow files for this version: {err}"),
        )],
    }
}

/// The registry tells only the first time it keeps an edited file, so this shows once.
fn edited_workflow_notice(upgrade: &WorkflowUpgrade) -> Option<(NoteLevel, String)> {
    if upgrade.outcome != (WorkflowUpgradeOutcome::KeptEdited { tell: true }) {
        return None;
    }
    let name = &upgrade.name;
    Some((
        NoteLevel::Info,
        format!("Your {name} workflow is different from this version's · /workflow diff {name}"),
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

/// Said the first time the app opens in a folder with no git, since it works differently
/// there: nothing is committed, and the history Captain and the workflows keep lives under
/// Argonauts Code's home.
fn no_git_notice(env: &AppEnv) -> Option<(NoteLevel, String)> {
    if env.git() != FolderGit::Off || !first_time(&env.state_dir, NO_GIT_TOLD) {
        return None;
    }
    Some((
        NoteLevel::Aside,
        format!(
            "This folder has no git. Captain and the workflows keep their own history in {}, so \
             /rewind works, and your folder stays as it is.",
            env.display(env.paths.home())
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

/// `argonauts welcome`: the first start's greeting, which the install script shows once it has
/// installed, with what to do next. Only on a terminal and never in CI, so a script that runs
/// it prints nothing. The first start then needn't greet again.
pub fn welcome() -> std::process::ExitCode {
    use std::io::IsTerminal as _;
    let var = |name: &str| std::env::var(name).ok();
    if !std::io::stdout().is_terminal() || in_ci(&var) {
        return std::process::ExitCode::SUCCESS;
    }
    if let Ok(paths) = Paths::from_env() {
        claim_greeting(&paths);
    }
    let theme = Theme::detect(&ColorEnv::from_process());
    let lines = welcome::greeting(env!("CARGO_PKG_VERSION"), Some(welcome::NEXT_STEP), &theme);
    // A blank line sets it apart from the installer's own lines above it.
    print!("\n{}", welcome::printed(&lines, theme.has_color()));
    std::process::ExitCode::SUCCESS
}

/// The marker that the greeting showed, beside the usage line's.
const GREETED: &str = "welcomed";

/// Whether this start is the one to greet: the first one, on a terminal, outside CI. Made with
/// `create_new`, so of two first starts together only one greets.
pub(crate) fn claim_greeting(paths: &Paths) -> bool {
    let marker = paths.home().join("state").join(GREETED);
    let made = marker
        .parent()
        .map_or(Ok(()), create_private_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .map(drop)
        });
    made.is_ok()
}

/// `CI` set to anything but empty, `0` or `false`.
fn in_ci(var: &dyn Fn(&str) -> Option<String>) -> bool {
    var("CI").is_some_and(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false"
        )
    })
}

#[cfg(test)]
mod tests {
    use argonauts_conductor::GitCheck;
    use argonauts_core::Paths;
    use argonauts_core::base::future::BoxFuture;
    use argonauts_core::base::names::ModelRef;
    use argonauts_core::models::limits::CostEstimate;
    use argonauts_core::models::model_catalog::{CATALOG_VERSION, CatalogEntry, ModelCatalog};
    use argonauts_core::models::registry::ModelEntry;
    use argonauts_core::settings::crew::CrewRole;
    use argonauts_testkit::TempRepo;

    use super::*;
    use crate::controller::base::test_support::{CliOptions, TestCtx};
    use argonauts_core::settings::crew::CrewSettings;

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

        settings.crew.add("opencode".parse().unwrap());
        assert!(noted(&settings, "opencode"), "in the crew");

        let settings = Settings {
            crew: CrewSettings {
                main: "opencode/glm-5".parse().unwrap(),
                ..CrewSettings::default()
            },
            ..Settings::default()
        };
        assert!(noted(&settings, "opencode"), "the conversation's agent");

        let mut settings = Settings::default();
        let role = settings.fallback.roles.entry(CrewRole::Main).or_default();
        role.order = vec![FallbackEntry::Model("opencode".parse().unwrap())];
        assert!(noted(&settings, "opencode"), "a fallback");
    }

    #[test]
    fn the_welcome_names_the_conversations_agent_model_and_effort() {
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(
                    repo.global_config(),
                    "crew.main = \"codex\"\n\n[agents.codex]\nmodel = \"gpt-5.6-luna\"\n\
                     effort = \"low\"\n",
                )
                .unwrap();
            })),
            ..CliOptions::default()
        });
        let report = DoctorReport {
            agents: Vec::new(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let AppEvent::Welcome {
            main_agent, effort, ..
        } = welcome_from(&parts.ctx, &report, RunCounts::default())
        else {
            panic!("a welcome");
        };
        assert_eq!(
            main_agent.map(|agent| agent.to_string()).as_deref(),
            Some("codex/gpt-5.6-luna")
        );
        assert_eq!(effort.as_deref(), Some("low"));
    }

    /// With nothing set, the header still names the model and effort: those of the shipped
    /// catalog's default, since this registry's CLIs mark none.
    #[test]
    fn the_welcome_names_a_model_and_effort_you_never_set() {
        let (parts, _) = TestCtx::new(CliOptions::default());
        let report = DoctorReport {
            agents: Vec::new(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let AppEvent::Welcome {
            main_agent, effort, ..
        } = welcome_from(&parts.ctx, &report, RunCounts::default())
        else {
            panic!("a welcome");
        };
        let main_agent = main_agent.unwrap();
        let worker = &parts.ctx.settings().crew.main.agent;
        assert_eq!(main_agent.agent, *worker);
        let shipped = argonauts_registry::models::shipped_catalog();
        let cli = ModelRef::new(worker.clone(), None);
        let runs = shipped.entry(&cli).and_then(|entry| entry.runs.clone());
        assert!(runs.is_some(), "the catalog names what {worker} runs");
        assert_eq!(main_agent.model, runs);
        let starts_at = shipped
            .entry(&main_agent)
            .and_then(|entry| entry.default_effort.clone());
        assert!(
            starts_at.is_some(),
            "the catalog says where {main_agent} starts"
        );
        assert_eq!(effort, starts_at);
    }

    /// What `codex debug models` lists, in its order: gpt-6.1-sol has the lowest priority.
    fn codex_catalog() -> Arc<dyn ModelRegistry> {
        let registry = argonauts_testkit::engine::services::StaticRegistry::default();
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
        fn get(&self, worker: &ModelRef) -> Option<ModelEntry> {
            self.0.get(worker)
        }
        fn provider(&self, worker: &ModelRef) -> String {
            self.0.provider(worker)
        }
        fn list(&self) -> Vec<ModelEntry> {
            self.0.list()
        }
        fn estimate(&self, worker: &ModelRef, input: u64, output: u64) -> CostEstimate {
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
                ..ModelCatalog::default()
            }
        }
    }

    fn header_of(config: &'static str) -> (Option<String>, Option<String>) {
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(move |repo: &TempRepo| {
                std::fs::write(repo.global_config(), config).unwrap();
            })),
            registry: Some(Arc::new(WithCatalog(codex_catalog()))),
            ..CliOptions::default()
        });
        let report = DoctorReport {
            agents: Vec::new(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let AppEvent::Welcome {
            main_agent, effort, ..
        } = welcome_from(&parts.ctx, &report, RunCounts::default())
        else {
            panic!("a welcome");
        };
        (main_agent.map(|agent| agent.to_string()), effort)
    }

    #[test]
    fn the_welcome_names_the_model_a_cli_runs_when_none_is_set() {
        let (agent, effort) = header_of("crew.main = \"codex\"\n");
        assert_eq!(agent.as_deref(), Some("codex/gpt-6.1-sol"));
        assert_eq!(effort.as_deref(), Some("low"));

        let (agent, effort) =
            header_of("crew.main = \"codex\"\n\n[agents.codex]\neffort = \"xhigh\"\n");
        assert_eq!(agent.as_deref(), Some("codex/gpt-6.1-sol"));
        assert_eq!(effort.as_deref(), Some("xhigh"), "the effort you set wins");

        let (agent, effort) =
            header_of("crew.main = \"codex\"\n\n[agents.codex]\nmodel = \"gpt-5.5\"\n");
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
        let (agent, effort) = header_of("crew.main = \"claude-code\"\n");
        assert_eq!(agent.as_deref(), Some("claude-code/claude-opus-5-5"));
        assert_eq!(effort.as_deref(), Some("xhigh"));

        let (agent, effort) =
            header_of("crew.main = \"claude-code\"\n\n[agents.claude-code]\neffort = \"low\"\n");
        assert_eq!(agent.as_deref(), Some("claude-code/claude-opus-5-5"));
        assert_eq!(effort.as_deref(), Some("low"), "the effort you set wins");

        // A CLI's own mark wins over the catalog, and one the catalog doesn't know stays bare.
        let (agent, _) = header_of("crew.main = \"codex\"\n");
        assert_eq!(agent.as_deref(), Some("codex/gpt-6.1-sol"));
        let (agent, effort) = header_of("crew.main = \"antigravity-cli\"\n");
        assert_eq!(agent.as_deref(), Some("antigravity-cli"));
        assert_eq!(effort, None);
    }

    #[test]
    fn the_welcome_carries_an_example_from_recent_commits() {
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                repo.write("export.rs", "fn main() {}\n").unwrap();
                repo.commit_all("fix(export): fix the flaky export test")
                    .unwrap();
            })),
            ..CliOptions::default()
        });
        let report = DoctorReport {
            agents: Vec::new(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        };
        let AppEvent::Welcome { example_prompt, .. } =
            welcome_from(&parts.ctx, &report, RunCounts::default())
        else {
            panic!("a welcome");
        };
        assert_eq!(
            example_prompt.as_deref(),
            Some("add a regression test for the flaky export test")
        );
    }

    #[test]
    fn each_settings_warning_is_a_notice_at_start() {
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(repo.global_config(), "[ui]\nthme = \"dark\"\n").unwrap();
            })),
            ..CliOptions::default()
        });
        let notices = start_notices(&parts.ctx);
        assert!(
            notices.iter().any(|(level, text)| *level == NoteLevel::Warn
                && text.starts_with("Unknown setting ui.thme ")),
            "{notices:?}"
        );
    }

    /// A key that went, a value no setting takes, and a role nobody has: the app starts, each
    /// is named and ignored, and the file stays as you wrote it.
    #[test]
    fn an_older_settings_file_starts_with_a_notice_for_each_name_it_ignores() {
        const OLDER: &str = "[goal]\nalways_ask = true\n\n\
            [drafting]\nby = \"foreman\"\n\n\
            [roles.foreman]\nmodels = [\"codex\"]\n";
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(repo.global_config(), OLDER).unwrap();
            })),
            ..CliOptions::default()
        });
        let settings = parts.ctx.settings.settings();
        assert_eq!(settings.drafting, Default::default());

        let notices = start_notices(&parts.ctx);
        let texts: Vec<&str> = notices.iter().map(|(_, text)| text.as_str()).collect();
        for named in ["goal.always_ask", "drafting.by", "\"foreman\""] {
            assert!(
                texts.iter().any(|text| text.contains(named)),
                "{named}: {texts:#?}"
            );
        }
        let now = std::fs::read_to_string(parts.repo.global_config()).unwrap();
        let set_up = crate::controller::base::test_support::SET_UP_GLOBAL;
        assert_eq!(now, format!("{set_up}{OLDER}"));
    }

    #[test]
    fn a_workflow_no_workflow_has_runs_the_default_with_one_notice() {
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(repo.global_config(), "workflow = \"retired-mode\"\n").unwrap();
            })),
            ..CliOptions::default()
        });
        assert_eq!(parts.ctx.settings.settings().workflow, "council");
        let notices = start_notices(&parts.ctx);
        let named: Vec<&str> = notices
            .iter()
            .map(|(_, text)| text.as_str())
            .filter(|text| text.contains("retired-mode"))
            .collect();
        let wanted = format!(
            "workflow = \"retired-mode\" in {} names no workflow, so the default is used; \
             /workflow lists the ones you can pick",
            parts.repo.global_config().display()
        );
        assert_eq!(named, [wanted.as_str()]);
    }

    #[test]
    fn a_warning_that_starts_with_a_key_keeps_its_spelling() {
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                repo.write(
                    ".argonauts/config.local.toml",
                    "[limits]\nspend_per_run = 1.0\n\n[checks]\nrun = [\"python3 -m unittest -q\"]\n",
                )
                .unwrap();
            })),
            ..CliOptions::default()
        });
        let notices = start_notices(&parts.ctx);
        let texts: Vec<&str> = notices.iter().map(|(_, text)| text.as_str()).collect();
        for key in ["limits.spend_per_run", "checks.run"] {
            let warning = format!(
                "{key} in .argonauts/config.local.toml is a safety setting and is ignored until \
                 you trust the file (/setup, or the trust question when it changes)"
            );
            assert!(texts.contains(&warning.as_str()), "{texts:#?}");
        }
    }

    #[test]
    fn the_old_settings_file_is_told_once() {
        let repo = TempRepo::new().unwrap();
        let env = env_for(&repo);
        let old = repo.config_home().join("argonauts").join("config.toml");
        assert!(old_config_notice(&old, &env).is_none());
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "crew.main = \"codex\"\n").unwrap();
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
    fn an_edited_workflow_is_told_only_when_the_registry_says_so() {
        let upgrade = |outcome| WorkflowUpgrade {
            name: "council".to_owned(),
            path: "/home/you/workflows/council.toml".into(),
            outcome,
        };
        let told =
            edited_workflow_notice(&upgrade(WorkflowUpgradeOutcome::KeptEdited { tell: true }));
        assert_eq!(
            told,
            Some((
                NoteLevel::Info,
                "Your council workflow is different from this version's · /workflow diff council"
                    .to_owned()
            ))
        );
        for quiet in [
            WorkflowUpgradeOutcome::KeptEdited { tell: false },
            WorkflowUpgradeOutcome::Updated,
            WorkflowUpgradeOutcome::Current,
        ] {
            assert_eq!(edited_workflow_notice(&upgrade(quiet)), None);
        }
    }

    #[test]
    fn workflows_are_checked_only_in_the_global_folder_you_have() {
        let dir = tempfile::tempdir().unwrap();
        assert!(workflow_notices(&dir.path().join("workflows")).is_empty());
    }

    /// The first start in a terminal greets, once. A later start, a screen reader and CI never
    /// do, and the greeting holds the usage line only when this start shows it.
    /// The first start after an update says so once; the very first start, and the starts
    /// after, say nothing of it.
    #[tokio::test]
    async fn the_first_start_after_an_update_says_so_once() {
        let (mut parts, _) = TestCtx::new(CliOptions::default());
        let updated = |parts: &mut TestCtx| {
            say_it_updated(&parts.ctx);
            let mut said = Vec::new();
            while let Ok(event) = parts.events.try_recv() {
                if let AppEvent::Updated { words } = event {
                    said.push(words);
                }
            }
            said
        };
        assert!(updated(&mut parts).is_empty(), "the very first start");
        let last = parts.env.paths.home().join("state").join("last-version");
        std::fs::write(&last, "0.0.1\n").unwrap();
        let running = env!("CARGO_PKG_VERSION");
        assert_eq!(
            updated(&mut parts),
            [format!("Updated to {running} · /release-notes")]
        );
        assert!(updated(&mut parts).is_empty(), "once");
    }

    #[test]
    fn only_the_first_start_in_a_terminal_greets() {
        let nothing = |_: &str| None;
        let ci = |value: &'static str| move |name: &str| (name == "CI").then(|| value.to_owned());
        assert!(may_greet(true, false, &nothing));
        assert!(may_greet(true, false, &ci("false")));
        assert!(!may_greet(false, false, &nothing), "a later start");
        assert!(!may_greet(true, true, &nothing), "a screen reader");
        assert!(!may_greet(true, false, &ci("true")), "CI");

        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        assert!(claim_greeting(&paths));
        assert!(!claim_greeting(&paths), "once");

        use crate::telemetry::notice::{LINE, STOP_COMMAND};
        assert_eq!(greeting(false).usage, None);
        let usage = Some((LINE.to_owned(), STOP_COMMAND.to_owned()));
        assert_eq!(greeting(true).usage, usage);
        assert_eq!(greeting(true).version, env!("CARGO_PKG_VERSION"));
    }
}
