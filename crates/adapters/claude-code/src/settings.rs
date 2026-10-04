//! The `--settings` JSON every session gets. Under `-p` Claude Code silently ignores a settings
//! file that fails validation (2.1.284), so what is generated here is checked before it is used.

use std::collections::BTreeMap;

use anyhow::{bail, ensure};
use serde::Serialize;
use serde_json::Value;

/// The server name the worker gives the status tool in `mcp.json`.
pub const STATUS_SERVER: &str = "argonauts_status";

/// Every shell command, for a session with its own sandbox off.
const BASH: &str = "Bash";

/// The tool rules the adapter may write: Bash, and the fetches a direct turn in auto mode makes
/// without asking.
const TOOL_RULES: &[&str] = &[BASH, "WebFetch", "WebSearch"];

/// What a session's tools may do without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    /// A run's session: its tool list is the boundary.
    Listed,
    /// A direct turn in auto mode: every command and fetch, our sandbox holding them.
    Everything,
    /// A direct turn that asks: only what the CLI and your own rules allow.
    AsTheCliAsks,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSettings {
    /// A `-p` session otherwise takes messages from your other Claude Code sessions at once,
    /// which would let two blind drafters reach each other.
    cross_session_inbound: &'static str,
    /// Agents never write git state, and commits carry no tool attribution lines.
    attribution: Attribution,
    sandbox: Sandbox,
    #[serde(skip_serializing_if = "Option::is_none")]
    permissions: Option<Permissions>,
    /// The hooks in every settings file, and the mods of every installed plugin.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disable_all_hooks: bool,
    /// What your claude.ai account syncs, hidden for this session only.
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_claude_ai_skills: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_claude_ai_plugins: Option<bool>,
    /// Settings passed with `--settings` outrank your own, so a `false` here wins.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    enabled_plugins: BTreeMap<String, bool>,
}

/// What a session leaves out of your own Claude Code setup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeftOut {
    /// Your hooks, and with them every installed plugin's mod.
    pub hooks: bool,
    /// Everything synced from your claude.ai account, and these plugins by name.
    pub plugins: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct Attribution {
    commit: &'static str,
    pr: &'static str,
}

/// Keeps the commands the agent runs off the network. It fails closed: without
/// `failIfUnavailable` a sandbox that can't start runs commands unsandboxed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Sandbox {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fail_if_unavailable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_unsandboxed_commands: Option<bool>,
}

/// Which sandbox holds the shell commands the agent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSandbox {
    /// Claude Code's own, which runs each sandboxed command without asking.
    Own,
    /// None of Claude Code's: ours holds the whole session, since macOS won't start Claude Code's
    /// inside it, or `sandbox.mode = "none"` asked for none. An allow rule lets commands run
    /// without asking, as they did in its own. A read-only session's tool list has no Bash to
    /// allow.
    Off,
}

#[derive(Debug, Serialize)]
struct Permissions {
    allow: Vec<String>,
}

/// `status_tool` adds an allow rule for the status server, since `dontAsk` denies an MCP call
/// no rule allows and `--permission-prompts none` denies one that would prompt. `commands` says
/// which sandbox runs the agent's shell commands.
pub fn session_settings(
    status_tool: bool,
    commands: CommandSandbox,
    allowed: Allowed,
    left_out: &LeftOut,
) -> anyhow::Result<String> {
    let sandbox = match commands {
        CommandSandbox::Own => Sandbox {
            enabled: true,
            fail_if_unavailable: Some(true),
            allow_unsandboxed_commands: Some(false),
        },
        CommandSandbox::Off => Sandbox {
            enabled: false,
            fail_if_unavailable: None,
            allow_unsandboxed_commands: None,
        },
    };
    let mut allow = Vec::new();
    match allowed {
        Allowed::Listed if commands == CommandSandbox::Off => allow.push(BASH.to_owned()),
        Allowed::Everything => allow.extend(TOOL_RULES.iter().map(|rule| (*rule).to_owned())),
        Allowed::Listed | Allowed::AsTheCliAsks => {}
    }
    if status_tool {
        allow.push(format!("mcp__{STATUS_SERVER}"));
    }
    let settings = SessionSettings {
        cross_session_inbound: "refuse",
        attribution: Attribution { commit: "", pr: "" },
        sandbox,
        permissions: (!allow.is_empty()).then_some(Permissions { allow }),
        disable_all_hooks: left_out.hooks,
        sync_claude_ai_skills: left_out.plugins.as_ref().map(|_| false),
        sync_claude_ai_plugins: left_out.plugins.as_ref().map(|_| false),
        enabled_plugins: left_out
            .plugins
            .iter()
            .flatten()
            .map(|plugin| (plugin.clone(), false))
            .collect(),
    };
    validate(&serde_json::to_value(&settings)?)?;
    // Written from the struct, so the keys keep their order whatever serde_json's map is.
    Ok(serde_json::to_string(&settings)?)
}

