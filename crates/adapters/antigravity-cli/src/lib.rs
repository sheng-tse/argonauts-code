//! agy (`agy`) adapter for Argonauts Code.

mod account;
mod launch;
mod parser;

use std::path::Path;

use argonauts_core::agents::adapter::IdentityMarks;
use argonauts_core::look::keywords::Keyword;
use argonauts_core::models::limits::{PlanUsageAnswer, PlanUsageReading};
use argonauts_core::settings::sessions::{Compaction, NativeFanout};
use argonauts_core::{
    Adapter, Capabilities, CommandSpec, LaunchTweak, SessionRequest, SetupHints, StopPlan,
    StreamParser, extract_version,
};

pub use launch::command::{
    API_KEY_ENV, AUTO_UPDATE_ENV, AUTO_UPDATE_VALUE, build_command, user_message_line,
};
pub use parser::AgyStreamParser;

pub const ADAPTER_ID: &str = "antigravity-cli";

/// agy 1.3.5 no longer knows the Gemini 3.6 levels its earlier releases ran.
pub const CHEAPEST_MODEL: &str = "gemini-3.8-flash-low";

pub const CHEAPEST_EFFORT: &str = "low";

/// The versions the recordings in `fixtures/` were made with or also match, oldest first,
/// read from the `.meta.json` beside each recording when the crate is built.
pub const TESTED_VERSIONS: &[&str] = include!(concat!(env!("OUT_DIR"), "/tested_versions.rs"));

/// Words agy treats specially in a prompt: none in 1.2.16. Its rainbow belongs to
/// the effort menu's max level, not to anything you type.
pub const KEYWORDS: &[Keyword] = &[];

/// From agy's install docs. It puts the binary at `~/.local/bin/agy`.
pub const INSTALL_COMMAND: &str = "curl -fsSL https://antigravity.google/cli/install.sh | bash";

/// agy asks you to sign in the first time you run it (its bundled docs, 1.2.13).
pub const LOGIN_COMMAND: &str = "agy";

#[derive(Debug, Clone, Copy, Default)]
pub struct AntigravityCli;

impl AntigravityCli {
    pub fn new() -> Self {
        Self
    }
}

/// The `init` event, with what the parser takes from it.
const STARTUP_MESSAGE: argonauts_core::agents::adapter::StartupMessage =
    argonauts_core::agents::adapter::StartupMessage {
        marker: r#""event":"init""#,
        is: &[("/event", "init")],
        fields: &[
            argonauts_core::agents::adapter::StartupField {
                pointer: "/conversation_id",
                what: "its session",
                without: "its conversations can't be resumed",
            },
            argonauts_core::agents::adapter::StartupField {
                pointer: "/init/model",
                what: "its model",
                without: "the model it really ran can't be told from the one asked for",
            },
        ],
    };

impl Adapter for AntigravityCli {
    fn first_count(&self) -> Option<argonauts_core::runs::events::context::FirstCount> {
        Some(argonauts_core::runs::events::context::FirstCount::Step)
    }

