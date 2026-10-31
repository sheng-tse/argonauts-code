//! `/doctor` in the app: each agent CLI by name with its version, read live, and its login, then
//! git. The same checks as `argonauts doctor` and the setup, run off the loop so a slow CLI never
//! holds the screen. It never calls a model. Claude Code's own checkup is `/checkup`, which runs
//! inside it. `/update` updates each CLI the way it was installed first, between runs only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use argonauts_conductor::GitCheck;
use argonauts_conductor::boards::cli_versions::{
    CliVersions, Installed, builtins_line, new_command, startup_warnings, unread_line,
    version_check,
};
use argonauts_conductor::doctor::update::{
    CliUpdate, UPDATE_TIMEOUT, UpdateColumns, UpdateLimits, each_once, update_each,
    update_lines_under,
};
use argonauts_core::Adapter;
use argonauts_core::app::status_tab::{VERSION_ROW_NAMES, VERSION_ROW_STATES, version_row};
use argonauts_core::app::{AppEvent, CommandWork};
use argonauts_core::base::log::NoteLevel;
use argonauts_core::settings::Settings;
use argonauts_registry::SettingsWarning;
use tokio_util::sync::CancellationToken;

use crate::controller::{Ctx, Effect};
use crate::self_update::{Here, OwnRow, Source, how_it_updates, own_row, running_version};
use crate::setup::probe::{self, FoundAgent, LoginState};

pub(crate) fn doctor(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    let adapters = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())?;
    let settings = ctx.settings.shared();
    let out = ctx.out.clone();
    let versions = ctx.env.paths.cli_versions();
    let prompt_files = crate::controller::library::prompt_templates::doctor_lines(ctx);
    let mods = crate::mods::doctor_lines(&ctx.env);
    let settings_lines = settings_lines(&ctx.settings.resolved().warnings);
    let paths = ctx.env.paths.clone();
    let unnamed = crate::controller::panels::status_tab::unnamed_accounts(ctx);
    tokio::spawn(async move {
        let mut lines = doctor_lines(&adapters, &settings, &versions).await;
        lines.extend(unnamed);
        lines.extend(settings_lines);
        lines.extend(prompt_files);
        lines.extend(mods);
        let import = tokio::task::spawn_blocking(move || crate::overview::doctor_lines(&paths));
        lines.extend(import.await.unwrap_or_default());
        out.send(AppEvent::Report {
            title: "Doctor".to_owned(),
            lines,
        });
    });
    Ok(Vec::new())
}

/// Each thing reading the settings warned of, such as a key that can't be used and has its
/// default instead.
fn settings_lines(warnings: &[SettingsWarning]) -> Vec<String> {
    warnings
        .iter()
        .map(|warning| format!("settings · {warning}"))
        .collect()
}

/// Esc while a command works: `/update` stops, keeping the rows so far, and every updater it
/// started ends with what that one started. With nothing at work it does nothing.
pub(crate) fn stop_command(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    if let Some(stop) = UpdateAtWork::stop_of(ctx.env.paths.home()) {
        stop.cancel();
    }
    Ok(Vec::new())
}

/// What `/update` says while another is at work.
pub(crate) const UPDATE_GOING: &str = "An update is going. Esc stops it.";

/// The `/update` at work for each Argonauts home, so Esc can stop it and a second one waits
/// for it to end. The app runs one controller for its home; tests run many side by side, each
/// with a home of its own.
static UPDATES_AT_WORK: Mutex<BTreeMap<PathBuf, CancellationToken>> = Mutex::new(BTreeMap::new());

/// An update at work, its stop kept until it's dropped.
struct UpdateAtWork {
    home: PathBuf,
    stop: CancellationToken,
}

impl UpdateAtWork {
    /// The update for `home`, or `None` while another is at work there.
    fn start(home: &Path) -> Option<Self> {
        let mut at_work = UPDATES_AT_WORK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if at_work.contains_key(home) {
            return None;
        }
        let stop = CancellationToken::new();
        at_work.insert(home.to_path_buf(), stop.clone());
        Some(Self {
            home: home.to_path_buf(),
            stop,
        })
    }

    fn stop_of(home: &Path) -> Option<CancellationToken> {
        UPDATES_AT_WORK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(home)
            .cloned()
    }
}

impl Drop for UpdateAtWork {
    fn drop(&mut self) {
        UPDATES_AT_WORK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.home);
    }
}

