//! `/agents add <name>`: sets up one more agent Argonauts Code can use. A CLI with a profile, such
//! as Hermes or OpenCode, is checked (its version and login, and how to install it when it's
//! missing) and turned on. Ollama is looked for at its address and turned on as a local
//! endpoint. OpenClaw is pointed at `/connect`, and any other name gets the `[[cli]]` entry
//! it needs.

use std::path::Path;

use argonauts_core::NoteLevel;
use argonauts_core::app::AppEvent;
use argonauts_core::settings::SettingScope;

use super::doctor;
use crate::controller::{Ctx, Effect};
use crate::setup::probe;

type Handled = anyhow::Result<Vec<Effect>>;

/// Where Ollama listens when `endpoints.ollama.base_url` names nowhere else.
const OLLAMA_URL: &str = "http://localhost:11434/v1";

/// `asked` is the place a flag named for turning it on.
pub(crate) async fn add(ctx: &mut Ctx, name: &str, asked: Option<SettingScope>) -> Handled {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("{name:?} isn't an agent name: use letters, digits, and dashes.");
    }
    match name.as_str() {
        "ollama" => ollama(ctx, asked).await,
        "openclaw" => Ok(openclaw(ctx)),
        _ => cli(ctx, profile_id(&name), asked),
    }
}

/// The names people use for the built-in CLIs.
fn profile_id(name: &str) -> &str {
    match name {
        "claude" => "claude-code",
        "agy" | "antigravity" => "antigravity-cli",
        other => other,
    }
}

/// Checks the CLI off the loop, since a login check can take seconds, and turns it on where
/// your changes save when it's off.
fn cli(ctx: &mut Ctx, id: &str, asked: Option<SettingScope>) -> Handled {
    let env = Some(ctx.env.as_ref());
    let resolved = ctx.settings.resolved();
    let adapters = crate::commands::adapters_turning_on(env, resolved, &[id.to_owned()])?;
    let Some(adapter) = adapters.iter().find(|adapter| adapter.id() == id).cloned() else {
        own_cli(ctx, id);
        return Ok(Vec::new());
    };
    let counted = crate::commands::adapters_here(env, resolved)?
        .iter()
        .any(|adapter| adapter.id() == id);
    let on = counted
        && ctx
            .settings()
            .agents
            .get(id)
            .is_none_or(|agent| agent.enabled);
    let name = adapter.display_name().to_owned();
    let settings = ctx.settings.shared();
    let out = ctx.out.clone();
    let id_owned = id.to_owned();
    tokio::spawn(async move {
        let (mut agents, _) = doctor::check(&[adapter], &settings).await;
        let lines = match agents.first_mut() {
            Some(agent) => {
                // Whether it's on is said once, below, not in the doctor's words.
                agent.enabled = true;
                cli_lines(agent, on)
            }
            None => vec![format!("{id_owned} couldn't be checked.")],
        };
        out.send(AppEvent::Report {
            title: format!("Adding {name}"),
            lines,
        });
    });
    if on {
        return Ok(Vec::new());
    }
    Ok(vec![turn_on(format!("agents.{id}.enabled"), asked)])
}

fn cli_lines(agent: &probe::FoundAgent, on: bool) -> Vec<String> {
    let mut lines = vec![doctor::agent_line(agent)];
    if !agent.installed() {
        match &agent.hints.install {
            Some(install) => lines.push(format!("Install it with: {install}")),
            None => lines.push(format!("Install {} and put it on your PATH.", agent.id)),
        }
        lines.push(format!("Then /agents add {} checks it again.", agent.id));
    } else if agent.login == probe::LoginState::LoggedOut
        && let Some(login) = &agent.hints.login
    {
        lines.push(format!("Log in with: {login}"));
    }
    lines.push(if on {
        "It's on already.".to_owned()
    } else {
        format!("Turning it on: agents.{}.enabled = true.", agent.id)
    });
    lines
}