/// Checks settings against the part of Claude Code's schema the adapter uses. Anything else is
/// refused, because a key Claude Code rejects would make it drop the whole file without a word.
pub fn validate(settings: &Value) -> anyhow::Result<()> {
    let Some(settings) = settings.as_object() else {
        bail!("Claude Code settings must be a JSON object");
    };
    for (key, value) in settings {
        match key.as_str() {
            "crossSessionInbound" => {
                let valid = matches!(value.as_str(), Some("accept" | "hold" | "refuse"));
                ensure!(
                    valid,
                    "crossSessionInbound must be accept, hold, or refuse, not {value}"
                );
            }
            "attribution" => check_fields(key, value, &["commit", "pr"], Value::is_string)?,
            "sandbox" => check_fields(
                key,
                value,
                &["enabled", "failIfUnavailable", "allowUnsandboxedCommands"],
                Value::is_boolean,
            )?,
            "permissions" => check_permissions(value)?,
            // Only ever turned off: the adapter never adds your hooks or synced extras back.
            "disableAllHooks" => ensure!(
                value == &Value::Bool(true),
                "disableAllHooks is only ever true here, not {value}"
            ),
            "syncClaudeAiSkills" | "syncClaudeAiPlugins" => ensure!(
                value == &Value::Bool(false),
                "{key} is only ever false here, not {value}"
            ),
            "enabledPlugins" => check_plugins(value)?,
            other => bail!("the adapter doesn't generate the Claude Code setting {other}"),
        }
    }
    Ok(())
}

fn check_fields(
    section: &str,
    value: &Value,
    known: &[&str],
    valid: fn(&Value) -> bool,
) -> anyhow::Result<()> {
    let Some(fields) = value.as_object() else {
        bail!("{section} must be a JSON object");
    };
    for (key, field) in fields {
        ensure!(
            known.contains(&key.as_str()),
            "{section}.{key} isn't a setting the adapter uses"
        );
        ensure!(valid(field), "{section}.{key} has the wrong type: {field}");
    }
    Ok(())
}

fn check_permissions(value: &Value) -> anyhow::Result<()> {
    let Some(fields) = value.as_object() else {
        bail!("permissions must be a JSON object");
    };
    for (key, rules) in fields {
        ensure!(
            key == "allow",
            "permissions.{key} isn't a setting the adapter uses"
        );
        let Some(rules) = rules.as_array() else {
            bail!("permissions.allow must be a list");
        };
        for rule in rules {
            let Some(rule) = rule.as_str() else {
                bail!("permissions.allow holds a rule that isn't a string: {rule}");
            };
            ensure!(
                TOOL_RULES.contains(&rule) || is_mcp_server_rule(rule),
                "permissions.allow only takes MCP server rules, Bash, WebFetch, and WebSearch, \
                 not {rule}"
            );
        }
    }
    Ok(())
}

/// Plugin ids, such as `lint-helper@claude-plugins-official`, each turned on or off.
fn check_plugins(value: &Value) -> anyhow::Result<()> {
    let Some(plugins) = value.as_object() else {
        bail!("enabledPlugins must be a JSON object");
    };
    for (id, on) in plugins {
        ensure!(
            !id.trim().is_empty(),
            "enabledPlugins holds a plugin with no name"
        );
        ensure!(
            on.is_boolean(),
            "enabledPlugins.{id} must be true or false, not {on}"
        );
    }
    Ok(())
}