/// `/update`: Argonauts Code itself first, from its channel's newest release, then each
/// installed CLI updated once, side by side, the way it was installed, with its version before
/// and after, then Doctor. The working line says which of the two goes on, and the Update
/// report gains its rows in order as they are done from the first on, in columns set from the
/// start, with a line under them on how Argonauts Code updates. Off the loop, since an updater
/// downloads, and never while a run or another update is going. Esc stops it: the rows done
/// stay, the rest read stopped, and Doctor doesn't follow.
pub(crate) fn update(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    if super::cli_updates::a_run_is_going(ctx) {
        ctx.notice(NoteLevel::Info, super::cli_updates::UPDATE_WAITS);
        return Ok(Vec::new());
    }
    let Some(at_work) = UpdateAtWork::start(ctx.env.paths.home()) else {
        ctx.notice(NoteLevel::Info, UPDATE_GOING);
        return Ok(Vec::new());
    };
    let adapters = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())?;
    let settings = ctx.settings.shared();
    let out = ctx.out.clone();
    let versions = ctx.env.paths.cli_versions();
    let paths = ctx.env.paths.clone();
    let here = Source::for_home(&paths).map(|source| Here::this_process(&paths, source));
    let working = |work| AppEvent::CommandWorking { work };
    out.send(working(Some(CommandWork::Updating)));
    tokio::spawn(async move {
        let stop = at_work.stop.clone();
        let clis = each_once(&adapters, &settings);
        let columns = UpdateColumns::of(&clis, &settings)
            .await
            .with_row(OWN_ROW, &running_version().to_string());
        let install = here.as_ref().ok().map(|here| here.install);
        let limits = UpdateLimits::within(UPDATE_TIMEOUT);
        let rows = std::sync::Mutex::new(Rows::default());
        let so_far = || {
            let rows = rows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(lines) = rows.lines(columns) {
                out.send(AppEvent::ReportSoFar {
                    title: UPDATE.to_owned(),
                    lines,
                });
            }
        };
        let own = async {
            // Heard only where stopping leaves nothing half done: before the check ends, before
            // the download, and before the switch.
            let row = own_row(here, &settings.updates, &stop).await;
            rows.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .own = Some(row);
            so_far();
        };
        let each = update_each(&clis, &settings, limits, &stop, |done| {
            rows.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clis = done.to_vec();
            so_far();
        });
        let ((), updates) = tokio::join!(own, each);
        let mut rows = rows
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rows.clis = updates;
        let mut lines = rows.lines(columns).unwrap_or_default();
        lines.extend(install.map(|install| how_it_updates(install, &settings.updates)));
        out.send(AppEvent::Report {
            title: UPDATE.to_owned(),
            lines,
        });
        if stop.is_cancelled() {
            out.send(working(None));
            return;
        }
        // A second /update may start while Doctor checks.
        drop(at_work);
        out.send(working(Some(CommandWork::Checking)));
        out.send(AppEvent::Report {
            title: "Doctor".to_owned(),
            lines: doctor_lines(&adapters, &settings, &versions).await,
        });
        out.send(working(None));
    });
    Ok(Vec::new())
}

/// The name Argonauts Code's own row starts with.
const OWN_ROW: &str = "Argonauts Code";

/// The Update report's rows as they come: Argonauts Code's own, then the CLIs done so far.
#[derive(Default)]
struct Rows {
    own: Option<OwnRow>,
    clis: Vec<CliUpdate>,
}

impl Rows {
    /// The rows from the first on, none until Argonauts Code's own is done.
    fn lines(&self, columns: UpdateColumns) -> Option<Vec<String>> {
        let own = self.own.as_ref()?;
        Some(update_lines_under(
            (OWN_ROW, &own.version, &own.words),
            columns,
            &self.clis,
        ))
    }
}

/// The Update report's title, which its rows so far and the whole report share.
const UPDATE: &str = "Update";

/// Doctor's report, each version read now, with how it stands and what there is to update to,
/// what each version sent that Argonauts Code doesn't read yet from `versions`, and how many
/// of each CLI's own commands its table lacks.
async fn doctor_lines(
    adapters: &[Arc<dyn Adapter>],
    settings: &Settings,
    versions: &Path,
) -> Vec<String> {
    // A second way into a CLI, such as OpenCode over ACP, is that CLI: one row, counted once.
    let adapters: Vec<Arc<dyn Adapter>> = each_once(adapters, settings)
        .into_iter()
        .map(|cli| cli.adapter)
        .collect();
    let adapters = adapters.as_slice();
    let (mut agents, git) = check(adapters, settings).await;
    let seen = CliVersions::load(versions);
    for agent in &mut agents {
        with_what_was_seen(agent, adapters, &seen);
    }
    // Counted against the table again, since this Argonauts Code may list one now.
    let new_commands = adapters
        .iter()
        .map(|adapter| {
            let listed = seen.commands(adapter.id());
            let count = listed
                .iter()
                .filter(|command| new_command(adapter.as_ref(), command))
                .count();
            (adapter.id().to_owned(), count)
        })
        .collect();
    report(&agents, &new_commands, &git)
}

