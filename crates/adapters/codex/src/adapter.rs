use std::path::Path;

use argonauts_core::keywords::Keyword;
use argonauts_core::limits::{QuotaAnswer, QuotaReading};
use argonauts_core::settings::sessions::{Compaction, NativeFanout};
use argonauts_core::vendor_commands::{BuiltinCommand, VendorCommand};
use argonauts_core::vendor_sessions::{VendorScan, VendorSession, VendorTurn};
use argonauts_core::{
    Adapter, Capabilities, CommandSpec, LaunchTweak, PARENT_SESSION_ENV_VARS, SessionRequest,
    SetupHints, StopPlan, StreamParser, extract_version,
};
use time::OffsetDateTime;

use crate::command::{self, flag_value};
use crate::commands::BUILTIN_COMMANDS;
use crate::parser::CodexParser;
use crate::{commands, probe, sessions, tweaks};

pub const ID: &str = "codex";

pub const CHEAPEST_MODEL: &str = "gpt-5.6-luna";

pub const FALLBACK_CHEAPEST_MODEL: &str = "gpt-6-luna";

pub const CHEAPEST_EFFORT: &str = "low";

pub const OUTPUT_SCHEMA_FILE: &str = "codex-output-schema.json";

pub const TESTED_VERSIONS: &[&str] = &["0.158.0", "0.160.0"];

/// Words Codex treats specially in a prompt: none in 0.160.0. Its `ultra` is a reasoning level
/// picked in the model menu, not a word you type.
pub const KEYWORDS: &[Keyword] = &[];

pub const API_KEY_ENV_VARS: &[&str] = &["CODEX_API_KEY", "OPENAI_API_KEY", "AZURE_OPENAI_API_KEY"];