    fn id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn display_name(&self) -> &'static str {
        "agy"
    }

    fn binary(&self) -> &'static str {
        "agy"
    }

    fn holds_answer_format(&self) -> bool {
        true
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        build_command(self.binary(), req)
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(AgyStreamParser::new())
    }

    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    fn keywords(&self) -> &'static [Keyword] {
        KEYWORDS
    }

    fn tested_versions(&self) -> &'static [&'static str] {
        TESTED_VERSIONS
    }

    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        (CHEAPEST_MODEL, Some(CHEAPEST_EFFORT))
    }

    /// agy ends a headless turn on SIGINT with status `INTERRUPTED`, then stops its background
    /// tasks.
    fn stop_plan(&self) -> StopPlan {
        StopPlan::default()
    }

    fn capabilities(&self) -> Capabilities {
        // The read-only agent file narrows the tools, but agy doesn't enforce it the way a
        // sandbox would, and --sandbox still allows writes in the workdir.
        Capabilities {
            enforces_read_only: false,
            ..Capabilities::default()
        }
    }

    fn input_line(&self, message: &str) -> Option<String> {
        Some(user_message_line(message))
    }

    fn plan_usage_probe(&self, workdir: &Path, model: Option<&str>) -> Option<CommandSpec> {
        Some(account::probe::plan_usage_probe_command(
            self.binary(),
            workdir,
            model,
        ))
    }

    fn parse_plan_usage_probe(&self, stdout: &str) -> Option<PlanUsageReading> {
        account::probe::parse_usage(self.id(), stdout)
    }

    fn parse_plan_usage_probe_answer(&self, stdout: &str) -> Option<PlanUsageAnswer> {
        account::probe::usage_answer(self.id(), stdout)
    }

    fn compaction(&self, setting: Compaction) -> LaunchTweak {
        launch::tweaks::compaction(setting)
    }

    fn native_fanout(&self, setting: NativeFanout) -> LaunchTweak {
        launch::tweaks::native_fanout(setting)
    }

    fn user_hooks_off(&self) -> LaunchTweak {
        launch::tweaks::user_hooks_off()
    }

    fn interactive_resume(&self, req: &SessionRequest) -> Option<CommandSpec> {
        let session_id = req.resume_session.as_deref()?;
        launch::command::interactive_resume_command(self.binary(), session_id, &req.workdir)
    }

    /// The tools its stream names and its home folder.
    fn identity_marks(&self) -> IdentityMarks {
        IdentityMarks {
            tool_names: &[
                "run_command",
                "view_file",
                "find_by_name",
                "write_to_file",
                "write_file",
                "invoke_subagent",
                "finish",
            ],
            row_marks: &[],
            home_folders: &["~/.gemini"],
            citation: None,
        }
    }

    /// `agy --conversation <id>`, in the folder it ran in.
    fn resume_command(
        &self,
        req: &SessionRequest,
    ) -> Option<argonauts_core::agents::cli_sessions::ResumeCommand> {
        let spec = self.interactive_resume(req)?;
        Some(argonauts_core::agents::cli_sessions::ResumeCommand {
            words: std::iter::once(spec.program).chain(spec.args).collect(),
            local_model: false,
            from_any_folder: false,
            also: argonauts_core::agents::cli_sessions::resume_facts::AGY_LISTS_IT
                .then(|| "pick it in agy's own list".to_owned()),
        })
    }

    /// agy reads the project's AGENTS.md as its rules.
    fn instruction_files(&self, workdir: &Path, _isolated: bool) -> Vec<String> {
        argonauts_core::agents::adapter::agents_md(workdir)
    }

    /// agy also runs Claude and GPT-OSS models. The registry maps those model ids to their
    /// own provider.
    fn default_provider(&self) -> &'static str {
        "google"
    }

    /// agy has no command that reports login state without a model call.
    fn setup_hints(&self) -> SetupHints {
        SetupHints {
            install: Some(INSTALL_COMMAND.to_owned()),
            login: Some(LOGIN_COMMAND.to_owned()),
            login_check: None,
        }
    }

    fn update_args(&self) -> Option<&'static [&'static str]> {
        Some(&["update"])
    }

    fn reads_banner(&self) -> bool {
        true
    }

    fn banner_account(
        &self,
        screen: &str,
    ) -> Option<argonauts_core::agents::adapter::BannerAccount> {
        account::banner::account(screen)
    }

    fn updater_off_env(&self) -> &'static [(&'static str, &'static str)] {
        &[(AUTO_UPDATE_ENV, AUTO_UPDATE_VALUE)]
    }

    fn startup_message(&self) -> Option<&'static argonauts_core::agents::adapter::StartupMessage> {
        Some(&STARTUP_MESSAGE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::settings::sessions::FanoutSwitch;
    use argonauts_core::{AuthMode, KNOWN_AGENT_IDS, SessionRole, StopSignal, TweakEffect};
    use std::path::PathBuf;

    #[test]
    fn identity_matches_the_contract() {
        let adapter = AntigravityCli::new();
        assert_eq!(adapter.id(), "antigravity-cli");
        assert!(KNOWN_AGENT_IDS.contains(&adapter.id()));
        assert_eq!(adapter.binary(), "agy");
        assert_eq!(adapter.display_name(), "agy");
    }

    #[test]
    fn cheapest_model_is_flash_low() {
        assert_eq!(
            AntigravityCli.cheapest_model(),
            ("gemini-3.8-flash-low", Some("low"))
        );
    }

    #[test]
    fn version_comes_from_the_bare_number() {
        let adapter = AntigravityCli;
        assert_eq!(adapter.version_args(), ["--version"]);
        assert_eq!(adapter.parse_version("1.2.13\n").as_deref(), Some("1.2.13"));
        assert_eq!(adapter.parse_version("garbage"), None);
        assert_eq!(adapter.tested_versions(), TESTED_VERSIONS);
    }

    #[test]
    fn stop_plan_starts_with_an_interrupt() {
        let plan = AntigravityCli.stop_plan();
        assert_eq!(plan.steps[0].signal, StopSignal::Interrupt);
        assert_eq!(plan.steps.last().map(|s| s.signal), Some(StopSignal::Kill));
    }

    #[test]
    fn wrap_up_falls_back_to_an_interrupt() {
        assert!(AntigravityCli.wrap_up_input("wrap up").is_none());
    }

    #[test]
    fn read_only_is_not_enforced() {
        let caps = AntigravityCli.capabilities();
        assert!(!caps.enforces_read_only);
        assert!(caps.live_usage && caps.reports_model);
    }

    #[test]
    fn no_adapter_id_says_gemini() {
        // Google's CLI is agy, and no id names the one before it.
        let adapter = AntigravityCli;
        for id in [adapter.id(), adapter.binary(), adapter.display_name()] {
            let lower = id.to_ascii_lowercase();
            assert!(!lower.contains("gemini"), "{id}");
        }
        assert!(KNOWN_AGENT_IDS.iter().all(|id| !id.contains("gemini")));
        assert_ne!(adapter.binary(), "gemini");
    }

    #[test]
    fn steering_goes_over_stdin_as_a_user_message() {
        let line = AntigravityCli.input_line("wrap up now").unwrap();
        assert_eq!(
            line,
            "{\"event\":\"user\",\"message\":{\"content\":\"wrap up now\"}}\n"
        );
    }

    #[test]
    fn compaction_flags_per_cli() {
        let adapter = AntigravityCli;
        assert_eq!(
            adapter.compaction(Compaction::Inherit),
            LaunchTweak::default()
        );
        for setting in [Compaction::Auto, Compaction::Off] {
            let tweak = adapter.compaction(setting);
            assert!(
                tweak.args.is_empty() && tweak.env_set.is_empty(),
                "{setting:?}"
            );
            assert!(
                matches!(tweak.effect, TweakEffect::NotPossible { .. }),
                "{setting:?}"
            );
        }
        let req = SessionRequest {
            prompt: "hi".into(),
            role: SessionRole::Build,
            compaction: Some(Compaction::Off),
            ..SessionRequest::default()
        };
        let spec = adapter.command(&req).unwrap();
        assert!(!spec.args.iter().any(|a| a.contains("compact")));
        assert!(!spec.env_set.iter().any(|(k, _)| k.contains("COMPACT")));
    }

    #[test]
    fn native_fanout_flags_per_cli() {
        let adapter = AntigravityCli;
        assert_eq!(
            adapter.native_fanout(NativeFanout::Limit { max: 3 }).effect,
            TweakEffect::EnforcedByConductor
        );
        assert_eq!(
            adapter
                .native_fanout(NativeFanout::Switch(FanoutSwitch::Off))
                .effect,
            TweakEffect::Exact
        );
        assert_eq!(
            adapter
                .native_fanout(NativeFanout::Switch(FanoutSwitch::On))
                .effect,
            TweakEffect::Exact
        );

        let request = |fanout| SessionRequest {
            prompt: "hi".into(),
            role: SessionRole::Build,
            native_fanout: Some(fanout),
            scratch_dir: Some(PathBuf::from("/state/s1")),
            ..SessionRequest::default()
        };
        let off = adapter
            .command(&request(NativeFanout::Switch(FanoutSwitch::Off)))
            .unwrap();
        let at = off.args.iter().position(|a| a == "--agent").unwrap();
        assert_eq!(off.args[at + 1], "argonauts-no-subagents");
        assert!(!off.files[0].contents.contains("invoke_subagent"));

        let capped = adapter
            .command(&request(NativeFanout::Limit { max: 3 }))
            .unwrap();
        assert!(!capped.args.iter().any(|a| a == "--agent"));
        assert!(capped.files.is_empty());
    }

    #[test]
    fn agy_loads_agents_md_itself() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            AntigravityCli
                .instruction_files(dir.path(), false)
                .is_empty()
        );
        std::fs::write(dir.path().join("AGENTS.md"), "x").unwrap();
        assert_eq!(
            AntigravityCli.instruction_files(dir.path(), false),
            ["AGENTS.md"]
        );
    }

    #[test]
    fn user_hooks_cannot_be_turned_off_per_run() {
        let tweak = AntigravityCli.user_hooks_off();
        assert!(tweak.args.is_empty());
        assert!(matches!(tweak.effect, TweakEffect::NotPossible { .. }));
    }

    #[test]
    fn take_over_resumes_the_conversation_in_the_worktree() {
        let spec = AntigravityCli
            .interactive_resume(&SessionRequest::reopening(
                "055a398f",
                Path::new("/work/task-001"),
            ))
            .unwrap();
        assert_eq!(spec.program, "agy");
        assert_eq!(spec.args, ["--conversation", "055a398f"]);
        assert_eq!(spec.cwd, PathBuf::from("/work/task-001"));
        let typed = AntigravityCli
            .resume_command(&SessionRequest::reopening(
                "055a398f",
                Path::new("/work/task-001"),
            ))
            .unwrap();
        assert_eq!(typed.words, ["agy", "--conversation", "055a398f"]);
        assert!(!typed.from_any_folder);
    }

    #[test]
    fn provider_setup_and_probe() {
        let adapter = AntigravityCli;
        assert_eq!(adapter.default_provider(), "google");
        let hints = adapter.setup_hints();
        assert!(
            hints
                .install
                .unwrap()
                .contains("antigravity.google/cli/install.sh")
        );
        assert_eq!(hints.login.as_deref(), Some("agy"));
        assert_eq!(hints.login_check, None);
        let probe = adapter.plan_usage_probe(Path::new("/w"), None).unwrap();
        assert_eq!(probe.args, ["-p", "/usage", "--output-format", "json"]);
        assert!(adapter.parse_plan_usage_probe("not json").is_none());
    }

    #[test]
    fn command_goes_through_the_adapter() {
        let req = SessionRequest {
            prompt: "hi".into(),
            role: SessionRole::Build,
            ..SessionRequest::default()
        };
        let spec = AntigravityCli.command(&req).unwrap();
        assert_eq!(spec.program, "agy");
        let mut parser = AntigravityCli.parser();
        assert_eq!(parser.parse_stdout_line("nope").len(), 1);
    }

    /// agy is a Go binary whose HTTP client takes its proxy from `HTTPS_PROXY` and `NO_PROXY`
    /// (`httpproxy.FromEnvironment`), so no auth mode drops them.
    #[test]
    fn proxy_variables_stay() {
        for auth in [AuthMode::Subscription, AuthMode::ApiKey] {
            let req = SessionRequest {
                prompt: "hi".into(),
                role: SessionRole::Build,
                auth,
                ..SessionRequest::default()
            };
            let spec = AntigravityCli.command(&req).unwrap();
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

/// Every tool its recorded output names is one of its marks, so a relayed message never names
/// one in its own words.
#[cfg(test)]
mod identity_tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use argonauts_core::Adapter;

    use super::*;

    const AFTER: &str = r#""tool_name""#;
    const THEN: &str = r#":""#;

    /// The names that follow `after` in every recorded fixture, up to the next quote.
    fn recorded(after: &str, then: &str) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        let mut dirs = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for piece in text.split(after).skip(1) {
                    let Some(at) = piece.find(then).filter(|at| *at < 160) else {
                        continue;
                    };
                    if let Some(name) = piece[at + then.len()..].split('"').next() {
                        names.insert(name.to_owned());
                    }
                }
            }
        }
        names
    }

    #[test]
    fn every_tool_its_output_names_is_one_of_its_marks() {
        let marks = AntigravityCli::new().identity_marks();
        let recorded = recorded(AFTER, THEN);
        assert!(!recorded.is_empty(), "the fixtures name tools");
        let unmarked: Vec<&String> = recorded
            .iter()
            .filter(|name| !name.starts_with("mcp__") && !marks.tool_names.contains(&name.as_str()))
            .collect();
        assert!(unmarked.is_empty(), "{unmarked:?}");
        assert!(!marks.home_folders.is_empty());
    }
}