/// Adds to `agent` what sessions and the install sources have shown of its version.
fn with_what_was_seen(agent: &mut FoundAgent, adapters: &[Arc<dyn Adapter>], seen: &CliVersions) {
    agent.builtins = seen.builtins(&agent.id);
    let Some(adapter) = adapters.iter().find(|adapter| adapter.id() == agent.id) else {
        return;
    };
    if !agent.installed() {
        return;
    }
    agent.check = version_check(
        adapter.as_ref(),
        Installed {
            version: agent.version.as_deref(),
            path: agent.path.as_deref(),
            flags_whole: agent.problem.is_none(),
        },
        seen,
    );
    if let Some(version) = &agent.version {
        agent.unread = seen.unread(&agent.id, version);
        let missing = seen.startup_missing(&agent.id, version).unwrap_or_default();
        agent.lacks = startup_warnings(adapter.as_ref(), version, &missing)
            .into_iter()
            .map(|(_, warning)| warning)
            .collect();
    }
}

/// Versions, then the logins of the installed CLIs with a verified check.
pub(in crate::controller) async fn check(
    adapters: &[Arc<dyn Adapter>],
    settings: &Settings,
) -> (Vec<FoundAgent>, GitCheck) {
    let report = argonauts_conductor::doctor(adapters, settings).await;
    let mut agents = probe::found_agents(adapters, &report, settings);
    for (id, state) in probe::logins(&agents).await {
        if let Some(agent) = agents.iter_mut().find(|agent| agent.id == id) {
            agent.login = state;
        }
    }
    (agents, report.git)
}

/// One row per agent CLI, as [`agent_line`] writes it, with `· 2 new commands read from it`
/// for the CLI's own commands `new_commands` counts by id. Under a row come what its version
/// lacks of the flags and start-up fields Argonauts Code reads, and the events it sent that
/// Argonauts Code doesn't read yet. Then git and the verdict, and Claude Code's own checkup
/// last, when it's installed.
pub(in crate::controller) fn report(
    agents: &[FoundAgent],
    new_commands: &BTreeMap<String, usize>,
    git: &GitCheck,
) -> Vec<String> {
    let mut lines = Vec::new();
    for agent in agents {
        let mut line = agent_line(agent);
        let count = match new_commands.get(&agent.id) {
            Some(&count) if agent.installed() => count,
            _ => 0,
        };
        match count {
            0 => {}
            1 => line.push_str(" · 1 new command read from it"),
            count => line.push_str(&format!(" · {count} new commands read from it")),
        }
        lines.push(line);
        if agent.installed() {
            lines.extend(agent.problem.iter().map(|problem| format!("  {problem}")));
            lines.extend(agent.lacks.iter().map(|lack| format!("  ⚠ {lack}")));
        }
        lines.extend(unread_line(&agent.unread).map(|line| format!("  {line}")));
        lines.extend(builtins_line(&agent.builtins).map(|line| format!("  {line}")));
    }
    lines.push(match (&git.version, &git.problem) {
        (Some(version), _) => format!("git {version}"),
        (None, Some(problem)) => format!("git · {problem}"),
        (None, None) => "git · not found".to_owned(),
    });
    lines.push(verdict(agents, git));
    if agents
        .iter()
        .any(|agent| agent.id == "claude-code" && agent.installed())
    {
        lines.push("Claude Code's own checkup: /checkup".to_owned());
    }
    lines
}

/// `Claude Code 2.1.296       checked · works               2.1.297 is out · claude update`:
/// the name and version, how the version stands, then what there is to update to, the login
/// and whether it's off.
pub(in crate::controller) fn agent_line(agent: &FoundAgent) -> String {
    if !agent.installed() {
        let mut parts = vec![agent.name.clone(), "not installed".to_owned()];
        if let Some(install) = &agent.hints.install {
            parts.push(format!("install: {install}"));
        }
        return parts.join(" · ");
    }
    let mut after = Vec::new();
    match agent.login {
        LoginState::LoggedIn => after.push("logged in".to_owned()),
        LoginState::LoggedOut => after.push(match &agent.hints.login {
            Some(login) => format!("not logged in: {login}"),
            None => "not logged in".to_owned(),
        }),
        LoginState::Unknown => {}
    }
    if !agent.enabled {
        after.push(format!("off · /agents add {} turns it on", agent.id));
    }
    let row = version_row(
        &agent.name,
        agent.version.as_deref().unwrap_or_default(),
        &agent.check,
    );
    if after.is_empty() {
        return row;
    }
    let after = after.join(" · ");
    if agent.check.note().is_some() {
        format!("{row} · {after}")
    } else {
        format!(
            "{row:<width$}{after}",
            width = VERSION_ROW_NAMES + VERSION_ROW_STATES
        )
    }
}

