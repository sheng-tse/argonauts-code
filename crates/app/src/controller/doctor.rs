//! `/doctor` in the app: each agent CLI by name with its version, read live, and its login, then
//! git. The same checks as `argonauts doctor` and the setup, run off the loop so a slow CLI never
//! holds the screen. It never calls a model. Claude Code's own checkup is `/checkup`, which runs
//! inside it. `/update` updates each CLI the way it was installed first, between runs only.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use argonauts_conductor::GitCheck;
use argonauts_conductor::cli_versions::{
    CliVersions, Installed, builtins_line, new_command, startup_warnings, unread_line,
    version_check,
};
use argonauts_conductor::doctor::update::{UPDATE_TIMEOUT, update_clis, update_lines};
use argonauts_core::Adapter;
use argonauts_core::app::AppEvent;
use argonauts_core::log::NoteLevel;
use argonauts_core::settings::Settings;
use argonauts_core::status_tab::{VERSION_ROW_NAMES, VERSION_ROW_STATES, version_row};

use super::{Ctx, Effect};
use crate::setup::probe::{self, FoundAgent, LoginState};

pub(crate) fn doctor(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    let adapters = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())?;
    let settings = ctx.settings.shared();
    let out = ctx.out.clone();
    let versions = ctx.env.paths.cli_versions();
    let prompt_files = super::prompt_templates::doctor_lines(ctx);
    let mods = crate::mods::doctor_lines(&ctx.env);
    let settings_lines = settings_lines(&ctx.settings.resolved().warnings);
    let paths = ctx.env.paths.clone();
    let unnamed = super::status_tab::unnamed_accounts(ctx);
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
fn settings_lines(warnings: &[String]) -> Vec<String> {
    warnings
        .iter()
        .map(|warning| format!("settings · {warning}"))
        .collect()
}

/// `/update`: each installed CLI updated in turn the way it was installed, with its version
/// before and after, then Doctor. Off the loop, since an updater downloads, and never while a
/// run is going.
pub(crate) fn update(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    if super::cli_updates::a_run_is_going(ctx) {
        ctx.notice(NoteLevel::Info, super::cli_updates::UPDATE_WAITS);
        return Ok(Vec::new());
    }
    let adapters = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())?;
    let settings = ctx.settings.shared();
    let out = ctx.out.clone();
    let versions = ctx.env.paths.cli_versions();
    tokio::spawn(async move {
        let updates = update_clis(&adapters, &settings, UPDATE_TIMEOUT).await;
        out.send(AppEvent::Report {
            title: "Update".to_owned(),
            lines: update_lines(&updates),
        });
        out.send(AppEvent::Report {
            title: "Doctor".to_owned(),
            lines: doctor_lines(&adapters, &settings, &versions).await,
        });
    });
    Ok(Vec::new())
}

/// Doctor's report, each version read now, with how it stands and what there is to update to,
/// what each version sent that Argonauts Code doesn't read yet from `versions`, and how many
/// of each CLI's own commands its table lacks.
async fn doctor_lines(
    adapters: &[Arc<dyn Adapter>],
    settings: &Settings,
    versions: &Path,
) -> Vec<String> {
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
pub(super) async fn check(
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
pub(super) fn report(
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
pub(super) fn agent_line(agent: &FoundAgent) -> String {
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
    use argonauts_core::adapter::SetupHints;
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::status_tab::{CliVersionCheck, VersionStanding};

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

        use crate::controller::test_support::{CliOptions, TestDriver};

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
        use std::os::unix::fs::PermissionsExt;

        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;

        use crate::controller::test_support::{CliOptions, TestDriver};

        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join("version");
        std::fs::write(&version, "2.1.292 (Claude Code)\n").unwrap();
        let claude = dir.path().join("claude");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) cat '{v}' ;;\n  update) echo '2.1.295 \
                 (Claude Code)' > '{v}' ;;\nesac\n",
                v = version.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        let report = |event: &AppEvent| matches!(event, AppEvent::Report { .. });
        let AppEvent::Report { title, lines } = driver.until(report).await else {
            unreachable!()
        };
        assert_eq!(title, "Update");
        assert!(
            lines[0].starts_with("Claude Code ") && lines[0].ends_with("2.1.292 → 2.1.295"),
            "{lines:?}"
        );
        assert!(lines[1].contains("couldn't update"), "{lines:?}");
        let AppEvent::Report { title, lines } = driver.until(report).await else {
            unreachable!()
        };
        assert_eq!(title, "Doctor");
        assert!(lines[0].starts_with("Claude Code 2.1.295 "), "{lines:?}");
        driver.exit().await;
    }

    /// While a run is going, in this folder or another, `/update` updates nothing and says why.
    #[tokio::test]
    async fn slash_update_waits_for_runs_to_end() {
        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;

        use crate::controller::test_support::TestDriver;
        use crate::live_registry::{LiveRunHost, register};

        let mut driver = TestDriver::start().await;
        let (pid, start_time) = crate::ancestry::own_process();
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
        assert_eq!(text, crate::controller::cli_updates::UPDATE_WAITS);
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
        let picture = include_str!("../../../tui/src/overview/pictures/versions.txt");
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
