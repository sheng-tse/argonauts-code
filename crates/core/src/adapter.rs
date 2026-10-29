//! The traits each agent adapter implements.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cli_commands::{BuiltinCommand, CliCommand, CommandKind};
use crate::cli_sessions::{CliScan, CliSession, CliTurn, ResumeLaunch};
use crate::driver::SessionDriver;
use crate::event::AgentEvent;
use crate::events::context::{ContextDetail, FirstCount};
use crate::keywords::Keyword;
use crate::limits::{PlanUsageAnswer, PlanUsageReading};
use crate::screen::ScreenRequest;
use crate::session::{CommandSpec, SessionRequest};
use crate::settings::sessions::{Compaction, NativeFanout};

pub const KNOWN_AGENT_IDS: &[&str] = &["claude-code", "codex", "antigravity-cli"];

/// Parsers never fail. A line they don't understand becomes [`AgentEvent::Unknown`].
/// Lines arrive without their trailing newline.
pub trait StreamParser: Send {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent>;
    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent>;
    /// Called once after the process exits, to flush state or report a missing result.
    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent>;
    /// The stdin line that stops one of the session's own subagents, by the id its
    /// `Subagent` updates carry. `None` where the CLI has no way to stop one alone, or the
    /// subagent isn't one it can stop yet. The parser answers because it's what has seen the
    /// ids the CLI knows its subagents by.
    fn stop_subagent_line(&self, id: &str) -> Option<String> {
        let _ = id;
        None
    }
    /// The stdin line that stops the running turn alone, so what the CLI runs in the
    /// background, its own subagents and workflows, goes on. `None` where the CLI has no such
    /// request or this session couldn't spare them, and the whole session stops instead. The
    /// parser answers because it's what has seen how the session opened.
    fn stop_turn_line(&self) -> Option<String> {
        None
    }
    /// What the session's requests since the CLI last reported its own totals cost, priced
    /// the way its provider bills them, such as a cache write by how long it keeps and a long
    /// prompt at its own price. `None` leaves the estimate to the meter's price. The parser
    /// answers because it's what has seen each request whole.
    fn billed_since_totals(&self) -> Option<f64> {
        None
    }
    /// What this process has spent at least, by the CLI's own word, where totals may never
    /// follow: Claude Code says so when its spend cap stops the session's background agents
    /// between turns. It leaves out what an earlier process of the CLI session spent. `None`
    /// when the CLI said nothing of the kind.
    fn spent_at_least(&self) -> Option<f64> {
        None
    }
    /// A command to run once the process has ended and its output is read, before
    /// [`StreamParser::finish`], whose output [`StreamParser::read_after_end`] reads: OpenCode's
    /// export of a session whose last step its stream never printed. It gets the session's
    /// own `command`, so it runs the same program with the same environment. `None`, the
    /// default, when there's nothing to fetch.
    fn after_end(&mut self, command: &CommandSpec) -> Option<CommandSpec> {
        let _ = command;
        None
    }
    /// What [`StreamParser::after_end`]'s command printed, or `None` when it failed or took
    /// too long.
    fn read_after_end(&mut self, stdout: Option<&str>) -> Vec<AgentEvent> {
        let _ = stdout;
        Vec::new()
    }
}

