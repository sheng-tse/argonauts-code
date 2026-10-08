//! `/doctor` in the app: each agent CLI by name with its version, read live, and its login, then
//! git. The same checks as `argonauts doctor` and the setup, run off the loop so a slow CLI never
//! holds the screen. It never calls a model. Claude Code's own checkup is `/checkup`, which runs
//! inside it. `/update` runs each CLI's own updater first.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use argonauts_conductor::cli_versions::{CliVersions, builtins_line, new_command, unread_line};
use argonauts_conductor::doctor::update::{UPDATE_TIMEOUT, update_clis, update_lines};
use argonauts_conductor::{AgentStatus, GitCheck};
use argonauts_core::Adapter;
use argonauts_core::app::AppEvent;
use argonauts_core::settings::Settings;

use super::{Ctx, Effect};
use crate::setup::probe::{self, FoundAgent, LoginState};

pub(crate) fn doctor(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    let adapters = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())?;
    let settings = ctx.settings.shared();
    let out = ctx.out.clone();
    let versions = ctx.env.paths.cli_versions();
    let prompt_files = super::prompt_templates::doctor_lines(ctx);
    let mods = crate::mods::doctor_lines(&ctx.env);
    tokio::spawn(async move {
        let mut lines = doctor_lines(&adapters, &settings, &versions).await;
        lines.extend(prompt_files);
        lines.extend(mods);
        out.send(AppEvent::Report {
            title: "Doctor".to_owned(),
            lines,
        });
    });
    Ok(Vec::new())
}

/// `/update`: each installed CLI's own updater in turn, with its version before and after,
/// then Doctor. Off the loop, since an updater downloads.
pub(crate) fn update(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
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

/// Doctor's report, each version read now, with what each version sent that Argonauts doesn't
/// read yet from `versions`, and how many of each CLI's own commands its table lacks.
async fn doctor_lines(
    adapters: &[Arc<dyn Adapter>],
    settings: &Settings,
    versions: &Path,
) -> Vec<String> {
    let (mut agents, git) = check(adapters, settings).await;
    let seen = CliVersions::load(versions);
    for agent in &mut agents {
        if let Some(version) = &agent.version {
            agent.unread = seen.unread(&agent.id, version);
        }
        agent.builtins = seen.builtins(&agent.id);
    }
    // Counted against the table again, since this Argonauts may list one now.
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

/// One line per agent CLI, with the events its version sent that Argonauts doesn't read yet
/// under it, then git and the verdict: `Claude Code 2.1.300 · checked · logged in`, and
/// `· 2 new commands read from it` for the CLI's own commands `new_commands` counts by id.
/// Claude Code's own checkup comes last, when it's installed.
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

pub(super) fn agent_line(agent: &FoundAgent) -> String {
    let mut parts = vec![match &agent.version {
        Some(version) => format!("{} {version}", agent.name),
        None => agent.name.clone(),
    }];
    if !agent.installed() {
        parts.push("not installed".to_owned());
        if let Some(install) = &agent.hints.install {
            parts.push(format!("install: {install}"));
        }
        return parts.join(" · ");
    }
    parts.push(match (agent.status, &agent.newest_tested) {
        (AgentStatus::Newer, Some(newest)) => format!("newer than checked ({newest}) · works"),
        (AgentStatus::Newer, None) => "newer than checked · works".to_owned(),
        (AgentStatus::Tested, _) => "checked".to_owned(),
        (AgentStatus::Untested, _) => "not a checked version, may still work".to_owned(),
        (AgentStatus::UnknownVersion, _) => "unknown version".to_owned(),
        (AgentStatus::NotInstalled, _) => "not installed".to_owned(),
    });
    match agent.login {
        LoginState::LoggedIn => parts.push("logged in".to_owned()),
        LoginState::LoggedOut => parts.push(match &agent.hints.login {
            Some(login) => format!("not logged in: {login}"),
            None => "not logged in".to_owned(),
        }),
        LoginState::Unknown => {}
    }
    if !agent.enabled {
        parts.push(format!("off · /agents add {} turns it on", agent.id));
    }
    parts.join(" · ")
}

fn verdict(agents: &[FoundAgent], git: &GitCheck) -> String {
    let ready = agents.iter().filter(|agent| agent.ready()).count();
    if git.version.is_none() || ready == 0 {
        return "Not ready: Argonauts needs git and at least one agent CLI that's installed, on, \
                and logged in."
            .to_owned();
    }
    format!("Ready with {ready} of {} agent CLIs.", agents.len())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::adapter::SetupHints;
    use argonauts_core::display_names::DisplayNames;

    use super::*;

    fn agent(id: &str, status: AgentStatus, login: LoginState) -> FoundAgent {
        let installed = status != AgentStatus::NotInstalled;
        FoundAgent {
            id: id.to_owned(),
            name: DisplayNames::default().harness(id),
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
                "Claude Code 1.0.0 · checked · logged in",
                "Codex 1.0.0 · checked · not logged in: codex login",
                "OpenCode · not installed · install: npm i -g opencode",
                "hermes 1.0.0 · not a checked version, may still work · off · /agents add hermes \
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
        claude.newest_tested = Some("2.1.289".to_owned());
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
                "Claude Code 2.1.291 · newer than checked (2.1.289) · works · logged in · 2 new \
                 commands read from it",
                "  sent 2 kinds of events Argonauts doesn't read yet: system/turn_summary, \
                 cost_update",
                "  loaded built-in plugins Argonauts doesn't know yet: cc-plugin-new-thing, \
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

        use crate::controller::test_support::{Harness, HarnessOptions};

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
        let mut harness = Harness::start_with(HarnessOptions {
            flags,
            ..HarnessOptions::default()
        })
        .await;
        harness
            .send(UiCommand::Slash {
                command: SlashCommand::Doctor,
            })
            .await;
        let event = harness
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
        harness.exit().await;
    }

    /// Claude Code's updater runs as a script here, and every other CLI points nowhere.
    #[tokio::test]
    async fn slash_update_runs_each_updater_then_doctor() {
        use std::os::unix::fs::PermissionsExt;

        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;

        use crate::controller::test_support::{Harness, HarnessOptions};

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
        let mut harness = Harness::start_with(HarnessOptions {
            flags,
            ..HarnessOptions::default()
        })
        .await;
        harness
            .send(UiCommand::Slash {
                command: SlashCommand::Update,
            })
            .await;
        let report = |event: &AppEvent| matches!(event, AppEvent::Report { .. });
        let AppEvent::Report { title, lines } = harness.until(report).await else {
            unreachable!()
        };
        assert_eq!(title, "Update");
        assert!(
            lines[0].starts_with("Claude Code ") && lines[0].ends_with("2.1.292 → 2.1.295"),
            "{lines:?}"
        );
        assert!(lines[1].contains("couldn't update"), "{lines:?}");
        let AppEvent::Report { title, lines } = harness.until(report).await else {
            unreachable!()
        };
        assert_eq!(title, "Doctor");
        assert!(lines[0].starts_with("Claude Code 2.1.295 · "), "{lines:?}");
        harness.exit().await;
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
