//! The fake agent's scenario file and invocation log. A scenario is TOML: rules matched in
//! order against each invocation, each with a script. `[[acp]]` rules script ACP sessions
//! instead (see [`crate::fake_clis::acp`]).
//!
//! ```toml
//! [version]
//! claude = "2.1.284 (Claude Code)"
//!
//! [[rule]]
//! argv0 = "claude"
//! args_contain = ["-p"]
//! prompt_contains = "hi"
//! [[rule.step]]
//! emit_file = "../../../adapters/claude-code/fixtures/conversation.stdout.jsonl"
//! [[rule.step]]
//! exit = 0
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fake_clis::acp::AcpRule;

pub const SCENARIO_ENV: &str = "ARGONAUTS_FAKE_SCENARIO";

/// Names the JSONL log every invocation appends to.
pub const LOG_ENV: &str = "ARGONAUTS_FAKE_LOG";

/// Exit code when no rule matched, so a test sees the mismatch at once.
pub const NO_RULE_EXIT: i32 = 97;

pub const BAD_SCENARIO_EXIT: i32 = 98;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FakeScenario {
    /// `--version` output per program name, used when no rule matches `--version`.
    #[serde(default)]
    pub version: BTreeMap<String, String>,
    /// First match wins.
    #[serde(default, rename = "rule")]
    pub rules: Vec<FakeRule>,
    /// For an invocation whose first argument is `acp`. First match wins.
    #[serde(default)]
    pub acp: Vec<AcpRule>,
}

impl FakeScenario {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading the fake scenario {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(self).context("writing the scenario as TOML")?;
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    pub fn rule(mut self, rule: FakeRule) -> Self {
        self.rules.push(rule);
        self
    }