/// One CLI, described well enough to run it headlessly.
pub trait Adapter: Send + Sync {
    /// Stable id, such as `claude-code`.
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn binary(&self) -> &'static str;
    /// Pure, so it can be unit tested.
    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec>;
    fn parser(&self) -> Box<dyn StreamParser>;
    /// The parser for one session. An isolated session's also reports what the CLI loaded that
    /// isolation keeps out.
    fn session_parser(&self, req: &SessionRequest) -> Box<dyn StreamParser> {
        let _ = req;
        self.parser()
    }
    fn version_args(&self) -> &'static [&'static str];
    fn parse_version(&self, output: &str) -> Option<String>;
    /// Whether `command` needs the installed version, so the runner reads it once before the
    /// first session and passes it in [`SessionRequest::cli_version`]. Every other CLI is never
    /// run an extra time.
    fn launch_reads_version(&self) -> bool {
        false
    }
    /// Whether `req`'s session talks to a server of its CLI's own, on a loopback port the CLI
    /// picks as it starts, as OpenCode 2's private server is. A session with the network off
    /// then reaches loopback straight, not only the proxy.
    fn talks_to_own_server(&self, req: &SessionRequest) -> bool {
        let _ = req;
        false
    }
    /// A command that has the CLI say what its own config holds in `req.workdir`, asked once
    /// before a session's first launch, in the session's sandbox, its output passed in
    /// [`SessionRequest::cli_config`]. `None` when `command` needs nothing of it.
    fn config_query(&self, req: &SessionRequest) -> Option<CommandSpec> {
        let _ = req;
        None
    }
    /// CLI versions whose recorded output the tests replay.
    fn tested_versions(&self) -> &'static [&'static str];
    /// Model and effort for live tests and recordings, and for cheap work while the CLI's own
    /// list isn't known; once it is, [`crate::cheapest::cheapest_listed`] decides. A test keeps
    /// this the cheapest its recorded list offers.
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>);
    fn stop_plan(&self) -> StopPlan;

    /// Stdin text that delivers `message` mid-run. `None` means Argonauts Code interrupts instead.
    fn wrap_up_input(&self, message: &str) -> Option<String> {
        let _ = message;
        None
    }

    /// The CLI's own ultra mode, if it has one. Models on a server and CLIs without one keep the
    /// mascot's normal look at any effort.
    fn ultra_mode(&self) -> Option<UltraMode> {
        None
    }

    /// Its CLI holds a session to the answer format it's given, as Claude Code's
    /// `--json-schema` does, so the report comes as the answer and no other way. A CLI told the
    /// format only in its prompt can't, and its model may hand the report in through a tool.
    fn holds_answer_format(&self) -> bool {
        false
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    /// How often the CLI writes while a tool call runs, so a longer silence then is a stall.
    /// `None` for a CLI that says nothing while it works, where silence is normal.
    fn heartbeat(&self) -> Option<Duration> {
        None
    }

    /// Like `wrap_up_input`, for sessions opened with `keep_input_open`. `None` means
    /// interrupt and resume.
    fn input_line(&self, message: &str) -> Option<String> {
        let _ = message;
        None
    }

    /// Like `input_line`, with the id the CLI reports the message's progress by. A CLI that
    /// takes no id gets the plain line.
    fn message_line(&self, message: &str, uuid: &str) -> Option<String> {
        let _ = uuid;
        self.input_line(message)
    }

    /// Like `message_line`, for a message that holds pictures. A CLI that can't take one on
    /// its open input gets each picture's path in the text.
    fn picture_line(
        &self,
        message: &str,
        pictures: &[crate::pictures::Picture],
        uuid: &str,
    ) -> Option<String> {
        let pictures: Vec<_> = pictures.iter().collect();
        self.message_line(&crate::pictures::with_paths(message, &pictures), uuid)
    }

    /// The line that hands what waits to the model now. `None` where the CLI can't.
    fn send_now_line(&self, uuid: &str) -> Option<String> {
        let _ = uuid;
        None
    }

    /// The line that takes back a message the model hasn't read. `None` where the CLI can't.
    fn cancel_line(&self, uuid: &str) -> Option<String> {
        let _ = uuid;
        None
    }

    /// The line for a screen's request. `None` where no screen can join.
    fn screen_line(&self, request: &ScreenRequest) -> Option<String> {
        let _ = request;
        None
    }

    /// The line that asks for the context by category. `None` where the CLI can't say.
    fn context_usage_line(&self, detail: ContextDetail) -> Option<String> {
        let _ = detail;
        None
    }

    /// The context fill this CLI's stream gives is worked out from the session's totals,
    /// because it never says how large the last request was. A screen marks it with `~`.
    fn estimates_context_fill(&self) -> bool {
        false
    }

    /// When the CLI first counts a fresh session itself: after its first answer, turn or step.
    /// `None` for a CLI whose count was never recorded.
    fn first_count(&self) -> Option<FirstCount> {
        None
    }

    /// A command that reads plan usage without spending any, such as `agy -p "/usage"`. `model` is
    /// the one the agent's sessions use, for a CLI that names a model on every call.
    fn plan_usage_probe(&self, workdir: &Path, model: Option<&str>) -> Option<CommandSpec> {
        let _ = (workdir, model);
        None
    }

    fn parse_plan_usage_probe(&self, stdout: &str) -> Option<PlanUsageReading> {
        let _ = stdout;
        None
    }

    /// The answer from what the probe printed before it ended or ran out of time. By default
    /// the same as [`Adapter::parse_plan_usage_probe_answer`].
    fn parse_plan_usage_probe_end(&self, stdout: &str) -> Option<PlanUsageAnswer> {
        self.parse_plan_usage_probe_answer(stdout)
    }

    /// A first part of the answer worth showing while the rest is on its way, such as Claude
    /// Code's windows before its slower scan. It must name the same account the whole answer
    /// will, or the two read as a change of account. `None` by default.
    fn parse_plan_usage_probe_early(&self, stdout: &str) -> Option<PlanUsageAnswer> {
        let _ = stdout;
        None
    }

    /// The session's own stream says when plan usage changes, so its probe is only for when
    /// nothing runs, and isn't repeated while a session does.
    fn stream_reports_plan_usage(&self) -> bool {
        false
    }

    /// Every window the probe's answer reports, one reading each, and the plan when it names
    /// one. By default, the one reading `parse_plan_usage_probe` gives.
    fn parse_plan_usage_probe_answer(&self, stdout: &str) -> Option<PlanUsageAnswer> {
        let reading = self.parse_plan_usage_probe(stdout)?;
        Some(PlanUsageAnswer {
            windows: vec![reading],
            plan: None,
            live: None,
            failed: None,
        })
    }

    fn compaction(&self, setting: Compaction) -> LaunchTweak {
        match setting {
            Compaction::Inherit => LaunchTweak::default(),
            _ => LaunchTweak {
                effect: TweakEffect::NotPossible {
                    note: "this CLI has no per-launch compaction control".to_owned(),
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
                note: "this CLI can't skip user hooks per launch".to_owned(),
            },
            ..LaunchTweak::default()
        }
    }

    /// The agent's interactive UI on `req.resume_session` in `req.workdir`, for take-over:
    /// `claude --resume <id>`, `codex resume <id>`, `agy --conversation <id>`,
    /// `opencode --session <id>`. `req` also has the session's model and launch hints, for a
    /// CLI that must be given them again, as Codex's local mode must.
    fn interactive_resume(&self, req: &SessionRequest) -> Option<CommandSpec> {
        let _ = req;
        None
    }

    /// What its own screen on a council seat's session adds, so the seat goes on as a run's
    /// session does: nothing kept in the CLI's memory, no attribution lines in its commits.
    /// Empty for a CLI that has neither or no flag for them.
    fn seat_screen_args(&self) -> Vec<String> {
        Vec::new()
    }

    /// What you type to carry `req.resume_session` on in the CLI's own screen, as the rows
    /// that say how print it: `claude --resume <id>`, `codex resume <id>`. `None` for a CLI
    /// that can't reopen one there.
    fn resume_command(&self, req: &SessionRequest) -> Option<crate::cli_sessions::ResumeCommand> {
        let _ = req;
        None
    }

    /// The model provider when the registry has no entry, such as `anthropic` or `openai`.
    fn default_provider(&self) -> &'static str {
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

    /// Arguments that run the CLI's own updater, such as `["update"]` for `claude update`.
    /// `None` when Argonauts Code knows of none, so `/update` leaves the CLI as it is.
    fn update_args(&self) -> Option<&'static [&'static str]> {
        None
    }

    /// Commands that say what the CLI can use, for a CLI that keeps a list of providers in
    /// place of one account, as OpenCode does. Each is run with no model call, and what it
    /// prints goes to [`providers`](Adapter::providers) in the same order.
    fn provider_checks(&self) -> &'static [&'static [&'static str]] {
        &[]
    }

    /// The providers and the default model, from what the
    /// [`provider_checks`](Adapter::provider_checks) printed. `None` when they don't say.
    fn providers(&self, _outputs: &[String]) -> Option<CliProviders> {
        None
    }

    /// The message the CLI sends as a session starts, with the fields of it Argonauts Code
    /// reads, so a version that stops sending one is noticed. `None` for a CLI that sends none.
    fn startup_message(&self) -> Option<&'static StartupMessage> {
        None
    }

    /// Where the CLI can be installed from. The default knows none, so its newest version is
    /// never asked for.
    fn install_sources(&self) -> InstallSources {
        InstallSources::default()
    }

    /// Versions known not to work with Argonauts Code, which need an update whatever else is
    /// true of them.
    fn broken_versions(&self) -> &'static [&'static str] {
        &[]
    }

    /// The CLI names who is signed in only on the screen it draws as it starts, so Argonauts Code
    /// may start it in a hidden terminal, send it nothing, and read that screen once.
    fn reads_banner(&self) -> bool {
        false
    }

    /// What the start-up screen `screen` says of who is signed in, for a CLI that
    /// [`reads_banner`](Adapter::reads_banner). `None` while it says nothing yet, as while the
    /// CLI still loads a saved login.
    fn banner_account(&self, _screen: &str) -> Option<BannerAccount> {
        None
    }

    /// The variables that keep the CLI from updating itself, such as
    /// `DISABLE_AUTOUPDATER=1` for Claude Code, so nothing Argonauts Code starts switches
    /// versions halfway. Empty for a CLI that never updates itself.
    fn updater_off_env(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    fn transport(&self) -> Transport {
        Transport::Lines
    }

    /// The driver for a two-way protocol, which gets the child's pipes instead of the parser.
    /// The CLI runner still owns the process.
    fn session_driver(&self) -> Option<Arc<dyn SessionDriver>> {
        None
    }

    /// Whether the CLI keeps a list of its sessions that [`Adapter::cli_sessions`] reads. One
    /// that keeps none, as agy, carries a session on only by an id you give it.
    fn lists_sessions(&self) -> bool {
        false
    }

    /// The CLI's own sessions that ran in `scan.folder`, newest first, at most `limit`. Reads
    /// only the CLI's session list and transcripts, never its credentials. Blocking file reads.
    fn cli_sessions(&self, scan: &CliScan, limit: usize) -> anyhow::Result<Vec<CliSession>> {
        let _ = (scan, limit);
        Ok(Vec::new())
    }

    /// The last `count` turns of one of those sessions, oldest first, to reprint when it's
    /// resumed.
    fn cli_session_turns(
        &self,
        scan: &CliScan,
        session: &str,
        count: usize,
    ) -> anyhow::Result<Vec<CliTurn>> {
        let _ = (scan, session, count);
        Ok(Vec::new())
    }

    /// How the conversation's next turn carries on `session` through the CLI's own resume. The
    /// default suits a CLI whose `command` resumes [`SessionRequest::resume_session`].
    fn resume_launch(&self, session: &CliSession) -> Option<ResumeLaunch> {
        Some(ResumeLaunch::of(session))
    }

    /// The CLI's built-in slash commands, each with what it maps to.
    fn builtin_commands(&self) -> &'static [BuiltinCommand] {
        &[]
    }

    /// Words the CLI treats specially in a prompt, which the input draws as the CLI does.
    fn keywords(&self) -> &'static [Keyword] {
        &[]
    }

    /// Custom commands, skills, and plugin commands the CLI loads for `scan.folder`: the
    /// project's, the home folder's, and installed plugins'. Blocking file reads.
    fn discover_commands(&self, scan: &CliScan) -> anyhow::Result<Vec<CliCommand>> {
        let _ = scan;
        Ok(Vec::new())
    }

    /// The prompt that runs `command` in the main agent's session, such as `/review src`.
    /// `None` when it can't run there.
    fn command_prompt(&self, command: &CliCommand, args: &str) -> Option<String> {
        matches!(command.kind, CommandKind::Cli).then(|| command.invocation(args))
    }

    /// The project's instruction files, such as `AGENTS.md`, that the CLI loads by itself in
    /// `workdir`, so a prompt needn't name them. None by default, so a prompt names them all.
    fn instruction_files(&self, workdir: &Path, isolated: bool) -> Vec<String> {
        let _ = (workdir, isolated);
        Vec::new()
    }

    /// What gives the CLI away in what its agent writes, which a council strips before another
    /// seat reads it. The default knows nothing, so nothing is stripped for this CLI.
    fn identity_marks(&self) -> IdentityMarks {
        IdentityMarks::default()
    }
}

