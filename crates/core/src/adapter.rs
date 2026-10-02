//! The traits each vendor adapter implements.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::driver::SessionDriver;
use crate::event::AgentEvent;
use crate::limits::QuotaReading;
use crate::session::{CommandSpec, SessionRequest};
use crate::settings::sessions::{Compaction, NativeFanout};
use crate::vendor_commands::{BuiltinCommand, CommandKind, VendorCommand};
use crate::vendor_sessions::{ResumeLaunch, VendorScan, VendorSession, VendorTurn};

pub const KNOWN_AGENT_IDS: &[&str] = &["claude-code", "codex", "antigravity-cli"];

/// Parsers never fail. A line they don't understand becomes [`AgentEvent::Unknown`].
/// Lines arrive without their trailing newline.
pub trait StreamParser: Send {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent>;
    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent>;
    /// Called once after the process exits, to flush state or report a missing result.
    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent>;
}

/// One vendor CLI, described well enough to run it headlessly.
pub trait Adapter: Send + Sync {
    /// Stable id, such as `claude-code`.
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn binary(&self) -> &'static str;
    /// Pure, so it can be unit tested.
    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec>;
    fn parser(&self) -> Box<dyn StreamParser>;
    fn version_args(&self) -> &'static [&'static str];
    fn parse_version(&self, output: &str) -> Option<String>;
    /// CLI versions whose recorded output the tests replay.
    fn tested_versions(&self) -> &'static [&'static str];
    /// Model and effort for live tests.
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>);
    fn stop_plan(&self) -> StopPlan;

    /// Stdin text that delivers `message` mid-run. `None` means the conductor interrupts instead.
    fn wrap_up_input(&self, message: &str) -> Option<String> {
        let _ = message;
        None
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    /// Like `wrap_up_input`, for sessions opened with `keep_input_open`. `None` means
    /// interrupt and resume.
    fn input_line(&self, message: &str) -> Option<String> {
        let _ = message;
        None
    }

    /// A command that reads quota without spending any, such as `agy -p "/usage"`. `model` is
    /// the one the vendor's sessions use, for a CLI that names a model on every call.
    fn quota_probe(&self, workdir: &Path, model: Option<&str>) -> Option<CommandSpec> {
        let _ = (workdir, model);
        None
    }

    fn parse_quota_probe(&self, stdout: &str) -> Option<QuotaReading> {
        let _ = stdout;
        None
    }

    fn compaction(&self, setting: Compaction) -> LaunchTweak {
        match setting {
            Compaction::Inherit => LaunchTweak::default(),
            _ => LaunchTweak {
                effect: TweakEffect::NotPossible {
                    note: "this harness has no per-launch compaction control".to_owned(),
                },
                ..LaunchTweak::default()
            },
        }
    }

    /// Flags and environment for the subagent cap.
    fn native_fanout(&self, setting: NativeFanout) -> LaunchTweak {
        let _ = setting;
        LaunchTweak {
            effect: TweakEffect::EnforcedByConductor,
            ..LaunchTweak::default()
        }
    }

    fn user_hooks_off(&self) -> LaunchTweak {
        LaunchTweak {
            effect: TweakEffect::NotPossible {
                note: "this harness can't skip user hooks per launch".to_owned(),
            },
            ..LaunchTweak::default()
        }
    }

    /// The vendor's interactive UI on the same session, for take-over:
    /// `claude --resume <id>`, `codex resume <id>`, `agy --conversation <id>`.
    fn interactive_resume(&self, session_id: &str, workdir: &Path) -> Option<CommandSpec> {
        let _ = (session_id, workdir);
        None
    }

    /// The model family when the registry has no entry, such as `claude` or `gpt`.
    fn default_family(&self) -> &'static str {
        "unknown"
    }

    /// Where the CLI's own config says its model is served, such as Hermes' `model.base_url`,
    /// for a CLI that takes its provider only from there. A server of yours makes the session
    /// free unless you set a price.
    fn model_base_url(&self) -> Option<String> {
        None
    }

    /// The default knows none, so the setup claims no login state.
    fn setup_hints(&self) -> SetupHints {
        SetupHints::default()
    }

    fn transport(&self) -> Transport {
        Transport::Lines
    }

    /// The driver for a two-way protocol, which gets the child's pipes instead of the parser.
    /// The harness worker still owns the process.
    fn session_driver(&self) -> Option<Arc<dyn SessionDriver>> {
        None
    }

    /// The CLI's own sessions that ran in `scan.folder`, newest first, at most `limit`. Reads
    /// only the CLI's session list and transcripts, never its credentials. Blocking file reads.
    fn vendor_sessions(
        &self,
        scan: &VendorScan,
        limit: usize,
    ) -> anyhow::Result<Vec<VendorSession>> {
        let _ = (scan, limit);
        Ok(Vec::new())
    }

    /// The last `count` turns of one of those sessions, oldest first, to reprint when it's
    /// resumed.
    fn vendor_session_turns(
        &self,
        scan: &VendorScan,
        session: &str,
        count: usize,
    ) -> anyhow::Result<Vec<VendorTurn>> {
        let _ = (scan, session, count);
        Ok(Vec::new())
    }

    /// How the chat's next turn carries on `session` through the CLI's own resume. The default
    /// suits a CLI whose `command` resumes [`SessionRequest::resume_session`].
    fn resume_launch(&self, session: &VendorSession) -> Option<ResumeLaunch> {
        Some(ResumeLaunch::of(session))
    }

    /// The CLI's built-in slash commands, each with what it maps to.
    fn builtin_commands(&self) -> &'static [BuiltinCommand] {
        &[]
    }

    /// Custom commands, skills, and plugin commands the CLI loads for `scan.folder`: the
    /// project's, the home folder's, and installed plugins'. Blocking file reads.
    fn discover_commands(&self, scan: &VendorScan) -> anyhow::Result<Vec<VendorCommand>> {
        let _ = scan;
        Ok(Vec::new())
    }

    /// The prompt that runs `command` in the main agent's session, such as `/review src`.
    /// `None` when it can't run there.
    fn command_prompt(&self, command: &VendorCommand, args: &str) -> Option<String> {
        matches!(command.kind, CommandKind::Vendor).then(|| command.invocation(args))
    }
}