/// A ChatGPT workspace access token, which signs Codex in to the user's plan.
pub const SUBSCRIPTION_ENV_VARS: &[&str] = &["CODEX_ACCESS_TOKEN"];

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Adapter for CodexAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn binary(&self) -> &'static str {
        "codex"
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        command::build(self.binary(), req)
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(CodexParser::new())
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
        (CHEAPEST_MODEL, Some(CHEAPEST_EFFORT))
    }

    fn stop_plan(&self) -> StopPlan {
        // Codex turns SIGINT into a clean turn interrupt. It has no SIGTERM handler.
        StopPlan::default()
    }

    fn capabilities(&self) -> Capabilities {
        // Usage comes only with turn.completed, and thread.started doesn't name the model.
        Capabilities {
            live_usage: false,
            reports_model: false,
            ..Capabilities::default()
        }
    }

    /// `codex exec` reads no input once started, so steering interrupts and resumes with the
    /// message. `codex queue` can't reach an exec session's in-process app server.
    fn input_line(&self, _message: &str) -> Option<String> {
        None
    }

    /// `app-server` reads the account's limits, which no model changes.
    fn quota_probe(&self, workdir: &Path, _model: Option<&str>) -> Option<CommandSpec> {
        Some(probe::command(self.binary(), workdir))
    }

    fn parse_quota_probe(&self, stdout: &str) -> Option<QuotaReading> {
        probe::parse(stdout, OffsetDateTime::now_utc())
    }

    fn parse_quota_probe_answer(&self, stdout: &str) -> Option<QuotaAnswer> {
        probe::answer(stdout, OffsetDateTime::now_utc())
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

    /// Codex reads the project's AGENTS.md into every session, `exec` too.
    fn instruction_files(&self, workdir: &Path, _isolated: bool) -> Vec<String> {
        let name = "AGENTS.md";
        if workdir.join(name).is_file() {
            vec![name.to_owned()]
        } else {
            Vec::new()
        }
    }

    /// Opens exec sessions too, by id. Names would match only interactive sessions.
    fn interactive_resume(&self, session_id: &str, workdir: &Path) -> Option<CommandSpec> {
        let id = flag_value("session id", session_id.trim()).ok()?;
        let dir = command::utf8(workdir).ok()?;
        let mut spec = CommandSpec::new(self.binary(), workdir).args(["resume", "--cd", dir, id]);
        spec.env_remove = PARENT_SESSION_ENV_VARS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        Some(spec)
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
        "gpt"
    }

    fn setup_hints(&self) -> SetupHints {
        SetupHints {
            install: Some("npm install -g @openai/codex".to_owned()),
            login: Some("codex login".to_owned()),
            // Exits 0 when logged in and 1 when not, without a model call (codex-cli 0.158.0).
            login_check: Some(vec!["login".to_owned(), "status".to_owned()]),
        }
    }

    // `codex update` updates a standalone install where the install supports it.
    fn update_args(&self) -> Option<&'static [&'static str]> {
        Some(&["update"])
    }

    fn builtin_commands(&self) -> &'static [BuiltinCommand] {
        BUILTIN_COMMANDS
    }

    fn keywords(&self) -> &'static [Keyword] {
        KEYWORDS
    }

    fn discover_commands(&self, scan: &VendorScan) -> anyhow::Result<Vec<VendorCommand>> {
        Ok(commands::discover(scan))
    }

    fn command_prompt(&self, command: &VendorCommand, args: &str) -> Option<String> {
        commands::prompt(command, args)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::command::output_schema_path;
    use argonauts_core::registry::LaunchHints;
    use argonauts_core::settings::sessions::FanoutSwitch;
    use argonauts_core::{
        AuthMode, SessionRole, TweakEffect, VENDOR_KEY_ENV_VARS, report_json_schema,
    };
    use serde_json::Value;

    fn request(role: SessionRole) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from("/work/tree"),
            prompt: "Create hello.txt".into(),
            model: Some(CHEAPEST_MODEL.into()),
            role,
            ..SessionRequest::default()
        }
    }

    fn command(req: &SessionRequest) -> CommandSpec {
        CodexAdapter.command(req).unwrap()
    }

    fn position(args: &[String], needle: &str) -> usize {
        args.iter()
            .position(|a| a == needle)
            .unwrap_or_else(|| panic!("{needle} missing from {args:?}"))
    }

    fn value_after<'a>(args: &'a [String], flag: &str) -> &'a str {
        &args[position(args, flag) + 1]
    }

    /// The values of every `-c` override.
    fn overrides(args: &[String]) -> Vec<&str> {
        args.windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1].as_str())
            .collect()
    }

    #[test]
    fn read_only_role_uses_read_only_sandbox() {
        let spec = command(&request(SessionRole::ReadOnly));
        assert_eq!(spec.program, "codex");
        assert_eq!(
            spec.args,
            [
                "exec",
                "--json",
                "--color",
                "never",
                "--cd",
                "/work/tree",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--model",
                "gpt-5.6-luna",
                "-",
            ]
        );
        assert_eq!(spec.cwd, PathBuf::from("/work/tree"));
        assert_eq!(spec.stdin.as_deref(), Some("Create hello.txt"));
        assert!(!spec.keep_stdin_open);
        assert!(spec.files.is_empty());
    }

    #[test]
    fn codex_passes_your_model_and_always_its_sandbox() {
        for role in [
            SessionRole::ReadOnly,
            SessionRole::Build,
            SessionRole::Direct,
        ] {
            let spec = command(&request(role));
            assert_eq!(value_after(&spec.args, "--model"), CHEAPEST_MODEL);
            assert!(!spec.args.iter().any(|arg| arg == "--output-schema"));
            let expected = match role {
                SessionRole::ReadOnly => "read-only",
                SessionRole::Build | SessionRole::Direct => "workspace-write",
            };
            assert_eq!(value_after(&spec.args, "--sandbox"), expected);
        }
        // With no model chosen, Codex runs its own default.
        for model in [None, Some("  ")] {
            let req = SessionRequest {
                model: model.map(Into::into),
                ..request(SessionRole::Build)
            };
            let spec = CodexAdapter.command(&req).unwrap();
            assert!(
                !spec.args.iter().any(|arg| arg == "--model"),
                "{:?}",
                spec.args
            );
        }
        // Flags that loosen the sandbox or pick a session by recency are refused.
        for extra in [
            "-a",
            "--autopilot",
            "--last",
            "--sandbox=danger-full-access",
            "-sdanger-full-access",
            "--dangerously-bypass-approvals-and-sandbox",
            "--worktree",
            "--cd",
        ] {
            let req = SessionRequest {
                extra_args: vec![extra.into()],
                ..request(SessionRole::Build)
            };
            assert!(CodexAdapter.command(&req).is_err(), "{extra} was accepted");
        }
        let fine = SessionRequest {
            extra_args: vec!["--add-dir".into(), "/extra".into()],
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&fine).is_ok());
    }

    /// macOS refuses Codex's sandbox inside ours, so a session we sandbox runs with Codex's off and
    /// ours keeps a read-only one from writing. `sandbox.mode = "none"` asks for the same flags.
    #[test]
    fn with_its_own_sandbox_off_it_has_full_access() {
        for role in [SessionRole::ReadOnly, SessionRole::Build] {
            let spec = command(&SessionRequest {
                tool_sandbox_off: true,
                ..request(role)
            });
            assert_eq!(value_after(&spec.args, "--sandbox"), "danger-full-access");
            assert!(!spec.args.iter().any(|a| a.starts_with("--dangerously")));
        }
    }

    #[test]
    fn codex_loads_its_own_skills_plugins_and_servers() {
        for role in [SessionRole::ReadOnly, SessionRole::Build] {
            let spec = command(&SessionRequest {
                native_fanout: Some(NativeFanout::default()),
                ..request(role)
            });
            let stripping = overrides(&spec.args)
                .into_iter()
                .filter(|value| {
                    ["mcp_servers", "skills", "plugins", "features"]
                        .iter()
                        .any(|key| value.starts_with(key))
                })
                .collect::<Vec<_>>();
            assert!(stripping.is_empty(), "{stripping:?}");
            assert!(!spec.args.iter().any(|a| a.starts_with("--ignore")));
            assert!(!spec.args.iter().any(|a| a == "--profile"));
        }
    }

    #[test]
    fn read_only_flags_until_approved() {
        let spec = command(&request(SessionRole::ReadOnly));
        assert_eq!(value_after(&spec.args, "--sandbox"), "read-only");
        assert!(!spec.args.iter().any(|a| a.contains("workspace-write")));
        assert!(!spec.args.iter().any(|a| a == "-a" || a == "--autopilot"));

        // A read-only session that must not fan out, such as a judge, also turns agents off.
        let judge = SessionRequest {
            native_fanout: Some(NativeFanout::Switch(FanoutSwitch::Off)),
            ..request(SessionRole::ReadOnly)
        };
        let spec = command(&judge);
        assert_eq!(value_after(&spec.args, "--disable"), "multi_agent");
        assert_eq!(overrides(&spec.args), ["agents.enabled=false"]);

        // Build sessions only after approval; the adapter keeps the role's sandbox.
        let build = SessionRequest {
            goal_hash: Some("abc".into()),
            native_fanout: Some(NativeFanout::Switch(FanoutSwitch::Off)),
            ..request(SessionRole::Build)
        };
        let spec = command(&build);
        assert_eq!(value_after(&spec.args, "--sandbox"), "workspace-write");
        assert!(overrides(&spec.args).is_empty());
        assert!(CodexAdapter.capabilities().enforces_read_only);
    }

    #[test]
    fn codex_oss_launch_hints() {
        let req = SessionRequest {
            model: Some("ignored-when-local".into()),
            launch: Some(LaunchHints {
                oss: true,
                local_provider: Some("ollama".into()),
                provider_model: Some("gpt-oss:20b".into()),
            }),
            ..request(SessionRole::Build)
        };
        let spec = command(&req);
        let oss = position(&spec.args, "--oss");
        assert_eq!(spec.args[oss + 1..oss + 3], ["--local-provider", "ollama"]);
        assert_eq!(value_after(&spec.args, "--model"), "gpt-oss:20b");

        // Without a provider model, the request's model names the local one.
        let lmstudio = SessionRequest {
            model: Some("qwen3-coder".into()),
            launch: Some(LaunchHints {
                oss: true,
                local_provider: Some("lmstudio".into()),
                provider_model: None,
            }),
            ..request(SessionRole::Build)
        };
        let spec = command(&lmstudio);
        assert_eq!(value_after(&spec.args, "--local-provider"), "lmstudio");
        assert_eq!(value_after(&spec.args, "--model"), "qwen3-coder");

        // --oss without a provider fails in Codex, so it's refused here.
        for (oss, provider) in [(true, None), (true, Some("vllm")), (false, Some("ollama"))] {
            let req = SessionRequest {
                launch: Some(LaunchHints {
                    oss,
                    local_provider: provider.map(Into::into),
                    provider_model: None,
                }),
                ..request(SessionRole::Build)
            };
            assert!(CodexAdapter.command(&req).is_err(), "{oss} {provider:?}");
        }
        let hosted = SessionRequest {
            launch: Some(LaunchHints::default()),
            ..request(SessionRole::Build)
        };
        assert!(!command(&hosted).args.iter().any(|a| a == "--oss"));
    }

    #[test]
    fn codex_resume_passes_every_flag_again() {
        let req = SessionRequest {
            resume_session: Some("0199a213-81c0-7800-8aa1-bbab2a035a53".into()),
            effort: Some("low".into()),
            json_schema: Some(report_json_schema()),
            scratch_dir: Some(PathBuf::from("/state/card")),
            extra_args: vec!["--add-dir".into(), "/extra".into()],
            compaction: Some(Compaction::Auto),
            native_fanout: Some(NativeFanout::Limit { max: 2 }),
            user_hooks_off: true,
            launch: Some(LaunchHints {
                oss: true,
                local_provider: Some("ollama".into()),
                provider_model: None,
            }),
            ..request(SessionRole::Build)
        };
        let fresh = command(&SessionRequest {
            resume_session: None,
            ..req.clone()
        });
        let resumed = command(&req);
        let resume = position(&resumed.args, "resume");
        assert_eq!(
            &resumed.args[..resume],
            &fresh.args[..fresh.args.len() - 1],
            "a resume repeats every flag of a fresh launch"
        );
        assert_eq!(
            &resumed.args[resume..],
            ["resume", "0199a213-81c0-7800-8aa1-bbab2a035a53", "-"]
        );
        assert_eq!(resumed.stdin, fresh.stdin);
        for flag in [
            "--cd",
            "--sandbox",
            "--model",
            "--oss",
            "--output-schema",
            "-c",
        ] {
            assert!(
                position(&resumed.args, flag) < resume,
                "{flag} after resume"
            );
        }
    }

    #[test]
    fn compaction_flags_per_harness() {
        let adapter = CodexAdapter::new();
        assert_eq!(
            adapter.compaction(Compaction::Inherit),
            LaunchTweak::default()
        );
        let auto = adapter.compaction(Compaction::Auto);
        assert_eq!(auto.args, ["-c", "model_auto_compact_token_limit=10000000"]);
        assert_eq!(auto.effect, TweakEffect::Exact);
        let off = adapter.compaction(Compaction::Off);
        assert_eq!(off.args, auto.args);
        assert!(matches!(&off.effect, TweakEffect::Approximated { note } if note.contains("90%")));

        let req = SessionRequest {
            compaction: Some(Compaction::Off),
            ..request(SessionRole::Build)
        };
        assert_eq!(
            overrides(&command(&req).args),
            ["model_auto_compact_token_limit=10000000"]
        );
        let inherit = SessionRequest {
            compaction: Some(Compaction::Inherit),
            ..request(SessionRole::Build)
        };
        assert!(overrides(&command(&inherit).args).is_empty());
    }

    #[test]
    fn native_fanout_flags_per_harness() {
        let adapter = CodexAdapter::new();
        let capped = adapter.native_fanout(NativeFanout::Limit { max: 2 });
        assert_eq!(
            capped.args,
            ["-c", "agents.max_concurrent_threads_per_session=2"]
        );
        assert_eq!(capped.effect, TweakEffect::Exact);
        let off = adapter.native_fanout(NativeFanout::Switch(FanoutSwitch::Off));
        assert_eq!(off.args, ["--disable", "multi_agent"]);
        assert_eq!(off.effect, TweakEffect::Exact);
        assert!(
            adapter
                .native_fanout(NativeFanout::Switch(FanoutSwitch::On))
                .args
                .is_empty()
        );

        let req = SessionRequest {
            native_fanout: Some(NativeFanout::default()),
            ..request(SessionRole::Build)
        };
        assert_eq!(
            overrides(&command(&req).args),
            ["agents.max_concurrent_threads_per_session=3"]
        );
    }

    #[test]
    fn codex_loads_agents_md_itself() {
        let dir = tempfile::tempdir().unwrap();
        assert!(CodexAdapter.instruction_files(dir.path(), false).is_empty());
        std::fs::write(dir.path().join("AGENTS.md"), "x").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "x").unwrap();
        assert_eq!(
            CodexAdapter.instruction_files(dir.path(), true),
            ["AGENTS.md"]
        );
    }

    #[test]
    fn user_hooks_are_off_through_the_hooks_feature() {
        let tweak = CodexAdapter.user_hooks_off();
        assert_eq!(tweak.args, ["--disable", "hooks"]);
        assert!(matches!(tweak.effect, TweakEffect::Approximated { .. }));
        let req = SessionRequest {
            user_hooks_off: true,
            ..request(SessionRole::Build)
        };
        assert_eq!(value_after(&command(&req).args, "--disable"), "hooks");
        assert!(
            !command(&request(SessionRole::Build))
                .args
                .iter()
                .any(|a| a == "hooks")
        );
    }

    #[test]
    fn env_scrubbed_per_vendor() {
        let req = SessionRequest {
            env_set: vec![
                ("ARGONAUTS_SESSION".into(), "s-001".into()),
                ("ANTHROPIC_API_KEY".into(), "sk-ant".into()),
                ("CODEX_API_KEY".into(), "sk-codex".into()),
            ],
            ..request(SessionRole::ReadOnly)
        };
        // Subscription: every key goes but the plan's access token, and no key comes back
        // through env_set.
        let spec = command(&req);
        for var in VENDOR_KEY_ENV_VARS {
            let removed = spec.env_remove.iter().any(|v| v == var);
            assert_eq!(removed, *var != "CODEX_ACCESS_TOKEN", "{var}");
        }
        for var in PARENT_SESSION_ENV_VARS {
            assert!(spec.env_remove.iter().any(|v| v == var), "{var}");
        }
        assert_eq!(spec.env_set, [("ARGONAUTS_SESSION".into(), "s-001".into())]);

        // API key: only OpenAI's keys stay, and this process's own key is set.
        let api = SessionRequest {
            auth: AuthMode::ApiKey,
            ..req
        };
        let spec = command(&api);
        for var in VENDOR_KEY_ENV_VARS {
            let removed = spec.env_remove.iter().any(|v| v == var);
            assert_eq!(removed, !API_KEY_ENV_VARS.contains(var), "{var}");
        }
        assert_eq!(
            spec.env_set,
            [
                ("ARGONAUTS_SESSION".into(), "s-001".into()),
                ("CODEX_API_KEY".into(), "sk-codex".into())
            ]
        );

        // The quota probe reads the plan, so it never sees an API key.
        let probe = CodexAdapter.quota_probe(Path::new("/w"), None).unwrap();
        assert!(probe.env_remove.iter().any(|v| v == "OPENAI_API_KEY"));
    }

    #[test]
    fn cheapest_model_and_effort_become_flags() {
        let (model, effort) = CodexAdapter.cheapest_model();
        assert_eq!((model, effort), ("gpt-5.6-luna", Some("low")));
        let req = SessionRequest {
            effort: effort.map(Into::into),
            ..request(SessionRole::ReadOnly)
        };
        let spec = command(&req);
        assert_eq!(
            value_after(&spec.args, "--config"),
            "model_reasoning_effort=\"low\""
        );
        assert_eq!(spec.args.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn odd_effort_and_flag_like_values_are_rejected() {
        let bad_effort = SessionRequest {
            effort: Some("low\" --x=\"".into()),
            ..request(SessionRole::ReadOnly)
        };
        assert!(CodexAdapter.command(&bad_effort).is_err());
        let bad_model = SessionRequest {
            model: Some("--dangerously-bypass-approvals-and-sandbox".into()),
            ..request(SessionRole::ReadOnly)
        };
        assert!(CodexAdapter.command(&bad_model).is_err());
        let bad_resume = SessionRequest {
            resume_session: Some("--last".into()),
            ..request(SessionRole::ReadOnly)
        };
        assert!(CodexAdapter.command(&bad_resume).is_err());
    }

    #[test]
    fn empty_prompt_or_workdir_is_an_error() {
        let no_prompt = SessionRequest {
            prompt: " \n".into(),
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&no_prompt).is_err());
        let no_workdir = SessionRequest {
            workdir: PathBuf::new(),
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&no_workdir).is_err());
    }

    #[test]
    fn schema_is_written_to_the_scratch_dir() {
        let req = SessionRequest {
            json_schema: Some(report_json_schema()),
            scratch_dir: Some(PathBuf::from("/state/cards/task-001")),
            ..request(SessionRole::Build)
        };
        let spec = command(&req);
        let path = output_schema_path(Path::new("/state/cards/task-001"));
        assert_eq!(
            value_after(&spec.args, "--output-schema"),
            path.to_str().unwrap()
        );
        assert_eq!(spec.files.len(), 1);
        assert_eq!(spec.files[0].path, path);
        let written: Value = serde_json::from_str(&spec.files[0].contents).unwrap();
        assert_eq!(written, report_json_schema());
    }

    #[test]
    fn schema_needs_a_scratch_dir_outside_the_worktree() {
        let missing = SessionRequest {
            json_schema: Some(report_json_schema()),
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&missing).is_err());
        let inside = SessionRequest {
            json_schema: Some(report_json_schema()),
            scratch_dir: Some(PathBuf::from("/work/tree/.scratch")),
            ..request(SessionRole::Build)
        };
        assert!(CodexAdapter.command(&inside).is_err());
    }

    #[test]
    fn prompt_never_appears_in_args() {
        let req = SessionRequest {
            prompt: "-rf secret plan".into(),
            ..request(SessionRole::Build)
        };
        let spec = command(&req);
        assert!(!spec.args.iter().any(|a| a.contains("secret plan")));
        assert_eq!(spec.stdin.as_deref(), Some("-rf secret plan"));
    }

    #[test]
    fn steering_interrupts_and_take_over_opens_the_session() {
        let adapter = CodexAdapter::new();
        assert!(adapter.input_line("focus on the tests").is_none());
        let spec = adapter
            .interactive_resume(
                "0199a213-81c0-7800-8aa1-bbab2a035a53",
                Path::new("/work/tree"),
            )
            .unwrap();
        assert_eq!(
            spec.args,
            [
                "resume",
                "--cd",
                "/work/tree",
                "0199a213-81c0-7800-8aa1-bbab2a035a53"
            ]
        );
        assert_eq!(spec.cwd, PathBuf::from("/work/tree"));
        assert!(spec.env_remove.iter().any(|v| v == "CLAUDECODE"));
        assert!(
            adapter
                .interactive_resume("--last", Path::new("/w"))
                .is_none()
        );
    }

    #[test]
    fn identity_versions_and_setup() {
        let adapter = CodexAdapter::new();
        assert_eq!(adapter.id(), "codex");
        assert_eq!(adapter.binary(), "codex");
        assert_eq!(adapter.display_name(), "Codex");
        assert_eq!(adapter.default_family(), "gpt");
        assert_eq!(adapter.version_args(), ["--version"]);
        assert_eq!(
            adapter.parse_version("codex-cli 0.158.0\n").as_deref(),
            Some("0.158.0")
        );
        assert_eq!(adapter.parse_version("codex-cli"), None);
        assert_eq!(adapter.tested_versions(), ["0.158.0", "0.160.0"]);
        assert!(adapter.wrap_up_input("wrap up").is_none());
        assert_eq!(adapter.stop_plan(), StopPlan::default());
        let caps = adapter.capabilities();
        assert!(!caps.live_usage && !caps.reports_model && !caps.reports_cost);
        let hints = adapter.setup_hints();
        assert_eq!(hints.login_check.unwrap(), ["login", "status"]);
        assert_eq!(hints.login.as_deref(), Some("codex login"));
        assert!(hints.install.unwrap().contains("@openai/codex"));
    }

    /// Codex's reqwest client reads `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, and `NO_PROXY`
    /// (0.159.3), so no auth mode drops them.
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