/// Looks for Ollama where your settings say, or at its usual address, and turns it on as a local
/// endpoint. Nothing leaves your machine.
async fn ollama(ctx: &mut Ctx, asked: Option<SettingScope>) -> Handled {
    let endpoint = ctx.settings().endpoints.get("ollama");
    let set_up = endpoint.is_some_and(|endpoint| endpoint.local);
    let url = endpoint
        .and_then(|endpoint| endpoint.base_url.clone())
        .unwrap_or_else(|| OLLAMA_URL.to_owned());
    let servers = probe::local_servers(ctx.settings()).await;
    let found = servers.iter().find(|server| server.id == "ollama");
    let mut lines = match found {
        Some(server) => {
            let mut lines = vec![format!(
                "Ollama answers at {} with {}: {}",
                server.base_url,
                count(server.models.len(), "model", "models"),
                server.models.join(", ")
            )];
            if let Some(first) = server.models.first() {
                lines.push(format!("Pick one with /model, such as ollama/{first}."));
            }
            lines
        }
        None => vec![
            format!("Ollama isn't answering at {url}."),
            "Install it from https://ollama.com, start it with `ollama serve`, and pull a model \
             with `ollama pull <model>`. Then /agents add ollama checks it again."
                .to_owned(),
        ],
    };
    lines.push(if set_up {
        "It's set up already.".to_owned()
    } else {
        "Turning it on: endpoints.ollama.local = true.".to_owned()
    });
    ctx.send(AppEvent::Report {
        title: "Adding Ollama".to_owned(),
        lines,
    });
    if set_up {
        return Ok(Vec::new());
    }
    Ok(vec![turn_on("endpoints.ollama.local".to_owned(), asked)])
}

/// `key = true`, saved where your changes go, or in `asked`.
fn turn_on(key: String, asked: Option<SettingScope>) -> Effect {
    let pairs = vec![(key, "true".to_owned())];
    match asked {
        Some(scope) => Effect::SaveSettings { pairs, scope },
        None => Effect::SaveChanges { pairs },
    }
}

/// OpenClaw's ACP bridge runs its tools outside the task's sandbox, so it drives Argonauts Code
/// instead of joining the crew.
fn openclaw(ctx: &mut Ctx) -> Vec<Effect> {
    ctx.send(AppEvent::Report {
        title: "OpenClaw can't join the crew".to_owned(),
        lines: vec![
            "`openclaw acp` runs its tools in OpenClaw's Gateway, outside the task's worktree \
             and Argonauts Code's sandbox, so no profile runs it."
                .to_owned(),
            "/connect openclaw lets OpenClaw start and watch your runs over MCP instead."
                .to_owned(),
        ],
    });
    Vec::new()
}

/// A CLI with no profile: where it is, and the entry that would run it.
fn own_cli(ctx: &mut Ctx, name: &str) {
    let found = argonauts_conductor::boards::plan_usage::find_unsandboxed(name, None).ok();
    let file = ctx.env.paths.global_models();
    let mut lines = vec![match &found {
        Some(path) => format!("{name} is at {}.", path.display()),
        None => format!("{name} isn't on your PATH yet."),
    }];
    lines.push(format!(
        "Argonauts Code runs a CLI with no profile through a [[cli]] entry in {}. For an ACP \
         agent, start from this:",
        ctx.env.display(&file)
    ));
    lines.push(String::new());
    lines.extend(cli_entry(name, found.as_deref()).lines().map(str::to_owned));
    lines.push(String::new());
    lines.push(format!(
        "Then /agents add {name} checks it and turns it on."
    ));
    ctx.send(AppEvent::Report {
        title: format!("No profile for {name} yet"),
        lines,
    });
    ctx.notice(
        NoteLevel::Info,
        "Built-in profiles: hermes, opencode-acp, claude-code, codex, antigravity-cli, opencode.",
    );
}