/// How the harness worker talks to the agent's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// One event per stdout line, read by the adapter's parser.
    #[default]
    Lines,
    /// JSON-RPC on stdio through the Agent Client Protocol, run by a session driver.
    Acp,
}

/// Help for a harness that isn't ready, for the guided setup and the startup header.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SetupHints {
    pub install: Option<String>,
    pub login: Option<String>,
    /// Arguments that report login state without a model call (exit 0 when logged in), such
    /// as `["login", "status"]` for Codex. `None` when no check is verified for this CLI.
    pub login_check: Option<Vec<String>>,
}

/// Extra flags and environment for one launch setting, and how faithfully they apply.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LaunchTweak {
    pub args: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub effect: TweakEffect,
}

/// How faithfully a setting applies to a harness.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum TweakEffect {
    #[default]
    Exact,
    Approximated {
        note: String,
    },
    NotPossible {
        note: String,
    },
    /// The conductor counts events and interrupts past the cap (agy subagents).
    EnforcedByConductor,
}

/// What a CLI reports and enforces. The defaults describe a CLI that streams its usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capabilities {
    /// Usage arrives while the session runs, not only when its turn ends.
    pub live_usage: bool,
    pub reports_cost: bool,
    pub reports_model: bool,
    pub resume_totals_include_earlier: bool,
    pub enforces_read_only: bool,
    /// A message written to open input while a turn runs joins that turn, so it brings no
    /// result of its own. Otherwise it queues as the next turn.
    pub input_joins_running_turn: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            live_usage: true,
            reports_cost: false,
            reports_model: true,
            resume_totals_include_earlier: true,
            enforces_read_only: true,
            input_joins_running_turn: false,
        }
    }
}

/// A signal the conductor sends to the agent's process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopSignal {
    /// SIGINT, the polite interrupt most CLIs handle.
    Interrupt,
    Terminate,
    Kill,
}

/// Send `signal`, then wait up to `wait` for the process to exit before the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopStep {
    pub signal: StopSignal,
    pub wait: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopPlan {
    /// The conductor stops early once the process exits.
    pub steps: Vec<StopStep>,
}