/// `mcp__<server>`. Claude Code skips an allow glob without a literal server name, such as
/// `mcp__*`, so wildcards are refused here.
fn is_mcp_server_rule(rule: &str) -> bool {
    rule.strip_prefix("mcp__").is_some_and(|server| {
        !server.is_empty()
            && server
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn generated(status_tool: bool, commands: CommandSandbox) -> Value {
        direct(status_tool, commands, Allowed::Listed)
    }

    fn direct(status_tool: bool, commands: CommandSandbox, allowed: Allowed) -> Value {
        left_out(status_tool, commands, allowed, &LeftOut::default())
    }

    fn left_out(
        status_tool: bool,
        commands: CommandSandbox,
        allowed: Allowed,
        left_out: &LeftOut,
    ) -> Value {
        let settings = session_settings(status_tool, commands, allowed, left_out).unwrap();
        serde_json::from_str(&settings).unwrap()
    }

    #[test]
    fn an_isolated_session_turns_off_your_hooks_synced_extras_and_plugins() {
        let isolated = LeftOut {
            hooks: true,
            plugins: Some(vec![
                "lint-helper@claude-plugins-official".into(),
                "cc-plugin-you-should-know@builtin".into(),
            ]),
        };
        let settings = left_out(false, CommandSandbox::Off, Allowed::Listed, &isolated);
        assert_eq!(settings["disableAllHooks"], json!(true));
        assert_eq!(settings["syncClaudeAiSkills"], json!(false));
        assert_eq!(settings["syncClaudeAiPlugins"], json!(false));
        assert_eq!(
            settings["enabledPlugins"],
            json!({
                "cc-plugin-you-should-know@builtin": false,
                "lint-helper@claude-plugins-official": false
            })
        );
        validate(&settings).unwrap();

        let hooks_only = LeftOut {
            hooks: true,
            plugins: None,
        };
        let settings = left_out(false, CommandSandbox::Own, Allowed::Listed, &hooks_only);
        assert_eq!(settings["disableAllHooks"], json!(true));
        for key in [
            "syncClaudeAiSkills",
            "syncClaudeAiPlugins",
            "enabledPlugins",
        ] {
            assert!(settings.get(key).is_none(), "{key}");
        }
        let nothing = generated(false, CommandSandbox::Own);
        assert!(nothing.get("disableAllHooks").is_none());
    }

    /// Auto mode asks nothing, so commands and fetches are allowed. A direct turn that asks
    /// lets Claude Code ask about each, as it does when you run it.
    #[test]
    fn a_direct_turn_allows_by_how_it_asks() {
        for commands in [CommandSandbox::Own, CommandSandbox::Off] {
            let auto = direct(true, commands, Allowed::Everything);
            assert_eq!(
                auto["permissions"],
                json!({"allow": ["Bash", "WebFetch", "WebSearch", "mcp__argonauts_status"]})
            );
            validate(&auto).unwrap();
            let asks = direct(true, commands, Allowed::AsTheCliAsks);
            assert_eq!(
                asks["permissions"],
                json!({"allow": ["mcp__argonauts_status"]})
            );
        }
        assert!(
            direct(false, CommandSandbox::Off, Allowed::AsTheCliAsks)
                .get("permissions")
                .is_none()
        );
    }

    #[test]
    fn generated_settings_refuse_messages_and_clear_attribution() {
        let settings = generated(false, CommandSandbox::Own);
        assert_eq!(
            settings,
            json!({
                "crossSessionInbound": "refuse",
                "attribution": {"commit": "", "pr": ""},
                "sandbox": {
                    "enabled": true,
                    "failIfUnavailable": true,
                    "allowUnsandboxedCommands": false
                }
            })
        );
        let with_status = generated(true, CommandSandbox::Own);
        assert_eq!(
            with_status["permissions"],
            json!({"allow": ["mcp__argonauts_status"]})
        );
    }

    /// Inside our sandbox Claude Code's own can't start, so it is off and commands run without
    /// asking, as its own sandbox would have run them.
    #[test]
    fn inside_our_sandbox_its_own_is_off_and_commands_run() {
        let settings = generated(true, CommandSandbox::Off);
        assert_eq!(settings["sandbox"], json!({"enabled": false}));
        assert_eq!(
            settings["permissions"],
            json!({"allow": ["Bash", "mcp__argonauts_status"]})
        );
        assert_eq!(
            generated(false, CommandSandbox::Off)["permissions"],
            json!({"allow": ["Bash"]})
        );
        validate(&settings).unwrap();
    }

    #[test]
    fn validation_refuses_what_claude_would_silently_drop() {
        let bad = [
            json!([]),
            json!({"crossSessionInbound": "reject"}),
            json!({"attribution": {"commit": false}}),
            json!({"attribution": {"co-author": ""}}),
            json!({"sandbox": {"enabled": "yes"}}),
            json!({"sandbox": true}),
            json!({"permissions": {"allow": ["mcp__*"]}}),
            json!({"permissions": {"allow": ["Bash(*)"]}}),
            json!({"permissions": {"allow": ["Write"]}}),
            json!({"permissions": {"deny": []}}),
            json!({"permissions": {"allow": "mcp__x"}}),
            json!({"hooks": {}}),
            json!({"disableAllHooks": false}),
            json!({"syncClaudeAiSkills": true}),
            json!({"syncClaudeAiPlugins": "no"}),
            json!({"enabledPlugins": {"x@y": "off"}}),
            json!({"enabledPlugins": ["x@y"]}),
        ];
        for settings in bad {
            assert!(validate(&settings).is_err(), "{settings}");
        }
        let good = json!({
            "crossSessionInbound": "hold",
            "permissions": {"allow": ["mcp__argonauts-status_2"]},
            "disableAllHooks": true,
            "syncClaudeAiSkills": false,
            "syncClaudeAiPlugins": false,
            "enabledPlugins": {"memory-keeper@market": false, "x@y": true}
        });
        validate(&good).unwrap();
    }
}