/// The least an ACP agent's entry claims; conformance pins the rest.
fn cli_entry(name: &str, binary: Option<&Path>) -> String {
    let binary = binary.map_or_else(
        || format!("/absolute/path/to/{name}"),
        |path| path.display().to_string(),
    );
    format!(
        r#"[[cli]]
id = "{name}"
display_name = "{name}"
binary = "{binary}"
version_args = ["--version"]
tested_versions = []
provider = "from-model"
transport = "acp"
read_only_args = ["acp"]
build_args = ["acp"]
interactive_resume = []

[cli.acp]
modes = {{ read_only = "default", build = "default", experiment = "default" }}
model = "none"
effort = "none"
report = "final-prompt"
resume = "none"
steer = "cancel-and-prompt"
usage = ["end-turn"]
plan_usage = "none""#
    )
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::{AppEvent, UiCommand};
    use argonauts_core::input::{AgentsAction, SlashCommand};
    use argonauts_core::models::registry::RegistryFile;
    use argonauts_core::settings::SettingScope;

    use super::cli_entry;
    use crate::controller::base::test_support::{CliOptions, TestDriver};

    fn add(name: &str) -> UiCommand {
        UiCommand::Slash {
            command: SlashCommand::Agents {
                action: AgentsAction::Add {
                    name: name.to_owned(),
                },
            },
        }
    }

    async fn report(driver: &mut TestDriver, title: &str) -> Vec<String> {
        let event = driver
            .until(|event| matches!(event, AppEvent::Report { title: t, .. } if t.contains(title)))
            .await;
        match event {
            AppEvent::Report { lines, .. } => lines,
            _ => unreachable!(),
        }
    }

    /// The key saved and where, with nothing asked first.
    async fn saved(driver: &mut TestDriver) -> (String, SettingScope) {
        let event = driver
            .until(|event| {
                matches!(
                    event,
                    AppEvent::SettingChanged { .. } | AppEvent::Choice { .. }
                )
            })
            .await;
        match event {
            AppEvent::SettingChanged { key, scope, .. } => (key, scope),
            other => panic!("asked instead of saving: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_hermes_says_how_to_install_it_and_turns_it_on_everywhere() {
        let mut driver = TestDriver::start_with(CliOptions {
            flags: vec![r#"agents.hermes.binary="/nowhere/hermes""#.to_owned()],
            ..CliOptions::default()
        })
        .await;
        driver.send(add("Hermes")).await;
        let saved = saved(&mut driver).await;
        let enabled = "agents.hermes.enabled".to_owned();
        assert_eq!(saved, (enabled, SettingScope::Global));
        let lines = report(&mut driver, "Adding Hermes").await;
        assert!(
            lines[0].starts_with("Hermes Agent · not installed"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("checks it again")),
            "{lines:?}"
        );
        assert!(lines.last().unwrap().contains("Turning it on"), "{lines:?}");
        driver.exit().await;
    }

    #[tokio::test]
    async fn a_cli_that_is_on_already_asks_nothing() {
        let mut driver = TestDriver::start_with(CliOptions {
            flags: vec![r#"agents.opencode.binary="/nowhere/opencode""#.to_owned()],
            ..CliOptions::default()
        })
        .await;
        driver.send(add("opencode")).await;
        let lines = report(&mut driver, "Adding OpenCode").await;
        assert_eq!(lines.last().map(String::as_str), Some("It's on already."));
        let events = driver.exit().await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AppEvent::Choice { .. })),
            "{events:#?}"
        );
    }

    #[tokio::test]
    async fn ollama_that_isnt_running_says_how_to_start_it() {
        let mut driver = TestDriver::start_with(CliOptions {
            flags: vec![r#"endpoints.ollama.base_url="http://127.0.0.1:9/v1""#.to_owned()],
            ..CliOptions::default()
        })
        .await;
        driver.send(add("ollama")).await;
        let lines = report(&mut driver, "Adding Ollama").await;
        assert_eq!(lines[0], "Ollama isn't answering at http://127.0.0.1:9/v1.");
        assert!(lines[1].contains("ollama serve"), "{lines:?}");
        let saved = saved(&mut driver).await;
        let local = "endpoints.ollama.local".to_owned();
        assert_eq!(saved, (local, SettingScope::Global));
        driver.exit().await;
    }

    #[tokio::test]
    async fn openclaw_is_pointed_at_connect() {
        let mut driver = TestDriver::start().await;
        driver.send(add("openclaw")).await;
        let lines = report(&mut driver, "OpenClaw").await;
        assert!(lines[1].contains("/connect openclaw"), "{lines:?}");
        driver.exit().await;
    }

    #[tokio::test]
    async fn an_unknown_cli_gets_the_entry_it_needs() {
        let mut driver = TestDriver::start().await;
        driver.send(add("no-such-cli-here")).await;
        let lines = report(&mut driver, "No profile for no-such-cli-here").await;
        assert_eq!(lines[0], "no-such-cli-here isn't on your PATH yet.");
        assert!(
            lines.contains(&r#"id = "no-such-cli-here""#.to_owned()),
            "{lines:?}"
        );
        driver.exit().await;
    }

    #[test]
    fn the_suggested_entry_is_one_the_registry_accepts() {
        let text = cli_entry("my-agent", Some(std::path::Path::new("/opt/bin/my-agent")));
        let file: RegistryFile = toml::from_str(&text).unwrap();
        let check = argonauts_registry::clis::check_manifest(&file.clis[0], None);
        assert!(check.is_ok(), "{:?}", check.errors);
    }
}