/// The traces a CLI leaves in its agent's words: its tool names, the marks its output rows draw,
/// its home folders and the way it cites a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IdentityMarks {
    /// Such as `Bash`, `TodoWrite` or `apply_patch`, which a relayed message names in plain words.
    pub tool_names: &'static [&'static str],
    /// Such as `⏺`, `⎿` or `• Ran`, at the start of a row.
    pub row_marks: &'static [&'static str],
    /// Such as `~/.claude`, written with `~` for the home folder.
    pub home_folders: &'static [&'static str],
    pub citation: Option<CitationForm>,
}

impl IdentityMarks {
    pub fn is_empty(&self) -> bool {
        self.tool_names.is_empty()
            && self.row_marks.is_empty()
            && self.home_folders.is_empty()
            && self.citation.is_none()
    }
}

/// How a CLI cites a file and its lines, as `【F:src/lib.rs†L3-L9】` is `open` `【F:`, `lines`
/// `†L` and `close` `】`. A relayed message rewrites one as `src/lib.rs:3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CitationForm {
    pub open: &'static str,
    pub lines: &'static str,
    pub close: &'static str,
}

/// How the CLI runner talks to the agent's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// One event per stdout line, read by the adapter's parser.
    #[default]
    Lines,
    /// JSON-RPC on stdio through the Agent Client Protocol, run by a session driver.
    Acp,
}

