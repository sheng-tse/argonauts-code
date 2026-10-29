//! Stands in for CLIs and ACP agents in tests: plays the first matching rule of
//! `ARGONAUTS_FAKE_SCENARIO` and logs every invocation to `ARGONAUTS_FAKE_LOG`. With `acp` as
//! its first argument it plays an `[[acp]]` rule as an ACP agent instead. Without a matching
//! rule it still answers `--version` from the scenario, and `--help`.

mod acp;
mod help;
mod report;

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use argonauts_testkit::deadline;
use argonauts_testkit::fake_clis::{is_message_command, signal_path};
use argonauts_testkit::scenario::{
    BAD_SCENARIO_EXIT, FakeRule, FakeScenario, FakeStep, Invocation, LOG_ENV, LogLine,
    NO_RULE_EXIT, SCENARIO_ENV, StdinPrompt,
};

fn main() -> ExitCode {
    let mut argv = std::env::args();
    let argv0 = argv
        .next()
        .map(|arg| {
            Path::new(&arg)
                .file_name()
                .map_or(arg.clone(), |name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let args: Vec<String> = argv.collect();
    let code = match run(&argv0, &args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("argonauts-fake-cli: {err:#}");
            BAD_SCENARIO_EXIT
        }
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn run(argv0: &str, args: &[String]) -> anyhow::Result<i32> {
    // `gh` answers from its own world file rather than a scenario.
    if argv0 == "gh" {
        return Ok(argonauts_testkit::fake_gh::serve(args));
    }
    let scenario_path = std::env::var_os(SCENARIO_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("{SCENARIO_ENV} is not set"))?;
    let scenario = FakeScenario::load(&scenario_path)?;
    let base = scenario_path
        .parent()
        .map(Path::to_owned)
        .unwrap_or_default();
    if args.first().is_some_and(|arg| arg == "acp") {
        return acp::serve(&scenario.acp, &base, argv0, args);
    }
    // OpenCode's server for a run of calls says where it answers, then lives until its input
    // closes, as `opencode serve --stdio` does. The calls to it find no rule.
    if argv0 == "opencode" && args.first().is_some_and(|arg| arg == "serve") {
        log_invocation(argv0, args, None, None);
        println!(r#"{{"url":"http://127.0.0.1:9"}}"#);
        let _ = io::copy(&mut io::stdin(), &mut io::sink());
        return Ok(0);
    }
    let mut stdin = Stdin::new();

    let matched = scenario
        .rules
        .iter()
        .enumerate()
        .find(|(_, rule)| matches(rule, argv0, args, &mut stdin));
    log_invocation(
        argv0,
        args,
        stdin.prompt.clone(),
        matched.map(|(index, _)| index),
    );

    let Some((_, rule)) = matched else {
        if args.iter().any(|a| a == "--version")
            && let Some(version) = scenario.version.get(argv0)
        {
            println!("{version}");
            return Ok(0);
        }
        if let Some(help) = help::text(argv0, args) {
            print!("{help}");
            return Ok(0);
        }
        if is_message_command(argv0, args) {
            return Ok(0);
        }
        eprintln!(
            "argonauts-fake-cli: no rule matched {argv0} {}",
            args.join(" ")
        );
        return Ok(NO_RULE_EXIT);
    };
    let code = play(&rule.steps, &base, args, &mut stdin)?;
    log(&LogLine::Exit {
        pid: std::process::id(),
        code,
    });
    Ok(code)
}

fn matches(rule: &FakeRule, argv0: &str, args: &[String], stdin: &mut Stdin) -> bool {
    if rule.argv0.as_deref().is_some_and(|want| want != argv0) {
        return false;
    }
    if !rule.args_contain.iter().all(|want| args.contains(want)) {
        return false;
    }
    let Some(want) = &rule.prompt_contains else {
        return true;
    };
    if prompt_args(args).any(|arg| arg.contains(want.as_str())) {
        return true;
    }
    stdin.read_prompt(rule.stdin_prompt);
    stdin
        .prompt
        .as_deref()
        .is_some_and(|text| text.contains(want.as_str()))
}

/// The arguments a prompt can be in. The name a session is listed by is left out: it carries
/// the conversation's title, so an earlier request would match a later turn.
fn prompt_args(args: &[String]) -> impl Iterator<Item = &String> {
    let is_name = std::iter::once(false).chain(args.iter().map(|arg| arg == "--name"));
    args.iter()
        .zip(is_name)
        .filter(|(_, is_name)| !is_name)
        .map(|(arg, _)| arg)
}

fn play(
    steps: &[FakeStep],
    base: &Path,
    args: &[String],
    stdin: &mut Stdin,
) -> anyhow::Result<i32> {
    let mut out = io::stdout().lock();
    for step in steps {
        match step {
            FakeStep::Emit(line) => {
                writeln!(out, "{}", reset_times(line))?;
                out.flush()?;
            }
            FakeStep::EmitWithUuid(line) => {
                let uuid = stdin.last_uuid.as_deref().unwrap_or_default();
                writeln!(out, "{}", line.replace("{uuid}", uuid))?;
                out.flush()?;
            }
            FakeStep::EmitFile(path) => {
                let path = base.join(path);
                let text = fs::read_to_string(&path)
                    .map_err(|err| anyhow::anyhow!("{}: {err}", path.display()))?;
                for line in text.lines() {
                    writeln!(out, "{line}")?;
                }
                out.flush()?;
            }
            FakeStep::SleepMs(ms) => thread::sleep(Duration::from_millis(*ms)),
            FakeStep::AwaitSignal(name) => await_signal(name)?,
            FakeStep::Write { path, content } => write_inside_cwd(path, content)?,
            FakeStep::ReadStdinLine(_) => {
                if let Some(line) = stdin.line() {
                    log(&LogLine::StdinLine {
                        pid: std::process::id(),
                        line,
                    });
                }
            }
            FakeStep::ExpectStdinContains(want) => loop {
                let Some(line) = stdin.line() else {
                    eprintln!("argonauts-fake-cli: stdin ended before {want:?}");
                    return Ok(1);
                };
                let found = line.contains(want.as_str());
                log(&LogLine::StdinLine {
                    pid: std::process::id(),
                    line,
                });
                if found {
                    break;
                }
            },
            FakeStep::Stderr(line) => eprintln!("{}", reset_times(line)),
            FakeStep::CallTool { name, arguments } => {
                let reply = call_config_tool(args, name, arguments)
                    .unwrap_or_else(|err| serde_json::json!({ "error": format!("{err:#}") }));
                log(&LogLine::ToolCall {
                    pid: std::process::id(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                    reply,
                });
            }
            FakeStep::Exit(code) => return Ok(*code),
            FakeStep::Hang(_) => loop {
                thread::sleep(Duration::from_secs(60));
            },
        }
    }
    Ok(0)
}

/// Calls `name` on the server Claude Code's `--mcp-config` names, as Claude Code would.
/// `line` with each `@RESET_IN:<n>s@` as the Unix time `n` seconds from now, as Claude Code's
/// `resetsAt` is, so a reset counts from when the limit is hit rather than from when the
/// scenario was written.
fn reset_times(line: &str) -> String {
    const OPEN: &str = "@RESET_IN:";
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let secs = after
            .split_once("s@")
            .and_then(|(secs, tail)| Some((secs.parse::<i64>().ok()?, tail)));
        match secs {
            Some((secs, tail)) => {
                let at = time::OffsetDateTime::now_utc().unix_timestamp() + secs;
                out.push_str(&at.to_string());
                rest = tail;
            }
            None => {
                out.push_str(OPEN);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn call_config_tool(
    args: &[String],
    name: &str,
    arguments: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let path = args
        .iter()
        .position(|arg| arg == "--mcp-config")
        .and_then(|at| args.get(at + 1))
        .ok_or_else(|| anyhow::anyhow!("no --mcp-config to call {name} through"))?;
    let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    let server = report::config_server(&config)
        .ok_or_else(|| anyhow::anyhow!("{path} names no MCP server"))?;
    report::call_tool(&server, name, arguments)
}

/// Writes a file the way an agent would, but never outside the working directory.
fn write_inside_cwd(path: &str, content: &str) -> anyhow::Result<()> {
    let relative = Path::new(path);
    let escapes = relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir));
    if escapes {
        anyhow::bail!("refusing to write {path:?} outside the working directory");
    }
    if let Some(parent) = relative.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(relative, content)?;
    Ok(())
}

fn log_invocation(argv0: &str, args: &[String], stdin: Option<String>, rule: Option<usize>) {
    log(&LogLine::Invocation(Invocation {
        pid: std::process::id(),
        argv0: argv0.to_owned(),
        args: args.to_owned(),
        env: env_names(),
        cwd: std::env::current_dir().unwrap_or_default(),
        stdin,
        rule,
    }));
}

fn env_names() -> Vec<String> {
    let mut names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The longest a fake waits for a test's signal, before the deadline scale stretches it: past
/// any wait a test makes before it gives one.
const SIGNAL_WAIT: Duration = Duration::from_secs(600);

/// Waits for the test's signal. Without a log there's no test to give it, so the steps go on.
/// A test that ended took the log's folder with it, and one that never signals in time has
/// failed already, so the fake stops then rather than wait on after its test.
fn await_signal(name: &str) -> anyhow::Result<()> {
    let Some(log) = std::env::var_os(LOG_ENV) else {
        return Ok(());
    };
    let path = signal_path(Path::new(&log), name)?;
    let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let deadline = deadline::after(SIGNAL_WAIT);
    while !path.exists() {
        anyhow::ensure!(
            folder.is_dir(),
            "the test that would signal {name} has ended"
        );
        anyhow::ensure!(
            Instant::now() < deadline,
            "no {name} signal came in {:?}",
            deadline::scaled(SIGNAL_WAIT)
        );
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn log(line: &LogLine) {
    let Some(path) = std::env::var_os(LOG_ENV) else {
        return;
    };
    let Ok(mut text) = serde_json::to_string(line) else {
        return;
    };
    text.push('\n');
    // One write per line, so lines from concurrent fakes don't interleave.
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

/// Stdin, read lazily: some callers keep it open for the whole session.
struct Stdin {
    reader: io::BufReader<io::Stdin>,
    prompt: Option<String>,
    /// The uuid of the last message read that carried one.
    last_uuid: Option<String>,
}

impl Stdin {
    fn new() -> Self {
        Self {
            reader: io::BufReader::new(io::stdin()),
            prompt: None,
            last_uuid: None,
        }
    }

    fn read_prompt(&mut self, how: StdinPrompt) {
        if self.prompt.is_some() {
            return;
        }
        self.prompt = match how {
            StdinPrompt::None => None,
            StdinPrompt::Line => self.prompt_line(),
            StdinPrompt::All => {
                let mut text = String::new();
                self.reader.read_to_string(&mut text).ok().map(|_| text)
            }
        };
    }

    /// The first line that isn't a control request, as Claude Code's `initialize` ahead of a
    /// conversation's message is. Those go to the log.
    fn prompt_line(&mut self) -> Option<String> {
        loop {
            let line = self.line()?;
            if !line.contains(r#""type":"control_request""#) {
                return Some(line);
            }
            log(&LogLine::StdinLine {
                pid: std::process::id(),
                line,
            });
        }
    }

    fn line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                let line = line.trim_end_matches(['\n', '\r']).to_owned();
                let uuid = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|value| value.get("uuid")?.as_str().map(str::to_owned));
                if uuid.is_some() {
                    self.last_uuid = uuid;
                }
                Some(line)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{prompt_args, reset_times};

    #[test]
    fn a_reset_counts_from_when_the_line_is_sent() {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let line = reset_times(r#"{"resetsAt":@RESET_IN:6s@,"note":"@RESET_IN:x"}"#);
        let at: i64 = line
            .trim_start_matches(r#"{"resetsAt":"#)
            .split(',')
            .next()
            .and_then(|at| at.parse().ok())
            .expect("a time");
        assert!((now + 6..=now + 8).contains(&at), "{line}");
        assert!(line.ends_with(r#""note":"@RESET_IN:x"}"#), "{line}");
    }

    #[test]
    fn a_session_name_is_not_where_its_prompt_is() {
        let args: Vec<String> = [
            "-p",
            "hi",
            "--name",
            "argonauts fix the readme",
            "--verbose",
        ]
        .map(str::to_owned)
        .to_vec();
        let seen: Vec<&str> = prompt_args(&args).map(String::as_str).collect();
        assert_eq!(seen, ["-p", "hi", "--name", "--verbose"]);
    }
}