    pub fn version(mut self, program: &str, output: &str) -> Self {
        self.version.insert(program.to_owned(), output.to_owned());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FakeRule {
    /// Such as `codex`. Any program when unset.
    #[serde(default)]
    pub argv0: Option<String>,
    /// Each must appear as a whole argument.
    #[serde(default)]
    pub args_contain: Vec<String>,
    /// Searched in the arguments and in whatever `stdin_prompt` reads.
    #[serde(default)]
    pub prompt_contains: Option<String>,
    /// How much stdin to read before matching, for CLIs that take the prompt there.
    #[serde(default)]
    pub stdin_prompt: StdinPrompt,
    #[serde(default, rename = "step")]
    pub steps: Vec<FakeStep>,
}

impl FakeRule {
    pub fn for_program(argv0: &str) -> Self {
        Self {
            argv0: Some(argv0.to_owned()),
            ..Self::default()
        }
    }

    pub fn args(mut self, args: &[&str]) -> Self {
        self.args_contain
            .extend(args.iter().map(|a| (*a).to_owned()));
        self
    }

    pub fn prompt(mut self, text: &str) -> Self {
        self.prompt_contains = Some(text.to_owned());
        self
    }

    pub fn stdin(mut self, how: StdinPrompt) -> Self {
        self.stdin_prompt = how;
        self
    }

    pub fn step(mut self, step: FakeStep) -> Self {
        self.steps.push(step);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdinPrompt {
    /// Nothing; the prompt is an argument.
    #[default]
    None,
    /// The first line, as with stream-json input that stays open.
    Line,
    All,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FakeStep {
    /// Prints one line on stdout.
    Emit(String),
    /// Prints one line with `{uuid}` replaced by the uuid of the last message read from stdin,
    /// as Claude Code's `command_lifecycle` names the message it moves along.
    EmitWithUuid(String),
    /// Prints every line of a recorded file on stdout. Relative paths start at the scenario
    /// file's folder.
    EmitFile(String),
    SleepMs(u64),
    /// Waits until a signal of this name is given with [`FakeClis::signal`], so a test can
    /// hold an agent at work until it has seen what it needs on screen, however long that takes
    /// on a busy machine.
    ///
    /// [`FakeClis::signal`]: crate::FakeClis::signal
    AwaitSignal(String),
    /// Writes a file under the working directory, as an agent editing code would.
    Write {
        /// Relative path; `..` and absolute paths are refused.
        path: String,
        content: String,
    },
    /// Reads one line of stdin into the log.
    ReadStdinLine(bool),
    /// Reads stdin lines until one contains the text; fails the run at end of input.
    ExpectStdinContains(String),
    Stderr(String),
    /// Calls a tool of the first MCP server in the `--mcp-config` file the fake was given, as
    /// Claude Code calls its permission prompt tool, and waits for the answer, however long.
    CallTool {
        name: String,
        arguments: Value,
    },
    Exit(i32),
    /// Waits until killed.
    Hang(bool),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "log", rename_all = "snake_case")]
pub enum LogLine {
    Invocation(Invocation),
    StdinLine {
        pid: u32,
        line: String,
    },
    Exit {
        pid: u32,
        code: i32,
    },
    /// One ACP frame. A line that isn't JSON is kept as a string.
    AcpFrame {
        pid: u32,
        from_agent: bool,
        frame: Value,
    },
    /// The client answered a permission request with another option than the one expected.
    WrongPick {
        pid: u32,
        tool_call_id: Value,
        expected: String,
        picked: String,
    },
    /// A `report` step's call of `argonauts_report`, with the tool's result or the error.
    ReportCall {
        pid: u32,
        server: String,
        report: Value,
        reply: Value,
    },
    /// A `call_tool` step's call, with the tool's result or the error.
    ToolCall {
        pid: u32,
        name: String,
        arguments: Value,
        reply: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invocation {
    pub pid: u32,
    pub argv0: String,
    pub args: Vec<String>,
    /// Names of every environment variable set, sorted. Never values.
    pub env: Vec<String>,
    pub cwd: PathBuf,
    /// Stdin read before matching, if the rule read any.
    pub stdin: Option<String>,
    /// The index of the rule that matched, among the `[[acp]]` rules for an ACP session.
    pub rule: Option<usize>,
}

/// A missing file is empty, and a torn last line is skipped.
pub fn read_log(path: &Path) -> anyhow::Result<Vec<LogLine>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_round_trip_through_toml() {
        let scenario = FakeScenario::default()
            .version("codex", "codex-cli 0.158.0")
            .rule(
                FakeRule::for_program("codex")
                    .args(&["exec"])
                    .prompt("hello")
                    .stdin(StdinPrompt::All)
                    .step(FakeStep::Emit("{\"type\":\"thread.started\"}".into()))
                    .step(FakeStep::Write {
                        path: "hello.txt".into(),
                        content: "hi\n".into(),
                    })
                    .step(FakeStep::ReadStdinLine(true))
                    .step(FakeStep::Hang(true))
                    .step(FakeStep::Exit(3)),
            );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.toml");
        scenario.save(&path).unwrap();
        assert_eq!(FakeScenario::load(&path).unwrap(), scenario);
    }

    #[test]
    fn the_documented_shape_parses() {
        let scenario: FakeScenario = toml::from_str(
            r#"
[version]
claude = "2.1.284 (Claude Code)"

[[rule]]
argv0 = "claude"
args_contain = ["-p"]
prompt_contains = "hi"
[[rule.step]]
emit_file = "fixtures/conversation.stdout.jsonl"
[[rule.step]]
sleep_ms = 20
[[rule.step]]
await_signal = "screen-seen"
[[rule.step]]
write = { path = "a.txt", content = "a" }
[[rule.step]]
exit = 0
"#,
        )
        .unwrap();
        assert_eq!(scenario.rules[0].steps.len(), 5);
        assert_eq!(scenario.rules[0].steps[1], FakeStep::SleepMs(20));
        assert_eq!(
            scenario.rules[0].steps[2],
            FakeStep::AwaitSignal("screen-seen".to_owned())
        );
    }
}