/// Help for a CLI that isn't ready, for the guided setup and the startup header.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SetupHints {
    pub install: Option<String>,
    pub login: Option<String>,
    /// Arguments that report login state without a model call (exit 0 when logged in), such
    /// as `["login", "status"]` for Codex. `None` when no check is verified for this CLI.
    pub login_check: Option<Vec<String>>,
}

/// What a CLI that keeps its own list of providers can use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliProviders {
    pub providers: Vec<crate::status_tab::CliProvider>,
    /// The model it uses when none is named, as it routes it, such as `openrouter/z-ai/glm-5`,
    /// and the name of that model's provider.
    pub default_model: Option<(String, String)>,
}

/// The message a CLI sends as a session starts, as one line of JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupMessage {
    /// Text every such line holds, so any other line is passed over without being parsed.
    pub marker: &'static str,
    /// What makes a line this message: each JSON pointer with the text it holds.
    pub is: &'static [(&'static str, &'static str)],
    pub fields: &'static [StartupField],
}

/// One field of a start-up message that Argonauts Code reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupField {
    /// Where it is in the message, as a JSON pointer such as `/init/model`.
    pub pointer: &'static str,
    /// What it is, as said in `no longer reports its model`.
    pub what: &'static str,
    /// What is lost without it, as said after `so`.
    pub without: &'static str,
}

