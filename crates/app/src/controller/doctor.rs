//! `/doctor` in the app: each agent CLI with its version and login, then git. The same checks
//! as `argonauts doctor` and the setup, run off the loop so a slow CLI never holds the screen.
//! It never calls a model. Claude Code's own checkup is `/checkup`, which runs inside it.

use std::sync::Arc;

use argonauts_conductor::cli_versions::{CliVersions, builtins_line, unread_line};
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
    tokio::spawn(async move {
        let (mut agents, git) = check(&adapters, &settings).await;
        let seen = CliVersions::load(&versions);
        for agent in &mut agents {
            if let Some(version) = &agent.version {
                agent.unread = seen.unread(&agent.id, version);
            }
            agent.builtins = seen.builtins(&agent.id);
        }
        out.send(AppEvent::Report {
            title: "Doctor".to_owned(),
            lines: report(&agents, &git),
        });
    });
    Ok(Vec::new())
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
/// under it, then git and the verdict: `claude-code 2.1.300 · tested · logged in`. Claude Code's
/// own checkup comes last, when it's installed.
pub(super) fn report(agents: &[FoundAgent], git: &GitCheck) -> Vec<String> {
    let mut lines = Vec::new();
    for agent in agents {
        lines.push(agent_line(agent));
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
        Some(version) => format!("{} {version}", agent.id),
        None => agent.id.clone(),
    }];
    if !agent.installed() {
        parts.push("not installed".to_owned());
        if let Some(install) = &agent.hints.install {
            parts.push(format!("install: {install}"));
        }
        return parts.join(" · ");
    }
    parts.push(match (agent.status, &agent.newest_tested) {
        (AgentStatus::Newer, Some(newest)) => {
            format!("newer than tested ({newest}), should work")
        }
        (AgentStatus::Newer, None) => "newer than tested, should work".to_owned(),
        (AgentStatus::Tested, _) => "tested".to_owned(),
        (AgentStatus::Untested, _) => "untested version, may still work".to_owned(),
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

    use super::*;

    fn agent(id: &str, status: AgentStatus, login: LoginState) -> FoundAgent {
        let installed = status != AgentStatus::NotInstalled;
        FoundAgent {
            id: id.to_owned(),
            name: id.to_owned(),
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
            &git(),
        );
        assert_eq!(
            lines,
            vec![
                "claude-code 1.0.0 · tested · logged in",
                "codex 1.0.0 · tested · not logged in: codex login",
                "opencode · not installed · install: npm i -g opencode",
                "hermes 1.0.0 · untested version, may still work · off · /agents add hermes turns \
                 it on",
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
        let lines = report(&[claude], &git());
        assert_eq!(
            lines[..3],
            [
                "claude-code 2.1.291 · newer than tested (2.1.289), should work · logged in",
                "  sent 2 kinds of events Argonauts doesn't read yet: system/turn_summary, \
                 cost_update",
                "  loaded built-in plugins Argonauts doesn't know yet: cc-plugin-new-thing, \
                 which blind roles turn off",
            ]
        );
        // Without Claude Code installed, nothing points at its checkup.
        let codex = report(
            &[agent("codex", AgentStatus::Tested, LoginState::LoggedIn)],
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
        for (line, id) in lines.iter().zip(ids) {
            assert!(
                line.starts_with(&format!("{id} · not installed")),
                "{lines:?}"
            );
        }
        assert_eq!(lines.len(), ids.len() + 2, "{lines:?}");
        assert!(lines[ids.len()].starts_with("git"), "{lines:?}");
        assert!(lines[ids.len() + 1].starts_with("Not ready"), "{lines:?}");
        harness.exit().await;
    }

    #[test]
    fn no_ready_cli_or_no_git_is_not_ready() {
        let none = report(
            &[agent("codex", AgentStatus::Tested, LoginState::LoggedOut)],
            &git(),
        );
        assert!(none.last().unwrap().starts_with("Not ready"), "{none:?}");
        let no_git = report(
            &[agent("codex", AgentStatus::Tested, LoginState::LoggedIn)],
            &GitCheck {
                version: None,
                problem: Some("git is not installed".to_owned()),
            },
        );
        assert_eq!(no_git[1], "git · git is not installed");
        assert!(no_git[2].starts_with("Not ready"), "{no_git:?}");
    }
}
