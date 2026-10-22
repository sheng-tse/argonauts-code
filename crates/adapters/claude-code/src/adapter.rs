use std::path::Path;
use std::time::Duration;

use argonauts_core::app::UltraColour;
use argonauts_core::cli_commands::{BuiltinCommand, CliCommand};
use argonauts_core::cli_sessions::{CliScan, CliSession, CliTurn};
use argonauts_core::events::context::ContextDetail;
use argonauts_core::keywords::Keyword;
use argonauts_core::limits::{PlanUsageAnswer, PlanUsageReading};
use argonauts_core::screen::ScreenRequest;
use argonauts_core::settings::sessions::{Compaction, NativeFanout};
use argonauts_core::{
    Adapter, Capabilities, CommandSpec, LaunchTweak, PARENT_SESSION_ENV_VARS, SessionRequest,
    SetupHints, StopPlan, StreamParser, UltraMode, UltraOn, extract_version,
};
use time::OffsetDateTime;

use crate::command::{PARENT_SESSION_STATE, build_command, user_message_line};
use crate::commands::{self, BUILTIN_COMMANDS};
use crate::keywords::KEYWORDS;
use crate::parser::ClaudeStreamParser;
use crate::{command, control, instructions, isolation, plan_usage, sessions, tweaks};

/// The versions the recordings in `fixtures/` were made with or also match, oldest first,
/// read from the `.meta.json` beside each recording when the crate is built.
pub const TESTED_VERSIONS: &[&str] = include!(concat!(env!("OUT_DIR"), "/tested_versions.rs"));

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