/// Where a CLI can be installed from. Its newest version is asked of the place the installed
/// one came from, and the update command is that place's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InstallSources {
    /// Its Homebrew formula or cask, such as `codex`.
    pub homebrew: Option<&'static str>,
    /// Its npm package, such as `@openai/codex`.
    pub npm: Option<&'static str>,
    pub own: Option<OwnInstaller>,
}

/// A CLI's own installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnInstaller {
    /// A part of the path it keeps the program under, such as `/claude/versions/`.
    pub path_part: &'static str,
    /// The address whose answer is the newest version's number, where it has one.
    pub newest_url: Option<&'static str>,
}

/// What a CLI's start-up screen says of who is signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerAccount {
    /// Its banner names the account.
    SignedIn {
        account: String,
        /// The plan as the CLI names it, such as `Google AI Pro`.
        plan: Option<String>,
    },
    /// It asks how to sign in, which it does only when no one is.
    SignedOut,
    /// It asks whether to trust the folder before it shows anything else. Only you answer
    /// that.
    UntrustedFolder,
}

/// Extra flags and environment for one launch setting, and how faithfully they apply.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LaunchTweak {
    pub args: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub effect: TweakEffect,
}

/// How faithfully a setting applies to a CLI.
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
    /// Argonauts Code counts events and interrupts past the cap (agy subagents).
    EnforcedByConductor,
}

/// A CLI's own ultra mode, which the welcome mascot shows in the CLI's colour while it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UltraMode {
    pub on: UltraOn,
    pub colour: crate::app::UltraColour,
}

/// When a CLI's ultra mode is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UltraOn {
    /// While `ultracode` is on, as Claude Code runs its own.
    Ultracode,
    /// While the main session runs at this effort, as Codex's `ultra`.
    Effort(&'static str),
}