fn verdict(agents: &[FoundAgent], git: &GitCheck) -> String {
    let ready = agents.iter().filter(|agent| agent.ready()).count();
    if git.version.is_none() || ready == 0 {
        return "Not ready: Argonauts Code needs git and at least one agent CLI that's installed, on, \
                and logged in."
            .to_owned();
    }
    format!("Ready with {ready} of {} agent CLIs.", agents.len())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_conductor::AgentStatus;
    use argonauts_core::agents::adapter::SetupHints;
    use argonauts_core::app::status_tab::{CliVersionCheck, VersionStanding};
    use argonauts_core::look::display_names::DisplayNames;

    use super::*;

    fn agent(id: &str, status: AgentStatus, login: LoginState) -> FoundAgent {
        let installed = status != AgentStatus::NotInstalled;
        FoundAgent {
            id: id.to_owned(),
            name: DisplayNames::default().cli(id),
            version: installed.then(|| "1.0.0".to_owned()),
            path: installed.then(|| PathBuf::from(format!("/bin/{id}"))),
            status,
            enabled: true,
            hints: SetupHints {
                install: Some(format!("npm i -g {id}")),
                login: Some(format!("{id} login")),
                login_check: None,
            },
            login,
            newest_tested: None,
            unread: Vec::new(),
            builtins: Vec::new(),
            problem: None,
            lacks: Vec::new(),
            check: CliVersionCheck {
                standing: match status {
                    AgentStatus::Tested => VersionStanding::Checked,
                    AgentStatus::Newer => VersionStanding::Newer,
                    _ => VersionStanding::Unknown,
                },
                ..CliVersionCheck::default()
            },
        }
    }

    fn git() -> GitCheck {
        GitCheck {
            version: Some("2.50.1".to_owned()),
            problem: None,
        }
    }

    #[test]
    fn each_cli_shows_its_version_and_login() {
        let mut off = agent("hermes", AgentStatus::Untested, LoginState::Unknown);
        off.enabled = false;
        let lines = report(
            &[
                agent("claude-code", AgentStatus::Tested, LoginState::LoggedIn),
                agent("codex", AgentStatus::Tested, LoginState::LoggedOut),
                agent("opencode", AgentStatus::NotInstalled, LoginState::Unknown),
                off,
            ],
            &BTreeMap::new(),
            &git(),
        );
        assert_eq!(
            lines,
            vec![
                "Claude Code 1.0.0         checked                       logged in",
                "Codex 1.0.0               checked                       not logged in: codex login",
                "OpenCode · not installed · install: npm i -g opencode",
                "hermes 1.0.0              unknown version               off · /agents add hermes \
                 turns it on",
                "git 2.50.1",
                "Ready with 1 of 4 agent CLIs.",
                "Claude Code's own checkup: /checkup",
            ]
        );
    }

    #[test]
    fn a_newer_cli_says_so_and_lists_what_argonauts_doesnt_read() {
        let mut claude = agent("claude-code", AgentStatus::Newer, LoginState::LoggedIn);
        claude.version = Some("2.1.291".to_owned());
        claude.check.works = true;
        claude.check.checked_with = Some("2.1.289".to_owned());
        claude.unread = vec![
            ("system/turn_summary".to_owned(), 3),
            ("cost_update".to_owned(), 1),
        ];
        claude.builtins = vec!["cc-plugin-new-thing".to_owned()];
        let new_commands = BTreeMap::from([("claude-code".to_owned(), 2)]);
        let lines = report(&[claude], &new_commands, &git());
        assert_eq!(
            lines[..3],
            [
                "Claude Code 2.1.291       newer than checked · works    its flags and start-up \
                 message match 2.1.289 · logged in · 2 new commands read from it",
                "  sent 2 kinds of events Argonauts Code doesn't read yet: system/turn_summary, \
                 cost_update",
                "  loaded built-in plugins Argonauts Code doesn't know yet: cc-plugin-new-thing, \
                 which blind roles turn off",
            ]
        );
        // Without Claude Code installed, nothing points at its checkup.
        let codex = report(
            &[agent("codex", AgentStatus::Tested, LoginState::LoggedIn)],
            &BTreeMap::new(),
            &git(),
        );
        assert!(
            !codex.iter().any(|line| line.contains("/checkup")),
            "{codex:?}"
        );
    }

    #[tokio::test]
    async fn slash_doctor_reports_each_cli_without_holding_the_screen() {
        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;

        use crate::controller::base::test_support::{CliOptions, TestDriver};

        // Nothing real runs: every CLI points nowhere.
        let ids = [
            "claude-code",
            "codex",
            "antigravity-cli",
            "opencode",
            "opencode-acp",
        ];
        let flags = ids
            .iter()
            .map(|id| format!(r#"agents.{id}.binary="/nowhere/{id}""#))
            .collect();
        let mut driver = TestDriver::start_with(CliOptions {
            flags,
            ..CliOptions::default()
        })
        .await;
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Doctor,
            })
            .await;
        let event = driver
            .until(|event| matches!(event, AppEvent::Report { .. }))
            .await;
        let AppEvent::Report { title, lines } = event else {
            unreachable!()
        };
        assert_eq!(title, "Doctor");
        assert!(
            lines[0].starts_with("Claude Code · not installed"),
            "{lines:?}"
        );
        for line in &lines[..ids.len()] {
            assert!(line.contains(" · not installed"), "{lines:?}");
        }
        assert_eq!(lines.len(), ids.len() + 2, "{lines:?}");
        assert!(lines[ids.len()].starts_with("git"), "{lines:?}");
        assert!(lines[ids.len() + 1].starts_with("Not ready"), "{lines:?}");
        driver.exit().await;
    }

    /// Claude Code's updater runs as a script here, and every other CLI points nowhere.
    #[tokio::test]
    async fn slash_update_runs_each_updater_then_doctor() {
        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;

        use crate::controller::base::test_support::{CliOptions, TestDriver};

        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join("version");
        std::fs::write(&version, "2.1.292 (Claude Code)\n").unwrap();
        let claude = dir.path().join("claude");
        argonauts_testkit::write_script(
            &claude,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) cat '{v}' ;;\n  update) echo '2.1.295 \
                 (Claude Code)' > '{v}' ;;\nesac\n",
                v = version.display()
            ),
        )
        .unwrap();
        let mut flags = vec![format!(
            r#"agents.claude-code.binary="{}""#,
            claude.display()
        )];
        for id in ["codex", "antigravity-cli", "opencode", "opencode-acp"] {
            flags.push(format!(r#"agents.{id}.binary="/nowhere/{id}""#));
        }
        let mut driver = TestDriver::start_with(CliOptions {
            flags,
            ..CliOptions::default()
        })
        .await;
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Update,
            })
            .await;
        let working = |work| move |event: &AppEvent| matches!(event, AppEvent::CommandWorking { work: w } if *w == work);
        driver.until(working(Some(CommandWork::Updating))).await;
        // Argonauts Code's own row comes first, then the CLIs update side by side, and the
        // rows come from the first on, Claude Code's first, in columns as wide as the rows
        // after it will be. A test reaches no manifest, so its own row says why.
        let so_far = |event: &AppEvent| matches!(event, AppEvent::ReportSoFar { lines, .. } if lines.len() > 1);
        let AppEvent::ReportSoFar { title, lines } = driver.until(so_far).await else {
            unreachable!()
        };
        assert_eq!(title, "Update");
        assert_eq!(
            lines[0],
            format!(
                "Argonauts Code   {:<7}   updates are off in tests",
                crate::self_update::running_version()
            )
        );
        assert!(
            lines[1].starts_with("Claude Code      2.1.292 →  2.1.295"),
            "{lines:?}"
        );
        let report = |event: &AppEvent| matches!(event, AppEvent::Report { .. });
        let AppEvent::Report { title, lines } = driver.until(report).await else {
            unreachable!()
        };
        assert_eq!(title, "Update");
        assert!(lines[0].starts_with("Argonauts Code   "), "{lines:?}");
        assert!(
            lines[1].starts_with("Claude Code ") && lines[1].ends_with("2.1.292 →  2.1.295"),
            "{lines:?}"
        );
        assert!(lines[2].contains("couldn't update"), "{lines:?}");
        driver.until(working(Some(CommandWork::Checking))).await;
        let AppEvent::Report { title, lines } = driver.until(report).await else {
            unreachable!()
        };
        assert_eq!(title, "Doctor");
        assert!(lines[0].starts_with("Claude Code 2.1.295 "), "{lines:?}");
        driver.until(working(None)).await;
        driver.exit().await;
    }

    /// Argonauts Code's own row reads its channel's manifest. A test's build wasn't put in
    /// place by the installer, so the row names the releases and the line under the CLIs says
    /// it doesn't replace itself.
    #[tokio::test]
    async fn slash_update_reads_argonauts_codes_own_manifest_first() {
        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;

        use crate::controller::base::test_support::{CliOptions, TestDriver};
        use crate::self_update::test_server::{FakeServer, Signer, manifest};

        let mut flags = Vec::new();
        for id in [
            "claude-code",
            "codex",
            "antigravity-cli",
            "opencode",
            "opencode-acp",
        ] {
            flags.push(format!(r#"agents.{id}.binary="/nowhere/{id}""#));
        }
        let mut driver = TestDriver::start_with(CliOptions {
            flags,
            ..CliOptions::default()
        })
        .await;
        let server = FakeServer::start().await;
        let signer = Signer::new();
        let mut next = running_version();
        next.patch += 1;
        server.publish("stable", &manifest(&next.to_string(), 1), &signer);
        crate::self_update::testing::serve(
            driver.env.paths.home(),
            server.source(vec![signer.public()]),
        );
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Update,
            })
            .await;
        let report =
            |event: &AppEvent| matches!(event, AppEvent::Report { title, .. } if title == "Update");
        let AppEvent::Report { lines, .. } = driver.until(report).await else {
            unreachable!()
        };
        assert_eq!(
            lines[0],
            format!(
                "Argonauts Code   {}   {next} is out · {}",
                running_version(),
                crate::self_update::RELEASES
            )
        );
        assert_eq!(
            lines.last().unwrap(),
            "This copy of Argonauts Code wasn't put here by its installer, so it doesn't replace \
             itself."
        );
        assert!(
            server
                .asked()
                .contains(&"/updates/v1/stable.json".to_owned())
        );
        driver.exit().await;
    }

    /// The report as the screen draws it at the pictures' width, without the blank rows around
    /// it.
    fn report_rows(lines: Vec<String>) -> Vec<ratatui::text::Line<'static>> {
        use argonauts_tui::bridge::Bridge;
        use argonauts_tui::{UiOptions, ViewModel};

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let report = AppEvent::Report {
            title: "Update".to_owned(),
            lines,
        };
        bridge.on_event(&mut model, &report, std::time::Instant::now());
        let theme = crate::testing::golden::theme();
        let rows = model
            .history
            .drain_printable(&crate::testing::golden::still(&theme));
        let drawn = crate::testing::golden::drawn_rows(&rows);
        let first = drawn.iter().position(|row| !row.is_empty()).unwrap_or(0);
        let end = drawn
            .iter()
            .rposition(|row| !row.is_empty())
            .map_or(0, |at| at + 1);
        rows[first..end].to_vec()
    }

    /// The Update report from the picture's example numbers, with Argonauts Code's own row
    /// first, then the first row a Homebrew install reads, which the picture shows under it
    /// without the hook.
    ///
    /// Two words take another colour than the picture's. `/release-notes` in the first row
    /// takes the code colour, as every command in a report does and as `/config` does two rows
    /// down, where the picture draws it dim. `brew upgrade argonauts-code` reads dim with the
    /// words around it, where the picture draws it in the code colour: a report colours only
    /// slash commands.
    #[test]
    fn update_panel_is_its_picture() {
        use argonauts_conductor::doctor::update::UpdateOutcome;
        use argonauts_core::settings::updates::UpdateSettings;
        use ratatui::text::{Line, Span};

        use crate::self_update::Install;

        let cli = |name: &str, version: &str, also: &[&str]| CliUpdate {
            name: name.to_owned(),
            outcome: UpdateOutcome::UpToDate {
                version: version.to_owned(),
            },
            also_through: also.iter().map(|way| (*way).to_owned()).collect(),
        };
        let clis = [
            cli("Claude Code", "2.1.304", &[]),
            cli("Codex", "0.163.0", &[]),
            cli("agy", "1.4.3", &[]),
            cli("OpenCode", "2.0.32", &["ACP"]),
        ];
        let rows = |words: &str| {
            update_lines_under((OWN_ROW, "0.1.0", words), UpdateColumns::default(), &clis)
        };
        let mut lines = rows("0.1.1 downloaded · takes over at the next start · /release-notes");
        lines.push(how_it_updates(Install::Shell, &UpdateSettings::default()));
        let mut drawn = report_rows(lines);
        let homebrew = report_rows(rows("0.1.1 is out · brew upgrade argonauts-code"));
        let hook_off =
            homebrew[1]
                .spans
                .iter()
                .fold((Vec::new(), 5usize), |(mut spans, mut skip), span| {
                    let chars = span.content.chars().count();
                    if skip >= chars {
                        skip -= chars;
                    } else {
                        let rest: String = span.content.chars().skip(skip).collect();
                        spans.push(Span::styled(rest, span.style));
                        skip = 0;
                    }
                    (spans, skip)
                });
        let mut first = vec![Span::raw("     ")];
        first.extend(hook_off.0);
        drawn.push(Line::from(first));
        crate::testing::golden::dump("update_panel", &drawn);
        let rows = crate::testing::golden::drawn_rows(&drawn);
        assert_eq!(
            rows,
            crate::testing::golden::picture_rows(include_str!(
                "../../../tests/golden/update_panel.txt"
            ))
        );
        let at = |row: usize, words: &str| {
            let start = rows[row][..rows[row].find(words).unwrap()].chars().count();
            (row, start..start + words.chars().count())
        };
        let otherwise = [
            at(1, "/release-notes"),
            at(7, "brew upgrade argonauts-code"),
        ];
        crate::testing::golden::assert_looks_except(
            &crate::testing::golden::theme(),
            &drawn,
            include_str!("../../../tests/golden/update_panel.html"),
            |row, column| {
                otherwise
                    .iter()
                    .any(|(at, columns)| *at == row && columns.contains(&column))
            },
        );
    }

    /// Esc while `/update` works stops it soon after: the updater at work ends, its row and
    /// the rows not done read stopped, Doctor doesn't follow, and the working line goes. A
    /// second `/update` meanwhile is refused.
    #[tokio::test]
    async fn slash_update_stops_on_esc() {
        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;

        use crate::controller::base::test_support::{CliOptions, TestDriver};

        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        let claude = dir.path().join("claude");
        argonauts_testkit::write_script(
            &claude,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo '2.1.292 (Claude Code)' ;;\n  \
                 update) echo started > '{}'; sleep 30 ;;\nesac\n",
                started.display()
            ),
        )
        .unwrap();
        let mut flags = vec![format!(
            r#"agents.claude-code.binary="{}""#,
            claude.display()
        )];
        for id in ["codex", "antigravity-cli", "opencode", "opencode-acp"] {
            flags.push(format!(r#"agents.{id}.binary="/nowhere/{id}""#));
        }
        let mut driver = TestDriver::start_with(CliOptions {
            flags,
            ..CliOptions::default()
        })
        .await;
        let update = UiCommand::Slash {
            command: SlashCommand::Update,
        };
        driver.send(update.clone()).await;
        while !started.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        driver.send(update).await;
        let notice = |event: &AppEvent| matches!(event, AppEvent::Notice { .. });
        let AppEvent::Notice { text, .. } = driver.until(notice).await else {
            unreachable!()
        };
        assert_eq!(text, UPDATE_GOING);

        let began = std::time::Instant::now();
        driver.send(UiCommand::StopCommand).await;
        let report =
            |event: &AppEvent| matches!(event, AppEvent::Report { title, .. } if title == "Update");
        let AppEvent::Report { lines, .. } = driver.until(report).await else {
            unreachable!()
        };
        assert!(
            began.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            began.elapsed()
        );
        assert!(
            lines[1].starts_with("Claude Code ") && lines[1].ends_with("2.1.292   stopped"),
            "{lines:?}"
        );
        driver
            .until(|event| matches!(event, AppEvent::CommandWorking { work: None }))
            .await;
        let events = driver.exit().await;
        let doctor = events
            .iter()
            .any(|event| matches!(event, AppEvent::Report { title, .. } if title == "Doctor"));
        assert!(!doctor, "Doctor doesn't follow a stopped update");
    }

    /// Esc with no command at work changes nothing and says nothing.
    #[tokio::test]
    async fn stopping_with_no_command_at_work_does_nothing() {
        use argonauts_core::app::UiCommand;

        use crate::controller::base::test_support::TestDriver;

        let driver = TestDriver::start().await;
        driver.send(UiCommand::StopCommand).await;
        let events = driver.exit().await;
        assert!(
            !events.iter().any(|event| matches!(
                event,
                AppEvent::Notice { .. } | AppEvent::CommandWorking { .. }
            )),
            "{events:#?}"
        );
    }

    /// While a run is going, in this folder or another, `/update` updates nothing and says why.
    #[tokio::test]
    async fn slash_update_waits_for_runs_to_end() {
        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;

        use crate::controller::base::test_support::TestDriver;
        use crate::host::live_registry::{LiveRunHost, register};

        let mut driver = TestDriver::start().await;
        let (pid, start_time) = crate::startup::ancestry::own_process();
        let going = LiveRunHost {
            pid,
            start_time,
            repo_root: "/another/folder".into(),
            run: argonauts_core::RunId::new("run-20261010-091503-3fa2").unwrap(),
            sessions: 1,
        };
        register(&driver.env.paths, &going).unwrap();
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Update,
            })
            .await;
        let notice = |event: &AppEvent| matches!(event, AppEvent::Notice { .. });
        let AppEvent::Notice { text, .. } = driver.until(notice).await else {
            unreachable!()
        };
        assert_eq!(text, crate::controller::clis::cli_updates::UPDATE_WAITS);
        let events = driver.exit().await;
        let reported = events
            .iter()
            .any(|event| matches!(event, AppEvent::Report { .. }));
        assert!(!reported, "nothing was updated or checked");
    }

    /// The four rows of the versions picture, from its example numbers.
    #[test]
    fn the_cli_rows_read_as_the_versions_picture_lists_them() {
        let row = |id: &str, version: &str, check: CliVersionCheck| {
            let mut found = agent(id, AgentStatus::Tested, LoginState::Unknown);
            found.version = Some(version.to_owned());
            found.check = check;
            agent_line(&found)
        };
        let checked = CliVersionCheck {
            standing: VersionStanding::Checked,
            works: true,
            ..CliVersionCheck::default()
        };
        let rows = [
            row(
                "claude-code",
                "2.1.296",
                CliVersionCheck {
                    newest: Some("2.1.297".to_owned()),
                    newer_is_out: true,
                    update_command: Some("claude update".to_owned()),
                    ..checked.clone()
                },
            ),
            row(
                "codex",
                "0.158.0",
                CliVersionCheck {
                    standing: VersionStanding::Older,
                    oldest_supported: Some("0.160.0".to_owned()),
                    update_command: Some("brew upgrade codex".to_owned()),
                    ..CliVersionCheck::default()
                },
            ),
            row(
                "antigravity-cli",
                "1.3.3",
                CliVersionCheck {
                    standing: VersionStanding::Newer,
                    works: true,
                    checked_with: Some("1.3.1".to_owned()),
                    ..CliVersionCheck::default()
                },
            ),
            row(
                "opencode",
                "1.18.34",
                CliVersionCheck {
                    newest: Some("1.18.34".to_owned()),
                    ..checked
                },
            ),
        ];
        let picture = include_str!("../../../../tui/tests/golden/overview/versions.txt");
        let drawn: Vec<&str> = picture.lines().skip(2).take(4).collect();
        assert_eq!(rows.to_vec(), drawn);
    }

    #[test]
    fn what_a_version_lacks_is_named_under_its_row() {
        let mut claude = agent("claude-code", AgentStatus::Newer, LoginState::Unknown);
        claude.version = Some("2.1.297".to_owned());
        claude.problem = Some("--help lacks flags Argonauts Code passes: --effort".to_owned());
        claude.lacks = vec![
            "Claude Code 2.1.297 no longer reports its model, so /context can't size its window \
             until Argonauts Code is updated"
                .to_owned(),
        ];
        let lines = report(&[claude], &BTreeMap::new(), &git());
        assert_eq!(
            lines[..3],
            [
                "Claude Code 2.1.297       newer than checked",
                "  --help lacks flags Argonauts Code passes: --effort",
                "  ⚠ Claude Code 2.1.297 no longer reports its model, so /context can't size its \
                 window until Argonauts Code is updated",
            ]
        );
    }

    #[test]
    fn no_ready_cli_or_no_git_is_not_ready() {
        let none = report(
            &[agent("codex", AgentStatus::Tested, LoginState::LoggedOut)],
            &BTreeMap::new(),
            &git(),
        );
        assert!(none.last().unwrap().starts_with("Not ready"), "{none:?}");
        let no_git = report(
            &[agent("codex", AgentStatus::Tested, LoginState::LoggedIn)],
            &BTreeMap::new(),
            &GitCheck {
                version: None,
                problem: Some("git is not installed".to_owned()),
            },
        );
        assert_eq!(no_git[1], "git · git is not installed");
        assert!(no_git[2].starts_with("Not ready"), "{no_git:?}");
    }
}