/// `system/init`, with what the parser takes from it.
const STARTUP_MESSAGE: argonauts_core::adapter::StartupMessage =
    argonauts_core::adapter::StartupMessage {
        marker: r#""subtype":"init""#,
        is: &[("/type", "system"), ("/subtype", "init")],
        fields: &[
            argonauts_core::adapter::StartupField {
                pointer: "/session_id",
                what: "its session",
                without: "its conversations can't be resumed",
            },
            argonauts_core::adapter::StartupField {
                pointer: "/model",
                what: "its model",
                without: "/context can't size its window",
            },
            argonauts_core::adapter::StartupField {
                pointer: "/tools",
                what: "its tools",
                without: "isolated sessions can't be checked for what they loaded",
            },
            argonauts_core::adapter::StartupField {
                pointer: "/claude_code_version",
                what: "its version",
                without: "a new version is only noticed by Doctor",
            },
        ],
    };

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

    fn instruction_files(&self, workdir: &Path, isolated: bool) -> Vec<String> {
        let config = isolation::your_config_dir();
        instructions::loaded(workdir, config.as_deref(), isolated)
    }

    fn session_parser(&self, req: &SessionRequest) -> Box<dyn StreamParser> {
        if req.isolated {
            Box::new(ClaudeStreamParser::isolated())
        } else {
            self.parser()
        }
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

    /// Claude Code 2.1.284 writes a `tool_progress` line every 30 seconds during a long tool
    /// call in the main conversation, and never while the model thinks or inside a subagent.
    fn heartbeat(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }

    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("haiku", Some("low"))
    }

    fn stop_plan(&self) -> StopPlan {
        // SIGINT ends the turn cleanly. SIGTERM exits with 143 and kills running Bash commands.
        StopPlan::stepped(Duration::from_secs(5), Duration::from_secs(5))
    }

    fn holds_answer_format(&self) -> bool {
        true
    }

    /// Its own ultracode, drawn in Claude Code's ultracode purple.
    fn ultra_mode(&self) -> Option<UltraMode> {
        Some(UltraMode {
            on: UltraOn::Ultracode,
            colour: UltraColour::Purple,
        })
    }

    fn capabilities(&self) -> Capabilities {
        // The result carries total_cost_usd, and after --resume it covers the whole conversation.
        // 2.1.285 hands a message sent mid-turn to the model with the next tool result, inside
        // the running turn, so it gets no result of its own.
        // 2.1.289 also takes each message with a uuid it reports progress by, hands waiting
        // messages over at once on send now, and lets a screen join the session. An isolated
        // session's settings turn your plugins, hooks, and synced extras off, and its init is
        // checked for any that loaded anyway.
        Capabilities {
            reports_cost: true,
            input_joins_running_turn: true,
            send_now: true,
            screen: true,
            isolates: true,
            ..Capabilities::default()
        }
    }

    /// With stream-json input, a message sent between turns starts the next turn, and one sent
    /// during a turn joins it. The model sees the latter beside a tool result and may dismiss it
    /// as injected text (2.1.285).
    fn input_line(&self, message: &str) -> Option<String> {
        Some(user_message_line(message))
    }

    fn message_line(&self, message: &str, uuid: &str) -> Option<String> {
        Some(command::user_message_line_with_uuid(message, uuid))
    }

    fn picture_line(
        &self,
        message: &str,
        pictures: &[argonauts_core::pictures::Picture],
        uuid: &str,
    ) -> Option<String> {
        Some(command::picture_message_line_with_uuid(
            message, pictures, uuid,
        ))
    }

    fn send_now_line(&self, uuid: &str) -> Option<String> {
        Some(control::send_now_line(uuid))
    }

    fn cancel_line(&self, uuid: &str) -> Option<String> {
        Some(control::cancel_line(uuid))
    }

    fn screen_line(&self, request: &ScreenRequest) -> Option<String> {
        Some(control::screen_line(request))
    }

    fn context_usage_line(&self, detail: ContextDetail) -> Option<String> {
        Some(control::context_usage_line(detail))
    }

    fn plan_usage_probe(&self, workdir: &Path, model: Option<&str>) -> Option<CommandSpec> {
        let model = model.unwrap_or(self.cheapest_model().0);
        Some(plan_usage::probe_command(self.binary(), workdir, model))
    }

    fn parse_plan_usage_probe(&self, stdout: &str) -> Option<PlanUsageReading> {
        self.parse_plan_usage_probe_end(stdout)?.binding().cloned()
    }

    fn parse_plan_usage_probe_answer(&self, stdout: &str) -> Option<PlanUsageAnswer> {
        plan_usage::answer(stdout, OffsetDateTime::now_utc())
    }

    fn parse_plan_usage_probe_end(&self, stdout: &str) -> Option<PlanUsageAnswer> {
        plan_usage::answer_at_end(stdout, OffsetDateTime::now_utc())
    }

    fn parse_plan_usage_probe_early(&self, stdout: &str) -> Option<PlanUsageAnswer> {
        plan_usage::windows_first(stdout, OffsetDateTime::now_utc())
    }

    fn stream_reports_plan_usage(&self) -> bool {
        true
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
    fn interactive_resume(&self, req: &SessionRequest) -> Option<CommandSpec> {
        let (session_id, workdir) = (req.resume_session.as_deref()?, &req.workdir);
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

    fn cli_sessions(&self, scan: &CliScan, limit: usize) -> anyhow::Result<Vec<CliSession>> {
        sessions::list(scan, limit)
    }

    fn cli_session_turns(
        &self,
        scan: &CliScan,
        session: &str,
        count: usize,
    ) -> anyhow::Result<Vec<CliTurn>> {
        sessions::turns(scan, session, count)
    }

    fn default_provider(&self) -> &'static str {
        "anthropic"
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

    fn update_args(&self) -> Option<&'static [&'static str]> {
        Some(&["update"])
    }

    /// The names are Claude Code's own: its cask and its package, and its installer asks the
    /// same address for the newest version (`claude update`, read from 2.1.296).
    fn install_sources(&self) -> argonauts_core::adapter::InstallSources {
        argonauts_core::adapter::InstallSources {
            homebrew: Some("claude-code"),
            npm: Some("@anthropic-ai/claude-code"),
            own: Some(argonauts_core::adapter::OwnInstaller {
                path_part: "/claude/versions/",
                newest_url: Some("https://downloads.claude.ai/claude-code-releases/latest"),
            }),
        }
    }

    fn updater_off_env(&self) -> &'static [(&'static str, &'static str)] {
        &[(crate::command::AUTO_UPDATER_ENV, "1")]
    }

    fn startup_message(&self) -> Option<&'static argonauts_core::adapter::StartupMessage> {
        Some(&STARTUP_MESSAGE)
    }

    fn builtin_commands(&self) -> &'static [BuiltinCommand] {
        BUILTIN_COMMANDS
    }

    fn keywords(&self) -> &'static [Keyword] {
        KEYWORDS
    }

    fn discover_commands(&self, scan: &CliScan) -> anyhow::Result<Vec<CliCommand>> {
        Ok(commands::discover(scan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{SETTINGS_FILE, native_cap_args};
    use argonauts_core::settings::sessions::FanoutSwitch;
    use argonauts_core::{
        Asks, AuthMode, PROVIDER_KEY_ENV_VARS, SessionRole, TweakEffect, report_json_schema,
        sha256_hex,
    };
    use serde_json::{Value, json};
    use std::path::PathBuf;

    const SESSION_ID: &str = "0b5f6c1e-aaaa-4bbb-8ccc-123456789abc";

    fn request(role: SessionRole) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from("/tmp/worktree"),
            prompt: "Fix the bug".into(),
            role,
            goal_hash: (role != SessionRole::ReadOnly).then(|| sha256_hex(b"Fix the bug")),
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
            spend_cap_usd: Some(0.75),
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

    /// A Claude Code folder whose settings enable one plugin and turn another off, with one more
    /// installed.
    fn your_claude_settings() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"enabledPlugins": {"lint-helper@claude-plugins-official": true,
                                   "off-already@market": false}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
        std::fs::write(
            dir.path().join("plugins/installed_plugins.json"),
            r#"{"version": 2, "plugins": {"memory-keeper@market": []}}"#,
        )
        .unwrap();
        dir
    }

    fn claude_config_dir(dir: &Path) -> Vec<(String, String)> {
        vec![(
            "CLAUDE_CONFIG_DIR".into(),
            dir.to_string_lossy().into_owned(),
        )]
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
        assert_eq!(adapter.tested_versions(), TESTED_VERSIONS);
        assert_eq!(adapter.cheapest_model(), ("haiku", Some("low")));
        assert!(adapter.wrap_up_input("wrap up").is_none());
        assert_eq!(adapter.stop_plan(), StopPlan::default());
        assert_eq!(adapter.default_provider(), "anthropic");
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
                "Read,Glob,Grep,ToolSearch",
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
            Some(
                "Read,Glob,Grep,Edit,Write,Bash,ToolSearch,TaskCreate,TaskGet,TaskUpdate,TaskList"
            )
        );
        assert_eq!(env(&spec, "CLAUDE_CODE_ENABLE_TODO_TOOLS"), Some("1"));
    }

    /// A `--tools` list without ToolSearch turns Claude Code's tool search off, and then every
    /// MCP tool of your setup goes out in full with each request. A run's session keeps it, and
    /// a read-only one still gets nothing that writes.
    #[test]
    fn run_sessions_keep_tool_search_and_a_read_only_one_still_cant_write() {
        let named = |spec: &CommandSpec| -> Vec<String> {
            let tools = value_after(spec, "--tools").expect("a tool list");
            tools.split(',').map(str::to_owned).collect()
        };
        for role in [SessionRole::ReadOnly, SessionRole::Build] {
            let tools = named(&command(&request(role)));
            assert!(tools.iter().any(|tool| tool == "ToolSearch"), "{tools:?}");
        }
        let tools = named(&command(&request(SessionRole::ReadOnly)));
        for writes in ["Edit", "Write", "Bash", "NotebookEdit"] {
            assert!(!tools.iter().any(|tool| tool == writes), "{tools:?}");
        }
        // The conversation's own session names no tools, so its tool search was never off.
        assert_eq!(
            value_after(&command(&request(SessionRole::Direct)), "--tools"),
            None
        );
    }

    /// Ultracode is Claude Code's own setting, which leaves the effort alone, and the size
    /// guideline goes with it. A session that can't run a workflow gets neither.
    #[test]
    fn ultracode_and_the_workflow_size_go_through_the_settings() {
        use argonauts_core::settings::workflows::WorkflowSize;
        let on = command(&SessionRequest {
            ultracode: true,
            workflow_size: Some(WorkflowSize::None),
            asks: Asks::Never,
            scratch_dir: Some("/tmp/state/t-002".into()),
            ..request(SessionRole::Direct)
        });
        let settings = settings_file(&on);
        assert_eq!(settings["ultracode"], json!(true));
        assert_eq!(settings["workflowSizeGuideline"], json!("unrestricted"));
        assert!(
            !on.args.iter().any(|arg| arg.contains("ultracode")),
            "{:?}",
            on.args
        );
        let read_only = command(&SessionRequest {
            ultracode: true,
            workflow_size: Some(WorkflowSize::Small),
            scratch_dir: Some("/tmp/state/t-003".into()),
            ..request(SessionRole::ReadOnly)
        });
        let settings = settings_file(&read_only);
        assert!(settings.get("ultracode").is_none());
        assert!(settings.get("workflowSizeGuideline").is_none());
    }

    /// A build session that may run its own workflow gets the tool, and no one is there to
    /// review a workflow before it runs.
    #[test]
    fn a_build_session_with_its_own_workflow_runs_one_unasked() {
        let spec = command(&SessionRequest {
            native_fanout: Some(NativeFanout::Switch(FanoutSwitch::On)),
            ultracode: true,
            scratch_dir: Some("/tmp/state/t-004".into()),
            ..request(SessionRole::Build)
        });
        assert!(
            value_after(&spec, "--tools")
                .unwrap()
                .ends_with(",Workflow")
        );
        let settings = settings_file(&spec);
        assert_eq!(settings["permissions"], json!({"allow": ["Workflow"]}));
        assert_eq!(settings["ultracode"], json!(true));
    }

    /// Your message goes out marked as typed by you only when you asked for a workflow with
    /// it, since the mark is what makes Claude Code's keyword work.
    #[test]
    fn a_message_you_typed_the_keyword_in_goes_out_as_yours() {
        let req = SessionRequest {
            keep_input_open: true,
            prompt: "ultracode tidy the date helpers".into(),
            ..request(SessionRole::Direct)
        };
        let plain = command(&req);
        assert!(!plain.stdin.as_deref().unwrap().contains("origin"));
        let typed = command(&SessionRequest {
            typed_by_you: true,
            ..req
        });
        let line: Value = serde_json::from_str(typed.stdin.as_deref().unwrap().trim()).unwrap();
        assert_eq!(line["origin"], json!({"kind": "human"}));
        assert_eq!(
            line["message"]["content"],
            json!("ultracode tidy the date helpers")
        );
    }

    /// A direct turn as the worker launches one: the session server for asking, a schema never.
    fn direct(asks: Asks, sandbox_off: bool) -> CommandSpec {
        command(&SessionRequest {
            asks,
            tool_sandbox_off: sandbox_off,
            mcp_config: Some("/tmp/state/t-001/mcp.json".into()),
            scratch_dir: Some("/tmp/state/t-001".into()),
            ..request(SessionRole::Direct)
        })
    }

    /// On autopilot a direct turn's file tools reach anywhere without asking, in its own
    /// sandbox or ours, and the never-list stays closed. Supervised asks as before.
    #[test]
    fn a_direct_turn_on_autopilot_reads_and_writes_anywhere_but_the_never_list() {
        use argonauts_core::outside::{Never, Outside};
        let outside = Outside {
            read: true,
            write: true,
            project: Some("/home/you/app".into()),
            never: Never {
                paths: vec!["/home/you/.ssh".into()],
                names: vec![".env".to_owned()],
                unwritten: vec!["/etc".into()],
            },
        };
        for sandbox_off in [true, false] {
            let spec = command(&SessionRequest {
                asks: Asks::Never,
                tool_sandbox_off: sandbox_off,
                outside: outside.clone(),
                mcp_config: Some("/tmp/state/t-001/mcp.json".into()),
                scratch_dir: Some("/tmp/state/t-001".into()),
                ..request(SessionRole::Direct)
            });
            assert_eq!(value_after(&spec, "--permission-mode"), Some("acceptEdits"));
            let permissions = &settings_file(&spec)["permissions"];
            let has_rule = |list: &str, rule: &str| {
                permissions[list]
                    .as_array()
                    .is_some_and(|rules| rules.iter().any(|r| r == rule))
            };
            for rule in ["Read(//**)", "Edit(//**)", "Write(//**)", "Bash"] {
                assert!(has_rule("allow", rule), "{rule}: {permissions}");
            }
            for rule in [
                "Read(//home/you/.ssh/**)",
                "Edit(//home/you/.ssh/**)",
                "Read(//**/.env)",
                "Edit(//etc/**)",
                "Edit(.argonauts/**)",
            ] {
                assert!(has_rule("deny", rule), "{rule}: {permissions}");
            }
        }
        let supervised = command(&SessionRequest {
            asks: Asks::LikeCli,
            mcp_config: Some("/tmp/state/t-001/mcp.json".into()),
            scratch_dir: Some("/tmp/state/t-001".into()),
            ..request(SessionRole::Direct)
        });
        assert!(has(&supervised, "--permission-prompt-tool"));
        let permissions = settings_file(&supervised)["permissions"].to_string();
        assert!(!permissions.contains("//"), "{permissions}");
    }

    /// A direct turn is Claude Code as you run it: its default tools and no `--tools` list,
    /// your memory, no schema. Auto mode accepts edits and allows commands and fetches inside
    /// our sandbox, and anything else that would ask is refused.
    #[test]
    fn a_direct_turn_in_auto_mode_has_the_default_tools_and_asks_nothing() {
        for sandbox_off in [true, false] {
            let spec = direct(Asks::Never, sandbox_off);
            assert!(!has(&spec, "--tools"), "{:?}", spec.args);
            assert!(!has(&spec, "--json-schema"));
            assert!(!has(&spec, "--permission-prompt-tool"));
            assert_eq!(value_after(&spec, "--permission-mode"), Some("acceptEdits"));
            assert_eq!(value_after(&spec, "--permission-prompts"), Some("none"));
            assert_eq!(
                values_after(&spec, "--disallowedTools"),
                ["SendMessage", "ListAgents"]
            );
            // No one reviews a workflow before it runs, so the workflow tool asks nothing too.
            assert_eq!(
                settings_file(&spec)["permissions"],
                json!({
                    "allow": [
                        "Bash", "WebFetch", "WebSearch", "mcp__argonauts_status", "Workflow"
                    ],
                    "deny": ["Edit(.argonauts/**)", "Write(.argonauts/**)", "NotebookEdit(.argonauts/**)"],
                })
            );
            assert!(env(&spec, "CLAUDE_CODE_DISABLE_AUTO_MEMORY").is_none());
            assert!(
                env(&spec, "ENABLE_CLAUDEAI_MCP_SERVERS").is_none(),
                "your connectors"
            );
            assert!(env(&spec, "CLAUDE_CODE_ENABLE_TODO_TOOLS").is_none());
            assert!(env(&spec, "MCP_TOOL_TIMEOUT").is_none());
            assert!(env(&spec, "CLAUDE_CODE_DISABLE_DANGEROUS_RM_TIMEOUT").is_none());
            // Not isolated: your agents, skills, plugins, and MCP servers load.
            assert!(!has(&spec, "--strict-mcp-config"));
            assert!(!has(&spec, "--setting-sources"));
        }
    }

    /// The standing lines are added to Claude Code's own prompt, never in place of it.
    #[test]
    fn standing_lines_are_appended_to_the_cli_s_own_prompt() {
        let mut req = request(SessionRole::Direct);
        assert!(!has(&command(&req), "--append-system-prompt"));
        req.standing_note = Some("  Hand real work to the crew.\n".to_owned());
        let spec = command(&req);
        assert_eq!(
            value_after(&spec, "--append-system-prompt"),
            Some("Hand real work to the crew.")
        );
        assert!(!has(&spec, "--system-prompt"));
        req.standing_note = Some("   ".to_owned());
        assert!(!has(&command(&req), "--append-system-prompt"));
    }

    /// A run on autopilot answers nobody's questions, so its sessions work in the style Claude
    /// Code has for that. Every other session keeps the style you set.
    #[test]
    fn a_session_on_autopilot_works_in_the_proactive_style() {
        let mut req = full_request();
        assert!(settings_file(&command(&req)).get("outputStyle").is_none());
        req.autopilot = true;
        let settings = settings_file(&command(&req));
        assert_eq!(settings["outputStyle"], "Proactive");
        assert_eq!(settings["sandbox"]["enabled"], true);
        crate::settings::validate(&settings).unwrap();
    }

    /// Supervised, `like_cli`: no permission mode, so yours applies, and every call Claude
    /// Code would ask about goes to the approval tool, which waits as long as you take.
    #[test]
    fn a_direct_turn_that_asks_like_the_cli_sends_its_prompts_to_the_approval_tool() {
        let spec = direct(Asks::LikeCli, true);
        assert!(!has(&spec, "--permission-mode"), "{:?}", spec.args);
        assert!(!has(&spec, "--permission-prompts"));
        assert!(!has(&spec, "--tools"));
        assert_eq!(
            value_after(&spec, "--permission-prompt-tool"),
            Some("mcp__argonauts_status__approve")
        );
        // No Bash rule: commands ask, as Claude Code without its own sandbox asks.
        assert_eq!(
            settings_file(&spec)["permissions"],
            json!({"allow": ["mcp__argonauts_status"], "deny": ["Edit(.argonauts/**)", "Write(.argonauts/**)", "NotebookEdit(.argonauts/**)"]})
        );
        assert_eq!(env(&spec, "MCP_TOOL_TIMEOUT"), Some("86400000"));
        // A flagged rm's question waits too, whatever default mode is yours.
        assert_eq!(
            env(&spec, "CLAUDE_CODE_DISABLE_DANGEROUS_RM_TIMEOUT"),
            Some("1")
        );
        // A direct turn is like its CLI, so none of the isolation keys.
        let settings = settings_file(&spec);
        for key in [
            "disableAllHooks",
            "syncClaudeAiSkills",
            "syncClaudeAiPlugins",
            "enabledPlugins",
        ] {
            assert!(settings.get(key).is_none(), "{key}");
        }

        let every = direct(Asks::EveryChange, true);
        assert_eq!(value_after(&every, "--permission-mode"), Some("manual"));
        assert!(has(&every, "--permission-prompt-tool"));
    }

    /// With nothing to ask through, a direct turn that should ask refuses instead, and one
    /// without your message's hash gets the read-only flags.
    #[test]
    fn a_direct_turn_without_its_tool_or_its_hash_falls_back_closed() {
        let unasked = command(&SessionRequest {
            asks: Asks::LikeCli,
            ..request(SessionRole::Direct)
        });
        assert_eq!(value_after(&unasked, "--permission-prompts"), Some("none"));
        assert!(!has(&unasked, "--permission-prompt-tool"));
        let unapproved = command(&SessionRequest {
            goal_hash: None,
            ..request(SessionRole::Direct)
        });
        assert_eq!(
            value_after(&unapproved, "--permission-mode"),
            Some("dontAsk")
        );
        assert_eq!(
            value_after(&unapproved, "--tools"),
            Some("Read,Glob,Grep,ToolSearch")
        );
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
            assert_eq!(
                value_after(&spec, "--tools"),
                Some("Read,Glob,Grep,ToolSearch")
            );
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
            Some("Read,Glob,Grep,ToolSearch")
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
        let config = your_claude_settings();
        for role in [SessionRole::ReadOnly, SessionRole::Build] {
            let spec = command(&SessionRequest {
                mcp_config: Some("/tmp/state/s-001/mcp.json".into()),
                scratch_dir: Some("/tmp/state/s-001".into()),
                isolated: true,
                env_set: claude_config_dir(config.path()),
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
            // None of your hooks, plugins, or synced extras.
            assert_eq!(settings["disableAllHooks"], true);
            assert_eq!(settings["syncClaudeAiSkills"], false);
            assert_eq!(settings["syncClaudeAiPlugins"], false);
            let plugins = &settings["enabledPlugins"];
            assert_eq!(plugins["lint-helper@claude-plugins-official"], false);
            assert_eq!(plugins["memory-keeper@market"], false);
            assert_eq!(plugins["cc-plugin-you-should-know@builtin"], false);
            assert!(plugins.get("cc-plugin-agents-md@builtin").is_none());
            assert!(plugins.get("off-already@market").is_none());
            crate::settings::validate(&settings).unwrap();
        }
        // Without a status server no MCP server loads at all, and none is allowed.
        let spec = command(&SessionRequest {
            isolated: true,
            env_set: claude_config_dir(config.path()),
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
            // Argonauts Code's status server joins yours rather than replacing them.
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
            // Other agents still can't reach it outside the blackboard.
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
            scratch_dir: Some("/tmp/state/runs/run-01/task-002-worker".into()),
            ..request(SessionRole::Build)
        });
        assert_eq!(
            value_after(&spec, "--name"),
            Some("argonauts task-002-worker")
        );
        let path = value_after(&spec, "--settings").unwrap();
        assert_eq!(
            Path::new(path),
            Path::new("/tmp/state/runs/run-01/task-002-worker").join(SETTINGS_FILE)
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
    fn compaction_flags_per_cli() {
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
    fn native_fanout_flags_per_cli() {
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
        // A capped session gets subagents and no workflow of its own, whose agents the cap
        // wouldn't count.
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
        // On lets Claude Code fan out as it sees fit, with a workflow of its own too.
        let on = with(Some(NativeFanout::Switch(FanoutSwitch::On)));
        assert!(
            value_after(&on, "--tools")
                .unwrap()
                .ends_with(",Agent,Workflow")
        );
        assert!(env(&on, "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS").is_none());
        assert!(env(&on, "CLAUDE_CODE_DISABLE_WORKFLOWS").is_none());

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
        assert!(!value_after(&unset, "--tools").unwrap().contains("Workflow"));
        assert_eq!(
            values_after(&unset, "--disallowedTools"),
            ["SendMessage", "ListAgents"]
        );
        let reader = command(&SessionRequest {
            native_fanout: Some(NativeFanout::Switch(FanoutSwitch::On)),
            ..request(SessionRole::ReadOnly)
        });
        assert_eq!(
            value_after(&reader, "--tools"),
            Some("Read,Glob,Grep,ToolSearch")
        );
    }

    /// A reviewer with the checks gets Bash, held by its settings to those commands; one with
    /// any shell gets Bash with no such rules, and one with none gets no Bash.
    #[test]
    fn a_reviewer_runs_the_checks_and_nothing_else() {
        use argonauts_core::session_runner::SessionShell;
        let reader = |shell: SessionShell| {
            command(&SessionRequest {
                shell,
                scratch_dir: Some("/tmp/state/s-001".into()),
                ..request(SessionRole::ReadOnly)
            })
        };
        let checks = reader(SessionShell::Only(vec!["node --test".to_owned()]));
        assert_eq!(
            value_after(&checks, "--tools"),
            Some("Read,Glob,Grep,ToolSearch,Bash")
        );
        assert_eq!(value_after(&checks, "--permission-mode"), Some("dontAsk"));
        let settings = settings_file(&checks);
        assert_eq!(
            settings["permissions"]["allow"],
            json!(["Bash(node --test)", "Bash(node --test:*)"])
        );
        let any = reader(SessionShell::Any);
        assert_eq!(
            value_after(&any, "--tools"),
            Some("Read,Glob,Grep,ToolSearch,Bash")
        );
        assert!(settings_file(&any)["permissions"].is_null());
        let none = reader(SessionShell::None);
        assert_eq!(
            value_after(&none, "--tools"),
            Some("Read,Glob,Grep,ToolSearch")
        );
    }

    /// A crew agent gets no workflow or background agent of Claude Code's own, whatever its
    /// fan-out, and no ultracode.
    #[test]
    fn a_crew_agent_starts_nothing_in_the_background() {
        let crew_agent = command(&SessionRequest {
            native_fanout: Some(NativeFanout::Switch(FanoutSwitch::On)),
            background_off: true,
            ultracode: true,
            scratch_dir: Some("/tmp/state/s-001".into()),
            ..request(SessionRole::Build)
        });
        let tools = value_after(&crew_agent, "--tools").unwrap();
        assert!(tools.ends_with(",Agent"), "{tools}");
        assert_eq!(
            values_after(&crew_agent, "--disallowedTools"),
            ["SendMessage", "ListAgents", "Workflow"]
        );
        assert_eq!(env(&crew_agent, "CLAUDE_CODE_DISABLE_WORKFLOWS"), Some("1"));
        assert_eq!(
            env(&crew_agent, "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS"),
            Some("1")
        );
        let settings = settings_file(&crew_agent);
        assert!(settings.get("ultracode").is_none(), "{settings}");

        // Off already switches workflows off, once.
        let off = command(&SessionRequest {
            native_fanout: Some(NativeFanout::Switch(FanoutSwitch::Off)),
            background_off: true,
            ..request(SessionRole::Build)
        });
        let switched = off
            .env_set
            .iter()
            .filter(|(name, _)| name == "CLAUDE_CODE_DISABLE_WORKFLOWS")
            .count();
        assert_eq!(switched, 1);
    }

    #[test]
    fn user_hooks_go_through_the_session_settings() {
        let tweak = ClaudeCodeAdapter.user_hooks_off();
        assert!(tweak.args.is_empty() && tweak.env_set.is_empty());
        assert_eq!(tweak.effect, TweakEffect::Exact);
        let off = settings_file(&command(&SessionRequest {
            user_hooks_off: true,
            scratch_dir: Some("/tmp/state/s-001".into()),
            ..request(SessionRole::Build)
        }));
        assert_eq!(off["disableAllHooks"], true);
        // Your plugins and synced skills stay: only an isolated session drops them.
        assert!(off.get("enabledPlugins").is_none());
        assert!(off.get("syncClaudeAiSkills").is_none());
        let kept = settings_file(&command(&SessionRequest {
            scratch_dir: Some("/tmp/state/s-001".into()),
            ..request(SessionRole::Build)
        }));
        assert!(kept.get("disableAllHooks").is_none());
    }

    /// What you set for the session you talk to reaches Claude Code in its `--settings` file.
    #[test]
    fn your_preferences_go_through_the_session_settings() {
        let yours = settings_file(&command(&SessionRequest {
            scratch_dir: Some("/tmp/state/s-001".into()),
            preferences: argonauts_core::session_runner::SessionPreferences {
                thinking_off: true,
                fast: true,
                language: Some("ja".into()),
                workflow_word_off: true,
            },
            ..request(SessionRole::Direct)
        }));
        assert_eq!(yours["alwaysThinkingEnabled"], false);
        assert_eq!(yours["fastMode"], true);
        assert_eq!(yours["language"], "ja");
        assert_eq!(yours["workflowKeywordTriggerEnabled"], false);
    }

    #[test]
    fn native_cap_backstop_args() {
        let spec = command(&SessionRequest {
            spend_cap_usd: Some(0.5),
            ..request(SessionRole::Build)
        });
        assert_eq!(value_after(&spec, "--max-budget-usd"), Some("0.5"));
        let whole = command(&SessionRequest {
            spend_cap_usd: Some(3.0),
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
    fn env_scrubbed_per_agent() {
        let spec = command(&request(SessionRole::Build));
        for var in PROVIDER_KEY_ENV_VARS {
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
        assert_eq!(env(&spec, "ENABLE_CLAUDEAI_MCP_SERVERS"), Some("false"));
        // No session switches versions halfway: Claude Code's own updater stays off.
        assert_eq!(env(&spec, "DISABLE_AUTOUPDATER"), Some("1"));
        assert_eq!(
            ClaudeCodeAdapter.updater_off_env(),
            [("DISABLE_AUTOUPDATER", "1")]
        );

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

        // The resumed id may be named twice, but only a fork gives the session a new id.
        let same = SessionRequest {
            resume_session: Some(SESSION_ID.into()),
            session_id: Some(SESSION_ID.into()),
            ..full_request()
        };
        assert_eq!(command(&same).args, resume.args);
        let renamed = SessionRequest {
            resume_session: Some("11111111-2222-4333-8444-555555555555".into()),
            ..full_request()
        };
        assert!(ClaudeCodeAdapter.command(&renamed).is_err());
    }

    /// `claude --resume <id> --fork-session` carries on a copy, which Claude Code gives an id
    /// of its own or the one chosen for it, and leaves the session as it was.
    #[test]
    fn claude_fork_resumes_a_copy_of_the_session() {
        const ORIGINAL: &str = "11111111-2222-4333-8444-555555555555";
        let resume = command(&SessionRequest {
            resume_session: Some(ORIGINAL.into()),
            session_id: None,
            ..full_request()
        });
        let fork = command(&SessionRequest {
            resume_session: Some(ORIGINAL.into()),
            session_id: None,
            fork_session: true,
            ..full_request()
        });
        assert_eq!(value_after(&fork, "--resume"), Some(ORIGINAL));
        assert!(has(&fork, "--fork-session") && !has(&fork, "--session-id"));
        let mut plain = fork.args.clone();
        plain.retain(|arg| arg != "--fork-session");
        assert_eq!(plain, resume.args, "a fork repeats every flag of a resume");

        // Claude Code takes `--session-id` beside `--resume` only on a fork.
        let chosen = command(&SessionRequest {
            resume_session: Some(ORIGINAL.into()),
            fork_session: true,
            ..full_request()
        });
        assert_eq!(value_after(&chosen, "--session-id"), Some(SESSION_ID));
        assert_eq!(value_after(&chosen, "--resume"), Some(ORIGINAL));
        assert!(has(&chosen, "--fork-session"));

        let nothing_to_copy = SessionRequest {
            fork_session: true,
            ..full_request()
        };
        assert!(ClaudeCodeAdapter.command(&nothing_to_copy).is_err());
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

        // Your conversation's session opens with `initialize`, then your message.
        let mut main = full_request();
        main.main_session = true;
        let stdin = command(&main).stdin.unwrap();
        let lines: Vec<Value> = stdin
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["request"]["subtype"], "initialize");
        assert_eq!(lines[1]["message"]["content"], "Fix the bug");
        // It keeps SendMessage, which is how the main agent passes your message on to one of
        // its subagents. A session that may start none goes without, as every other does.
        assert_eq!(
            values_after(&command(&main), "--disallowedTools"),
            ["ListAgents"]
        );
        assert_eq!(
            values_after(&command(&full_request()), "--disallowedTools"),
            ["SendMessage", "ListAgents"]
        );
        main.native_fanout = Some(NativeFanout::Switch(FanoutSwitch::Off));
        assert_eq!(
            values_after(&command(&main), "--disallowedTools"),
            ["SendMessage", "ListAgents", "Agent"]
        );

        // A message sent later carries the uuid its progress is reported by.
        let line = adapter.message_line("Also this", "u-1").unwrap();
        let value: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["uuid"], "u-1");
        assert!(adapter.capabilities().send_now && adapter.capabilities().screen);
    }

    #[test]
    fn interactive_resume_opens_the_same_session() {
        let spec = ClaudeCodeAdapter
            .interactive_resume(&SessionRequest::reopening(
                SESSION_ID,
                Path::new("/tmp/worktree"),
            ))
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
                    .interactive_resume(&SessionRequest::reopening(bad, Path::new("/w")))
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
                spend_cap_usd: Some(0.0),
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
