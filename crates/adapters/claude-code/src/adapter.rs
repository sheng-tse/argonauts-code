use std::path::Path;
use std::time::Duration;

use argonauts_core::settings::sessions::{Compaction, NativeFanout};
use argonauts_core::vendor_commands::{BuiltinCommand, VendorCommand};
use argonauts_core::vendor_sessions::{VendorScan, VendorSession, VendorTurn};
use argonauts_core::{
    Adapter, Capabilities, CommandSpec, LaunchTweak, PARENT_SESSION_ENV_VARS, SessionRequest,
    SetupHints, StopPlan, StreamParser, extract_version,
};

use crate::command::{PARENT_SESSION_STATE, build_command, user_message_line};
use crate::commands::{self, BUILTIN_COMMANDS};
use crate::parser::ClaudeStreamParser;
use crate::{sessions, tweaks};

/// The first version is the original recordings, the second the steering and resume ones.
pub const TESTED_VERSIONS: &[&str] = &["2.1.284", "2.1.285"];

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Adapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn binary(&self) -> &'static str {
        "claude"
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        build_command(self.binary(), req)
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(ClaudeStreamParser::new())
    }

    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    fn tested_versions(&self) -> &'static [&'static str] {
        TESTED_VERSIONS
    }

    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("haiku", Some("low"))
    }

    fn stop_plan(&self) -> StopPlan {
        // SIGINT ends the turn cleanly. SIGTERM exits with 143 and kills running Bash commands.
        StopPlan::escalating(Duration::from_secs(5), Duration::from_secs(5))
    }

    fn capabilities(&self) -> Capabilities {
        // The result carries total_cost_usd, and after --resume it covers the whole conversation.
        // 2.1.285 hands a message sent mid-turn to the model with the next tool result, inside
        // the running turn, so it gets no result of its own.
        Capabilities {
            reports_cost: true,
            input_joins_running_turn: true,
            ..Capabilities::default()
        }
    }

    /// With stream-json input, a message sent between turns starts the next turn, and one sent
    /// during a turn joins it. The model sees the latter beside a tool result and may dismiss it
    /// as injected text (2.1.285).
    fn input_line(&self, message: &str) -> Option<String> {
        Some(user_message_line(message))
    }

    fn compaction(&self, setting: Compaction) -> LaunchTweak {
        tweaks::compaction(setting)
    }

    fn native_fanout(&self, setting: NativeFanout) -> LaunchTweak {
        tweaks::native_fanout(setting)
    }

    fn user_hooks_off(&self) -> LaunchTweak {
        tweaks::user_hooks_off()
    }

    /// Opens a `-p` session in the interactive UI. Such sessions aren't in the `/resume` picker,
    /// so the id is passed. The UI restores the session's permission mode itself.
    fn interactive_resume(&self, session_id: &str, workdir: &Path) -> Option<CommandSpec> {
        let usable = !session_id.trim().is_empty() && !session_id.starts_with('-');
        usable.then(|| CommandSpec {
            env_remove: PARENT_SESSION_ENV_VARS
                .iter()
                .chain(PARENT_SESSION_STATE)
                .map(|v| (*v).to_owned())
                .collect(),
            ..CommandSpec::new(self.binary(), workdir).args(["--resume", session_id])
        })
    }

    fn vendor_sessions(
        &self,
        scan: &VendorScan,
        limit: usize,
    ) -> anyhow::Result<Vec<VendorSession>> {
        sessions::list(scan, limit)
    }

    fn vendor_session_turns(
        &self,
        scan: &VendorScan,
        session: &str,
        count: usize,
    ) -> anyhow::Result<Vec<VendorTurn>> {
        sessions::turns(scan, session, count)
    }

    fn default_family(&self) -> &'static str {
        "claude"
    }

    fn setup_hints(&self) -> SetupHints {
        SetupHints {
            // From code.claude.com/docs/en/setup, for macOS, Linux, and WSL.
            install: Some("curl -fsSL https://claude.ai/install.sh | bash".to_owned()),
            login: Some("claude auth login".to_owned()),
            // Checked on 2.1.285: exit 0 logged in, 1 with an empty HOME. No model call.
            login_check: Some(vec!["auth".to_owned(), "status".to_owned()]),
        }
    }

    fn builtin_commands(&self) -> &'static [BuiltinCommand] {
        BUILTIN_COMMANDS
    }

    fn discover_commands(&self, scan: &VendorScan) -> anyhow::Result<Vec<VendorCommand>> {
        Ok(commands::discover(scan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{SETTINGS_FILE, native_cap_args};
    use argonauts_core::settings::sessions::FanoutSwitch;
    use argonauts_core::{
        AuthMode, SessionRole, TweakEffect, VENDOR_KEY_ENV_VARS, report_json_schema, sha256_hex,
    };
    use serde_json::{Value, json};
    use std::path::PathBuf;

    const SESSION_ID: &str = "0b5f6c1e-aaaa-4bbb-8ccc-123456789abc";

    fn request(role: SessionRole) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from("/tmp/worktree"),
            prompt: "Fix the bug".into(),
            role,
            goal_hash: (role == SessionRole::Build).then(|| sha256_hex(b"Fix the bug")),
            ..SessionRequest::default()
        }
    }

    /// Every new field set, the way the worker launches a crew session.
    fn full_request() -> SessionRequest {
        SessionRequest {
            model: Some("haiku".into()),
            effort: Some("low".into()),
            keep_input_open: true,
            session_id: Some(SESSION_ID.into()),
            compaction: Some(Compaction::Auto),
            native_fanout: Some(NativeFanout::Limit { max: 2 }),
            mcp_config: Some("/tmp/state/s-001/mcp.json".into()),
            scratch_dir: Some("/tmp/state/s-001".into()),
            max_budget_usd: Some(0.75),
            json_schema: Some(report_json_schema()),
            env_set: vec![("ARGONAUTS_SESSION".into(), "s-001".into())],
            ..request(SessionRole::Build)
        }
    }

    fn command(req: &SessionRequest) -> CommandSpec {
        ClaudeCodeAdapter.command(req).unwrap()
    }

    fn value_after<'a>(spec: &'a CommandSpec, flag: &str) -> Option<&'a str> {
        let index = spec.args.iter().position(|a| a == flag)?;
        spec.args.get(index + 1).map(String::as_str)
    }

    /// The values of a flag that takes a list, up to the next flag.
    fn values_after<'a>(spec: &'a CommandSpec, flag: &str) -> Vec<&'a str> {
        let Some(index) = spec.args.iter().position(|a| a == flag) else {
            return Vec::new();
        };
        spec.args[index + 1..]
            .iter()
            .take_while(|a| !a.starts_with("--"))
            .map(String::as_str)
            .collect()
    }

    fn has(spec: &CommandSpec, arg: &str) -> bool {
        spec.args.iter().any(|a| a == arg)
    }

    fn removes(spec: &CommandSpec, var: &str) -> bool {
        spec.env_remove.iter().any(|v| v == var)
    }

    fn env<'a>(spec: &'a CommandSpec, var: &str) -> Option<&'a str> {
        spec.env_set
            .iter()
            .find(|(name, _)| name == var)
            .map(|(_, value)| value.as_str())
    }

    fn settings_file(spec: &CommandSpec) -> Value {
        let path = value_after(spec, "--settings").unwrap();
        let file = spec
            .files
            .iter()
            .find(|f| f.path == Path::new(path))
            .expect("the settings file is written before the spawn");
        serde_json::from_str(&file.contents).unwrap()
    }

    #[test]
    fn identity_and_versions() {
        let adapter = ClaudeCodeAdapter::new();
        assert_eq!(adapter.id(), "claude-code");
        assert_eq!(adapter.binary(), "claude");
        assert_eq!(adapter.display_name(), "Claude Code");
        assert_eq!(adapter.version_args(), ["--version"]);
        assert_eq!(
            adapter.parse_version("2.1.285 (Claude Code)\n").as_deref(),
            Some("2.1.285")
        );
        assert_eq!(adapter.parse_version("no version here"), None);
        assert_eq!(adapter.tested_versions(), ["2.1.284", "2.1.285"]);
        assert_eq!(adapter.cheapest_model(), ("haiku", Some("low")));
        assert!(adapter.wrap_up_input("wrap up").is_none());
        assert_eq!(adapter.stop_plan(), StopPlan::default());
        assert_eq!(adapter.default_family(), "claude");
        let caps = adapter.capabilities();
        assert!(caps.reports_cost && caps.live_usage && caps.resume_totals_include_earlier);
        assert!(caps.enforces_read_only && caps.reports_model);
        assert!(caps.input_joins_running_turn);
    }

    #[test]
    fn read_only_sessions_use_read_tools_and_dont_ask() {
        let spec = command(&request(SessionRole::ReadOnly));
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.cwd, PathBuf::from("/tmp/worktree"));
        assert_eq!(
            spec.args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-mode",
                "dontAsk",
                "--permission-prompts",
                "none",
                "--tools",
                "Read,Glob,Grep",
                "--disallowedTools",
                "SendMessage",
                "ListAgents",
                "--settings",
                r#"{"crossSessionInbound":"refuse","attribution":{"commit":"","pr":""},"sandbox":{"enabled":true,"failIfUnavailable":true,"allowUnsandboxedCommands":false}}"#,
            ]
        );
        assert_eq!(spec.stdin.as_deref(), Some("Fix the bug"));
        assert!(!spec.keep_stdin_open);
        assert!(spec.files.is_empty());
    }

    #[test]
    fn build_sessions_accept_edits_and_keep_a_todo_list() {
        let spec = command(&request(SessionRole::Build));
        assert_eq!(value_after(&spec, "--permission-mode"), Some("acceptEdits"));
        assert_eq!(
            value_after(&spec, "--tools"),
            Some("Read,Glob,Grep,Edit,Write,Bash,TaskCreate,TaskGet,TaskUpdate,TaskList")
        );
        assert_eq!(env(&spec, "CLAUDE_CODE_ENABLE_TODO_TOOLS"), Some("1"));
    }

    #[test]
    fn read_only_flags_until_approved() {
        for goal_hash in [None, Some(String::new()), Some("  ".to_owned())] {
            let unapproved = SessionRequest {
                goal_hash,
                ..request(SessionRole::Build)
            };
            let spec = command(&unapproved);
            assert_eq!(value_after(&spec, "--permission-mode"), Some("dontAsk"));
            assert_eq!(value_after(&spec, "--tools"), Some("Read,Glob,Grep"));
            assert!(env(&spec, "CLAUDE_CODE_ENABLE_TODO_TOOLS").is_none());
        }
        // The default request is read-only too.
        let default = SessionRequest {
            workdir: "/w".into(),
            prompt: "hi".into(),
            ..SessionRequest::default()
        };
        assert_eq!(
            value_after(&command(&default), "--tools"),
            Some("Read,Glob,Grep")
        );

        let approved = command(&request(SessionRole::Build));
        assert_eq!(
            value_after(&approved, "--permission-mode"),
            Some("acceptEdits")
        );
        assert!(value_after(&approved, "--tools").unwrap().contains("Edit"));
    }

    #[test]
    fn claude_strict_mcp_and_no_cross_session_when_isolated() {
        for role in [SessionRole::ReadOnly, SessionRole::Build] {
            let spec = command(&SessionRequest {
                mcp_config: Some("/tmp/state/s-001/mcp.json".into()),
                scratch_dir: Some("/tmp/state/s-001".into()),
                isolated: true,
                ..request(role)
            });
            assert_eq!(
                value_after(&spec, "--mcp-config"),
                Some("/tmp/state/s-001/mcp.json")
            );
            let mcp = spec.args.iter().position(|a| a == "--mcp-config").unwrap();
            assert_eq!(spec.args[mcp + 2], "--strict-mcp-config");
            assert_eq!(
                values_after(&spec, "--disallowedTools"),
                ["SendMessage", "ListAgents"]
            );
            assert_eq!(value_after(&spec, "--setting-sources"), Some("user"));

            let settings = settings_file(&spec);
            assert_eq!(settings["crossSessionInbound"], "refuse");
            assert_eq!(
                settings["permissions"]["allow"],
                json!(["mcp__argonauts_status"])
            );
            assert_eq!(settings["sandbox"]["enabled"], true);
            assert_eq!(settings["sandbox"]["failIfUnavailable"], true);
            assert_eq!(settings["sandbox"]["allowUnsandboxedCommands"], false);
            crate::settings::validate(&settings).unwrap();
        }
        // Without a status server no MCP server loads at all, and none is allowed.
        let spec = command(&SessionRequest {
            isolated: true,
            ..request(SessionRole::ReadOnly)
        });
        assert!(has(&spec, "--strict-mcp-config") && !has(&spec, "--mcp-config"));
        let settings: Value =
            serde_json::from_str(value_after(&spec, "--settings").unwrap()).unwrap();
        assert!(settings.get("permissions").is_none());
        assert_eq!(settings["crossSessionInbound"], "refuse");
    }

    #[test]
    fn claude_loads_its_own_agents_skills_plugins_and_servers_unless_isolated() {
        for role in [SessionRole::ReadOnly, SessionRole::Build] {
            let spec = command(&SessionRequest {
                mcp_config: Some("/tmp/state/s-001/mcp.json".into()),
                scratch_dir: Some("/tmp/state/s-001".into()),
                ..request(role)
            });
            // Argonauts' status server joins yours rather than replacing them.
            assert_eq!(
                value_after(&spec, "--mcp-config"),
                Some("/tmp/state/s-001/mcp.json")
            );
            for stripping in [
                "--strict-mcp-config",
                "--setting-sources",
                "--disable-slash-commands",
                "--bare",
                "--safe-mode",
                "--restricted",
            ] {
                assert!(!has(&spec, stripping), "{stripping} in {:?}", spec.args);
            }
            // Peers still can't reach it outside the blackboard.
            assert_eq!(
                values_after(&spec, "--disallowedTools"),
                ["SendMessage", "ListAgents"]
            );
            assert_eq!(settings_file(&spec)["crossSessionInbound"], "refuse");
        }
    }

    #[test]
    fn claude_name_and_attribution_settings() {
        let spec = command(&SessionRequest {
            scratch_dir: Some("/tmp/state/runs/run-01/card-002-owner".into()),
            ..request(SessionRole::Build)
        });
        assert_eq!(
            value_after(&spec, "--name"),
            Some("argonauts card-002-owner")
        );
        let path = value_after(&spec, "--settings").unwrap();
        assert_eq!(
            Path::new(path),
            Path::new("/tmp/state/runs/run-01/card-002-owner").join(SETTINGS_FILE)
        );
        let settings = settings_file(&spec);
        assert_eq!(settings["attribution"], json!({"commit": "", "pr": ""}));

        // Control characters never reach the name, and a nameless folder gives no --name.
        let odd = command(&SessionRequest {
            scratch_dir: Some("/tmp/state/s\u{1b}[31m-9".into()),
            ..request(SessionRole::Build)
        });
        assert_eq!(value_after(&odd, "--name"), Some("argonauts s[31m-9"));
        let root = command(&SessionRequest {
            scratch_dir: Some("/".into()),
            ..request(SessionRole::Build)
        });
        assert!(!has(&root, "--name"));
        assert!(!has(&command(&request(SessionRole::Build)), "--name"));
    }

    #[test]
    fn compaction_flags_per_harness() {
        let adapter = ClaudeCodeAdapter;
        assert_eq!(
            adapter.compaction(Compaction::Inherit),
            LaunchTweak::default()
        );
        let auto = adapter.compaction(Compaction::Auto);
        assert_eq!(auto.args, ["--autocompact", "auto"]);
        assert!(auto.env_set.is_empty());
        assert_eq!(auto.effect, TweakEffect::Exact);
        let off = adapter.compaction(Compaction::Off);
        assert!(off.args.is_empty());
        assert_eq!(
            off.env_set,
            [("DISABLE_AUTO_COMPACT".to_owned(), "1".to_owned())]
        );
        assert_eq!(off.effect, TweakEffect::Exact);

        let auto_spec = command(&SessionRequest {
            compaction: Some(Compaction::Auto),
            ..request(SessionRole::Build)
        });
        assert_eq!(value_after(&auto_spec, "--autocompact"), Some("auto"));
        // These outrank the flag, so an explicit auto clears them.
        for var in [
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
            "DISABLE_AUTO_COMPACT",
            "DISABLE_COMPACT",
        ] {
            assert!(removes(&auto_spec, var), "{var}");
        }
        let off_spec = command(&SessionRequest {
            compaction: Some(Compaction::Off),
            ..request(SessionRole::Build)
        });
        assert!(!has(&off_spec, "--autocompact"));
        assert_eq!(env(&off_spec, "DISABLE_AUTO_COMPACT"), Some("1"));
        for unset in [None, Some(Compaction::Inherit)] {
            let spec = command(&SessionRequest {
                compaction: unset,
                ..request(SessionRole::Build)
            });
            assert!(!has(&spec, "--autocompact"));
            assert!(env(&spec, "DISABLE_AUTO_COMPACT").is_none());
            assert!(!removes(&spec, "DISABLE_AUTO_COMPACT"));
        }
    }

    #[test]
    fn native_fanout_flags_per_harness() {
        let adapter = ClaudeCodeAdapter;
        let capped = adapter.native_fanout(NativeFanout::Limit { max: 2 });
        assert!(capped.args.is_empty());
        assert_eq!(
            capped.env_set,
            [
                (
                    "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS".to_owned(),
                    "2".to_owned()
                ),
                (
                    "CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH".to_owned(),
                    "1".to_owned()
                ),
            ]
        );
        assert_eq!(capped.effect, TweakEffect::Exact);
        let on = adapter.native_fanout(NativeFanout::Switch(FanoutSwitch::On));
        assert_eq!(on, LaunchTweak::default());
        for off in [
            NativeFanout::Switch(FanoutSwitch::Off),
            NativeFanout::Limit { max: 0 },
        ] {
            let tweak = adapter.native_fanout(off);
            assert_eq!(tweak.args, ["--disallowedTools", "Agent"]);
            assert_eq!(
                tweak.env_set,
                [("CLAUDE_CODE_DISABLE_WORKFLOWS".to_owned(), "1".to_owned())]
            );
            assert_eq!(tweak.effect, TweakEffect::Exact);
        }

        let with = |fanout| {
            command(&SessionRequest {
                native_fanout: fanout,
                ..request(SessionRole::Build)
            })
        };
        let capped = with(Some(NativeFanout::Limit { max: 2 }));
        assert!(value_after(&capped, "--tools").unwrap().ends_with(",Agent"));
        assert_eq!(
            env(&capped, "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS"),
            Some("2")
        );
        assert_eq!(
            env(&capped, "CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH"),
            Some("1")
        );
        let on = with(Some(NativeFanout::Switch(FanoutSwitch::On)));
        assert!(value_after(&on, "--tools").unwrap().ends_with(",Agent"));
        assert!(env(&on, "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS").is_none());

        let off = with(Some(NativeFanout::Switch(FanoutSwitch::Off)));
        assert!(!value_after(&off, "--tools").unwrap().contains("Agent"));
        assert_eq!(
            values_after(&off, "--disallowedTools"),
            ["SendMessage", "ListAgents", "Agent"]
        );
        assert_eq!(env(&off, "CLAUDE_CODE_DISABLE_WORKFLOWS"), Some("1"));

        // Unset keeps the tool set from before fan-out settings, and read-only never gets Agent.
        let unset = with(None);
        assert!(!value_after(&unset, "--tools").unwrap().contains("Agent"));
        assert_eq!(
            values_after(&unset, "--disallowedTools"),
            ["SendMessage", "ListAgents"]
        );
        let reader = command(&SessionRequest {
            native_fanout: Some(NativeFanout::Switch(FanoutSwitch::On)),
            ..request(SessionRole::ReadOnly)
        });
        assert_eq!(value_after(&reader, "--tools"), Some("Read,Glob,Grep"));
    }

    #[test]
    fn user_hooks_cant_be_turned_off_alone() {
        let tweak = ClaudeCodeAdapter.user_hooks_off();
        assert!(tweak.args.is_empty() && tweak.env_set.is_empty());
        assert!(
            matches!(tweak.effect, TweakEffect::NotPossible { note } if note.contains("--safe-mode"))
        );
        let spec = command(&SessionRequest {
            user_hooks_off: true,
            ..request(SessionRole::Build)
        });
        assert_eq!(spec, command(&request(SessionRole::Build)));
    }

    #[test]
    fn native_cap_backstop_args() {
        let spec = command(&SessionRequest {
            max_budget_usd: Some(0.5),
            ..request(SessionRole::Build)
        });
        assert_eq!(value_after(&spec, "--max-budget-usd"), Some("0.5"));
        let whole = command(&SessionRequest {
            max_budget_usd: Some(3.0),
            ..request(SessionRole::Build)
        });
        assert_eq!(value_after(&whole, "--max-budget-usd"), Some("3"));
        assert_eq!(
            native_cap_args(0.125).unwrap(),
            ["--max-budget-usd", "0.125"]
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(native_cap_args(bad).is_err(), "{bad}");
        }
        assert!(!has(
            &command(&request(SessionRole::Build)),
            "--max-budget-usd"
        ));
    }

    #[test]
    fn env_scrubbed_per_vendor() {
        let spec = command(&request(SessionRole::Build));
        for var in VENDOR_KEY_ENV_VARS {
            assert_eq!(
                removes(&spec, var),
                *var != "CLAUDE_CODE_OAUTH_TOKEN",
                "{var}"
            );
        }
        for var in PARENT_SESSION_ENV_VARS
            .iter()
            .chain(&["CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SESSION_ATTENDED"])
        {
            assert!(removes(&spec, var), "{var}");
        }
        assert!(!removes(&spec, "CLAUDE_CODE_EFFORT_LEVEL"));
        assert_eq!(env(&spec, "CLAUDE_CODE_DISABLE_AUTO_MEMORY"), Some("1"));

        let api = command(&SessionRequest {
            auth: AuthMode::ApiKey,
            ..request(SessionRole::Build)
        });
        assert!(!removes(&api, "ANTHROPIC_API_KEY"));
        assert!(!removes(&api, "ANTHROPIC_AUTH_TOKEN"));
        for var in [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "OPENAI_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "GITHUB_TOKEN",
            "CLAUDECODE",
        ] {
            assert!(removes(&api, var), "{var}");
        }

        // The request's own variables are added, but never one the auth mode removes.
        let full = command(&full_request());
        assert_eq!(env(&full, "ARGONAUTS_SESSION"), Some("s-001"));
        assert!(removes(&full, "CLAUDE_CODE_EFFORT_LEVEL"));
        for name in [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "",
            "A=B",
        ] {
            let req = SessionRequest {
                env_set: vec![(name.into(), "x".into())],
                ..request(SessionRole::Build)
            };
            assert!(ClaudeCodeAdapter.command(&req).is_err(), "{name:?}");
        }
    }

    #[test]
    fn claude_resume_passes_every_flag_again() {
        let launch = command(&full_request());
        assert_eq!(value_after(&launch, "--session-id"), Some(SESSION_ID));
        assert!(!has(&launch, "--resume"));

        let resume = command(&SessionRequest {
            resume_session: Some(SESSION_ID.into()),
            prompt: "Carry on".into(),
            ..full_request()
        });
        assert_eq!(value_after(&resume, "--resume"), Some(SESSION_ID));
        assert!(!has(&resume, "--session-id") && !has(&resume, "--continue"));
        let without = |spec: &CommandSpec, flag: &str| {
            let mut args = spec.args.clone();
            let i = args.iter().position(|a| a == flag).unwrap();
            args.drain(i..i + 2);
            args
        };
        assert_eq!(
            without(&resume, "--resume"),
            without(&launch, "--session-id")
        );
        for flag in [
            "--permission-mode",
            "--mcp-config",
            "--settings",
            "--name",
            "--autocompact",
            "--max-budget-usd",
            "--input-format",
        ] {
            assert!(has(&resume, flag), "{flag}");
        }
        assert_eq!(resume.env_set, launch.env_set);
        assert_eq!(resume.env_remove, launch.env_remove);
        assert_eq!(resume.files, launch.files);

        // The resumed id may be named twice, but a new id would fork the session.
        let same = SessionRequest {
            resume_session: Some(SESSION_ID.into()),
            session_id: Some(SESSION_ID.into()),
            ..full_request()
        };
        assert_eq!(command(&same).args, resume.args);
        let fork = SessionRequest {
            resume_session: Some("11111111-2222-4333-8444-555555555555".into()),
            ..full_request()
        };
        assert!(ClaudeCodeAdapter.command(&fork).is_err());
    }

    #[test]
    fn claude_input_line_is_a_user_message() {
        let adapter = ClaudeCodeAdapter;
        let line = adapter.input_line("Also check \"b\"\nthen stop").unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1, "one JSON line: {line:?}");
        let value: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(
            value,
            json!({"type": "user", "message": {"role": "user", "content": "Also check \"b\"\nthen stop"}})
        );

        // With input kept open the prompt is the first message, and stdin stays open.
        let spec = command(&full_request());
        assert_eq!(values_after(&spec, "--input-format"), ["stream-json"]);
        assert_eq!(
            spec.stdin.as_deref(),
            adapter.input_line("Fix the bug").as_deref()
        );
        assert!(spec.keep_stdin_open);
        assert!(!spec.args.iter().any(|a| a.contains("Fix the bug")));
    }

    #[test]
    fn interactive_resume_opens_the_same_session() {
        let spec = ClaudeCodeAdapter
            .interactive_resume(SESSION_ID, Path::new("/tmp/worktree"))
            .unwrap();
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, ["--resume", SESSION_ID]);
        assert_eq!(spec.cwd, PathBuf::from("/tmp/worktree"));
        for var in ["CLAUDECODE", "CLAUDE_CODE_SESSION_ATTENDED"] {
            assert!(spec.env_remove.iter().any(|v| v == var), "{var}");
        }
        assert!(spec.stdin.is_none());
        for bad in ["", "  ", "--dangerously-skip-permissions"] {
            assert!(
                ClaudeCodeAdapter
                    .interactive_resume(bad, Path::new("/w"))
                    .is_none()
            );
        }
    }

    #[test]
    fn setup_hints_install_login_and_check() {
        let hints = ClaudeCodeAdapter.setup_hints();
        assert_eq!(
            hints.install.as_deref(),
            Some("curl -fsSL https://claude.ai/install.sh | bash")
        );
        assert_eq!(hints.login.as_deref(), Some("claude auth login"));
        assert_eq!(hints.login_check.unwrap(), ["auth", "status"]);
    }

    #[test]
    fn schema_model_and_extra_args() {
        let schema = report_json_schema();
        let spec = command(&SessionRequest {
            model: Some("haiku".into()),
            effort: Some("low".into()),
            json_schema: Some(schema.clone()),
            extra_args: vec!["--add-dir".into(), "/tmp/extra".into()],
            ..request(SessionRole::Build)
        });
        assert_eq!(value_after(&spec, "--model"), Some("haiku"));
        assert_eq!(value_after(&spec, "--effort"), Some("low"));
        let passed: Value =
            serde_json::from_str(value_after(&spec, "--json-schema").unwrap()).unwrap();
        assert_eq!(passed, schema);
        assert_eq!(
            spec.args[spec.args.len() - 2..],
            ["--add-dir", "/tmp/extra"]
        );
        assert!(removes(&spec, "CLAUDE_CODE_EFFORT_LEVEL"));
    }

    #[test]
    fn bad_requests_are_rejected() {
        let adapter = ClaudeCodeAdapter;
        let cases = [
            SessionRequest {
                prompt: "  ".into(),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                workdir: PathBuf::new(),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                model: Some("--dangerously-skip-permissions".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                effort: Some(String::new()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                resume_session: Some("-x".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                session_id: Some("not-a-uuid".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                session_id: Some("0b5f6c1e-aaaa-4bbb-8ccc-123456789abZ".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                json_schema: Some(json!(["not", "an", "object"])),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                max_budget_usd: Some(0.0),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                scratch_dir: Some("relative/dir".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                scratch_dir: Some("/tmp/worktree/.argonauts".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                mcp_config: Some("mcp.json".into()),
                ..request(SessionRole::Build)
            },
            SessionRequest {
                mcp_config: Some("/tmp/worktree/.mcp.json".into()),
                ..request(SessionRole::Build)
            },
        ];
        for req in cases {
            assert!(adapter.command(&req).is_err(), "{req:?}");
        }
    }

    #[test]
    fn prompts_that_look_like_flags_stay_on_stdin() {
        let req = SessionRequest {
            prompt: "--help me".into(),
            ..request(SessionRole::ReadOnly)
        };
        let spec = command(&req);
        assert!(!spec.args.iter().any(|a| a.contains("help me")));
        assert_eq!(spec.stdin.as_deref(), Some("--help me"));
    }

    /// macOS refuses Claude Code's sandbox inside ours, so a session we sandbox turns its own off
    /// and lets the agent's commands run, as `sandbox.mode = "none"` does, while the tool list
    /// still keeps a read-only session from having Bash at all.
    #[test]
    fn with_its_own_sandbox_off_commands_run() {
        for role in [SessionRole::ReadOnly, SessionRole::Build] {
            let spec = command(&SessionRequest {
                mcp_config: Some("/tmp/state/s-001/mcp.json".into()),
                scratch_dir: Some("/tmp/state/s-001".into()),
                tool_sandbox_off: true,
                ..request(role)
            });
            let settings = settings_file(&spec);
            assert_eq!(settings["sandbox"], json!({"enabled": false}), "{role:?}");
            assert_eq!(
                settings["permissions"]["allow"],
                json!(["Bash", "mcp__argonauts_status"])
            );
            crate::settings::validate(&settings).unwrap();
            let tools = value_after(&spec, "--tools").unwrap();
            assert_eq!(
                tools.split(',').any(|tool| tool == "Bash"),
                role == SessionRole::Build
            );
        }
    }

    /// Claude Code reaches its API through `HTTPS_PROXY` (2.1.286 reads `HTTPS_PROXY`,
    /// `HTTP_PROXY`, and `NO_PROXY`), so no auth mode drops them.
    #[test]
    fn proxy_variables_stay() {
        for auth in [AuthMode::Subscription, AuthMode::ApiKey] {
            let req = SessionRequest {
                auth,
                ..request(SessionRole::Build)
            };
            let spec = command(&req);
            assert!(
                !spec
                    .env_remove
                    .iter()
                    .any(|name| name.to_ascii_uppercase().contains("PROXY")),
                "{auth:?}: {:?}",
                spec.env_remove
            );
        }
    }
}