impl UltraMode {
    /// The colour to show, when this mode is on for a session with `ultracode` and `effort`.
    pub fn colour_when(
        &self,
        ultracode: bool,
        effort: Option<&str>,
    ) -> Option<crate::app::UltraColour> {
        let on = match self.on {
            UltraOn::Ultracode => ultracode,
            UltraOn::Effort(level) => {
                effort.is_some_and(|effort| effort.eq_ignore_ascii_case(level))
            }
        };
        on.then_some(self.colour)
    }
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
    /// A message written mid-turn carries an id, by which it can go to the model at once and
    /// be taken back until the model reads it (Claude Code's send now).
    #[serde(default)]
    pub send_now: bool,
    /// A screen can join the session and draw what the CLI's mods draw (Claude Code's
    /// `ui_attach`).
    #[serde(default)]
    pub screen: bool,
    /// An isolated session keeps out your MCP servers, plugins, hooks, and synced extras, and
    /// its parser reports any that load anyway. A blind role may go only to such a CLI.
    #[serde(default)]
    pub isolates: bool,
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
            send_now: false,
            screen: false,
            isolates: false,
        }
    }
}

/// A signal Argonauts Code sends to the agent's process group.
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
    /// Argonauts Code stops early once the process exits.
    pub steps: Vec<StopStep>,
}

impl StopPlan {
    pub fn stepped(after_interrupt: Duration, after_terminate: Duration) -> Self {
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
        Self::stepped(Duration::from_secs(5), Duration::from_secs(5))
    }
}

/// `AGENTS.md` when `workdir` has one, for the
/// [`instruction_files`](Adapter::instruction_files) of a CLI that reads the project's
/// AGENTS.md by itself.
pub fn agents_md(workdir: &Path) -> Vec<String> {
    const NAME: &str = "AGENTS.md";
    if workdir.join(NAME).is_file() {
        vec![NAME.to_owned()]
    } else {
        Vec::new()
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
                SessionRole::Build | SessionRole::Direct => spec,
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
        let mut parser = adapter.session_parser(&req);
        assert_eq!(parser.parse_stdout_line("x").len(), 1);
    }

    #[test]
    fn new_adapter_methods_default_to_the_0_0_1_behavior() {
        let adapter: Box<dyn Adapter> = Box::new(Fake);
        let dir = Path::new("/w");
        assert!(adapter.input_line("hi").is_none());
        assert!(adapter.plan_usage_probe(dir, None).is_none());
        assert!(adapter.parse_plan_usage_probe("{}").is_none());
        assert!(
            adapter
                .interactive_resume(&SessionRequest::reopening("s1", dir))
                .is_none()
        );
        assert_eq!(adapter.default_provider(), "unknown");
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
    fn an_agent_by_default_leaves_no_marks_to_strip() {
        let adapter: Box<dyn Adapter> = Box::new(Fake);
        assert!(adapter.identity_marks().is_empty());
        let codex_like = IdentityMarks {
            citation: Some(CitationForm {
                open: "【F:",
                lines: "†L",
                close: "】",
            }),
            ..IdentityMarks::default()
        };
        assert!(!codex_like.is_empty());
    }

    #[test]
    fn an_agent_without_sessions_or_commands_lists_none() {
        use crate::cli_commands::{CliCommandSet, CommandSource};
        use crate::names::AgentId;

        let adapter: Box<dyn Adapter> = Box::new(Fake);
        let scan = CliScan::new("/w", "/home");
        assert!(adapter.cli_sessions(&scan, 20).unwrap().is_empty());
        assert!(
            adapter
                .cli_session_turns(&scan, "s1", 4)
                .unwrap()
                .is_empty()
        );
        assert!(adapter.builtin_commands().is_empty());
        assert!(adapter.discover_commands(&scan).unwrap().is_empty());
        let set = CliCommandSet::of(adapter.as_ref(), &scan).unwrap();
        assert_eq!(set.agent_id, AgentId("fake".into()));
        assert!(set.commands.is_empty());
        assert!(adapter.keywords().is_empty());
        assert!(set.keywords.is_empty());

        let session = CliSession {
            agent_id: AgentId("fake".into()),
            id: "s1".into(),
            title: "Hi".into(),
            prompt: None,
            folder: "/w".into(),
            updated: time::OffsetDateTime::UNIX_EPOCH,
            model: None,
            branch: None,
            last_message: None,
            turns: 0,
        };
        let launch = adapter.resume_launch(&session).unwrap();
        assert_eq!(
            (launch.session.as_str(), launch.workdir.as_path()),
            ("s1", dir())
        );

        let mut command = CliCommand {
            name: "review".into(),
            aliases: Vec::new(),
            args: "[target]".into(),
            help: "Review".into(),
            kind: CommandKind::Cli,
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
    fn default_stop_plan_steps_up() {
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