impl StopPlan {
    pub fn escalating(after_interrupt: Duration, after_terminate: Duration) -> Self {
        Self {
            steps: vec![
                StopStep {
                    signal: StopSignal::Interrupt,
                    wait: after_interrupt,
                },
                StopStep {
                    signal: StopSignal::Terminate,
                    wait: after_terminate,
                },
                StopStep {
                    signal: StopSignal::Kill,
                    wait: Duration::from_secs(2),
                },
            ],
        }
    }
}

impl Default for StopPlan {
    fn default() -> Self {
        Self::escalating(Duration::from_secs(5), Duration::from_secs(5))
    }
}

/// The first `X.Y.Z` version number in `output`, with any `-pre` or `+build` suffix.
pub fn extract_version(output: &str) -> Option<String> {
    output
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | '"' | '\''))
        .find_map(version_in_token)
        .map(str::to_owned)
}

/// `token` without a leading `v` or trailing dots, if what remains is a version number.
fn version_in_token(token: &str) -> Option<&str> {
    let token = token.strip_prefix(['v', 'V']).unwrap_or(token);
    let token = token.trim_end_matches('.');
    let numbers_len = token
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(token.len());
    let (numbers, suffix) = token.split_at(numbers_len);
    let three_numbers = numbers.split('.').count() == 3 && !numbers.split('.').any(str::is_empty);
    let clean_suffix = suffix.is_empty() || suffix.starts_with(['-', '+']);
    (three_numbers && clean_suffix).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionRole;

    struct Fake;

    impl StreamParser for Fake {
        fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::unknown_line(line)]
        }
        fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
            Vec::new()
        }
        fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
            Vec::new()
        }
    }

    impl Adapter for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn display_name(&self) -> &'static str {
            "Fake"
        }
        fn binary(&self) -> &'static str {
            "fake"
        }
        fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
            let spec = CommandSpec::new(self.binary(), &req.workdir).arg(&req.prompt);
            Ok(match req.role {
                SessionRole::ReadOnly => spec.arg("--read-only"),
                SessionRole::Build => spec,
            })
        }
        fn parser(&self) -> Box<dyn StreamParser> {
            Box::new(Fake)
        }
        fn version_args(&self) -> &'static [&'static str] {
            &["--version"]
        }
        fn parse_version(&self, output: &str) -> Option<String> {
            extract_version(output)
        }
        fn tested_versions(&self) -> &'static [&'static str] {
            &["1.0.0"]
        }
        fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
            ("tiny", Some("low"))
        }
        fn stop_plan(&self) -> StopPlan {
            StopPlan::default()
        }
    }

    #[test]
    fn adapters_are_object_safe_and_default_to_no_stdin_steering() {
        let adapter: Box<dyn Adapter> = Box::new(Fake);
        assert!(adapter.wrap_up_input("wrap up").is_none());
        let caps = adapter.capabilities();
        assert!(caps.live_usage && caps.enforces_read_only && !caps.reports_cost);
        let req = SessionRequest {
            prompt: "hi".into(),
            ..SessionRequest::default()
        };
        let spec = adapter.command(&req).unwrap();
        assert_eq!(spec.args, ["hi", "--read-only"]);
        let mut parser = adapter.parser();
        assert_eq!(parser.parse_stdout_line("x").len(), 1);
    }

    #[test]
    fn new_adapter_methods_default_to_the_0_0_1_behavior() {
        let adapter: Box<dyn Adapter> = Box::new(Fake);
        let dir = Path::new("/w");
        assert!(adapter.input_line("hi").is_none());
        assert!(adapter.quota_probe(dir, None).is_none());
        assert!(adapter.parse_quota_probe("{}").is_none());
        assert!(adapter.interactive_resume("s1", dir).is_none());
        assert_eq!(adapter.default_family(), "unknown");
        assert_eq!(
            adapter.compaction(Compaction::Inherit),
            LaunchTweak::default()
        );
        assert!(matches!(
            adapter.compaction(Compaction::Off).effect,
            TweakEffect::NotPossible { .. }
        ));
        assert_eq!(
            adapter.native_fanout(NativeFanout::default()).effect,
            TweakEffect::EnforcedByConductor
        );
        assert!(adapter.user_hooks_off().args.is_empty());
        // No login state is claimed without a verified check.
        assert_eq!(adapter.setup_hints(), SetupHints::default());
        assert!(adapter.setup_hints().login_check.is_none());
    }

    #[test]
    fn a_vendor_without_sessions_or_commands_lists_none() {
        use crate::names::VendorId;
        use crate::vendor_commands::{CommandSource, VendorCommandSet};

        let adapter: Box<dyn Adapter> = Box::new(Fake);
        let scan = VendorScan::new("/w", "/home");
        assert!(adapter.vendor_sessions(&scan, 20).unwrap().is_empty());
        assert!(
            adapter
                .vendor_session_turns(&scan, "s1", 4)
                .unwrap()
                .is_empty()
        );
        assert!(adapter.builtin_commands().is_empty());
        assert!(adapter.discover_commands(&scan).unwrap().is_empty());
        let set = VendorCommandSet::of(adapter.as_ref(), &scan).unwrap();
        assert_eq!(set.vendor, VendorId("fake".into()));
        assert!(set.commands.is_empty());

        let session = VendorSession {
            vendor: VendorId("fake".into()),
            id: "s1".into(),
            title: "Hi".into(),
            prompt: None,
            folder: "/w".into(),
            updated: time::OffsetDateTime::UNIX_EPOCH,
            model: None,
            branch: None,
            last_message: None,
        };
        let launch = adapter.resume_launch(&session).unwrap();
        assert_eq!(
            (launch.session.as_str(), launch.workdir.as_path()),
            ("s1", dir())
        );

        let mut command = VendorCommand {
            name: "review".into(),
            aliases: Vec::new(),
            args: "[target]".into(),
            help: "Review".into(),
            kind: CommandKind::Vendor,
            source: CommandSource::BuiltIn,
        };
        assert_eq!(
            adapter.command_prompt(&command, "src").as_deref(),
            Some("/review src")
        );
        command.kind = CommandKind::ScreenOnly;
        assert_eq!(adapter.command_prompt(&command, ""), None);
    }

    fn dir() -> &'static Path {
        Path::new("/w")
    }

    #[test]
    fn tweak_effects_round_trip() {
        for effect in [
            TweakEffect::Exact,
            TweakEffect::Approximated { note: "90%".into() },
            TweakEffect::NotPossible { note: "no".into() },
            TweakEffect::EnforcedByConductor,
        ] {
            let tweak = LaunchTweak {
                args: vec!["--x".into()],
                env_set: vec![("A".into(), "1".into())],
                effect,
            };
            let json = serde_json::to_string(&tweak).unwrap();
            assert_eq!(serde_json::from_str::<LaunchTweak>(&json).unwrap(), tweak);
        }
    }

    #[test]
    fn default_stop_plan_escalates() {
        let plan = StopPlan::default();
        let signals: Vec<_> = plan.steps.iter().map(|s| s.signal).collect();
        assert_eq!(
            signals,
            [
                StopSignal::Interrupt,
                StopSignal::Terminate,
                StopSignal::Kill
            ]
        );
        assert_eq!(plan.steps[0].wait, Duration::from_secs(5));
    }

    #[test]
    fn extracts_versions_from_common_formats() {
        let cases = [
            ("2.1.284 (Claude Code)", Some("2.1.284")),
            ("codex-cli 0.158.0", Some("0.158.0")),
            ("agy version v1.2.12", Some("1.2.12")),
            ("tool 1.2.3-beta.1\n", Some("1.2.3-beta.1")),
            ("tool 1.2.3-dev", Some("1.2.3-dev")),
            ("\"V2.0.0\"", Some("2.0.0")),
            ("version 1.2.3.", Some("1.2.3")),
            ("build 1.2 and 20260929", None),
            ("1.2.3.4", None),
            ("", None),
        ];
        for (input, want) in cases {
            assert_eq!(extract_version(input).as_deref(), want, "{input:?}");
        }
    }

    #[test]
    fn known_ids_match_the_contract() {
        assert_eq!(KNOWN_AGENT_IDS, ["claude-code", "codex", "antigravity-cli"]);
    }
}
