//! Shared steps for the app scenarios: a repo with the fake agents, the app started and
//! typed at, and the trace, fake log, settings files, and journals read back.
// Each `app_*.rs` test crate uses a different part of this module.
#![allow(dead_code, unused_imports)]

mod files;
mod journal;
mod pids;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::app::{
    AppChoice, AppEvent, AppMode, ChoiceKind, ConversationEvent, TraceLine, UiCommand,
};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{AnswerOption, ApprovalEvent, ApprovalKind, ApprovalOpened};
use argonauts_core::layout::run_files;
use argonauts_core::names::ChoiceId;
use argonauts_scenarios::{
    COLS, ROWS, Scenario, WAIT, as_set_up, past_the_guard_of, past_the_question_guard,
};
use argonauts_testkit::{Invocation, Key, Pty, PtyCommand, argonauts_bin};

pub use files::{TomlEntry, json_at, same_toml, toml_entries, toml_value};
pub use journal::{
    WaitingRun, branch_files, fabricate_waiting_run, run_branches, run_events, run_id,
    run_settings, run_started, wait_journal,
};

/// The fake `claude`, `codex`, and `agy` every app scenario plays.
pub const AGENTS: &str = "app/agents.toml";

/// Keeps the guided setup closed and vim keys off, and gives a waiting run a short wait grace.
pub const GLOBAL_CONFIG: &str = "\
# Written by the scenario test, kept through every change.
[crew]
main = \"claude-code\"
worker = [\"claude-code\", \"codex\", \"antigravity-cli\"]
reviewer = [\"claude-code\", \"codex\", \"antigravity-cli\"]
judge = [\"claude-code\", \"codex\", \"antigravity-cli\"]

[ui]
keys = \"default\"
# These scenarios wait for an agent's words while it works, which its live block shows.
agents_in_conversation = \"as_they_work\"

# The fake CLIs write files directly, with none of their edit tools' events, so every new file
# counts as their work.
[commit]
unclaimed = \"keep\"

[run_host]
wait_grace = \"1s\"

# These scenarios follow Captain runs in a checked worktree.
[captain]
kind = \"checked\"

# Their crew runs answer the fixed phases, so a built-in workflow runs those.
[workflows]
adapt_with = \"phases\"

# And the verdict that ends each conversation reply, which a CLI that takes no tools keeps.
# `app_main_session.rs` plays the main session with its tools.
[main_session]
tools = false
";

/// One check that always passes, for scenarios that reach the merge question.
pub const PASSING_CHECK: &str = "
[[checks.run]]
name = \"tests\"
command = [\"true\"]
";

/// Requests the fake agents answer, one per path through the conversation.
pub mod requests {
    /// Answered, no run.
    pub const HI: &str = "hi";
    /// Unclear; the rules would pick crew.
    pub const QUESTION_COULD_BE_TASK: &str = "how should we add rate limiting?";
    /// A one-file docs change, so the rules pick Captain.
    pub const SMALL_FIX: &str =
        "the install command in README.md is wrong, it should be npm install @acme/api";
    /// A security-labeled feature, so the rules pick crew.
    pub const CREW_TASK: &str = "add per-key rate limiting to the public API";
    /// Two files, no tests, and a migration label, so the labels pick crew.
    pub const LABELED_TASK: &str = "move sessions to the new storage backend";
    /// Unclear; the rules would pick Captain.
    pub const UNCLEAR_SMALL: &str = "tidy the changelog";
    /// A Captain change whose worker works until stopped.
    pub const SLOW_TASK: &str = "rename the config loader everywhere";
    /// A Captain change whose worker writes its notes, then sits in a tool call until stopped,
    /// so a pause stays pending.
    pub const TASK_IN_A_TOOL_CALL: &str = "rename the config loader while the build runs";
    /// A Captain change whose worker takes a few seconds, then reports.
    pub const TIMED_TASK: &str = "rename the settings reader in a few seconds";
    /// A Captain change whose worker works until the test signals `worker-may-finish`.
    pub const HELD_TASK: &str = "rename the settings writer when told";
    /// A conversation turn the fake never finishes.
    pub const HANGING_TURN: &str = "take your time with this one";
    /// Answered only; for `/ask`.
    pub const ASK: &str = "should we split the storage module and add a cache?";
    /// The guidance the resumed worker's prompt must carry.
    pub const GUIDANCE: &str = "use the name loadSettings";
}

/// A scenario for the app. Dropping it stops what it left running, as every [`Scenario`] does.
pub struct AppScenario(Scenario);

impl std::ops::Deref for AppScenario {
    type Target = Scenario;

    fn deref(&self) -> &Scenario {
        &self.0
    }
}

/// A repo with the fake agents and the global config above.
pub fn scenario() -> anyhow::Result<AppScenario> {
    scenario_with(GLOBAL_CONFIG)
}

/// A repo with the fake agents and this global config.
pub fn scenario_with(global_config: &str) -> anyhow::Result<AppScenario> {
    let scenario = AppScenario(Scenario::new(AGENTS)?);
    std::fs::write(scenario.repo.global_config(), as_set_up(global_config))
        .context("writing the global config")?;
    Ok(scenario)
}

/// A repo with the fake agents and no global config, so the guided setup opens.
pub fn first_run_scenario() -> anyhow::Result<AppScenario> {
    Ok(AppScenario(Scenario::new(AGENTS)?))
}

/// A repo whose fakes play `agents`, such as a changed copy of [`AGENTS`].
pub fn scenario_playing(fakes: &Path) -> anyhow::Result<AppScenario> {
    Ok(AppScenario(Scenario::from_file(fakes)?))
}

/// `GLOBAL_CONFIG` with more `[workflows]` keys, each on a line of its own.
pub fn config_with_workflows(keys: &str) -> String {
    GLOBAL_CONFIG.replace("[workflows]\n", &format!("[workflows]\n{keys}"))
}

/// `GLOBAL_CONFIG` with another main agent, an adapter id such as `codex`.
pub fn config_with_main_agent(agent: &str) -> String {
    GLOBAL_CONFIG.replace(
        "[crew]\nmain = \"claude-code\"",
        &format!("[crew]\nmain = \"{agent}\""),
    )
}

fn first_number(text: &str) -> Option<u32> {
    text.split(|c: char| !c.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse().ok())
}

/// The run host pid a run recorded.
pub fn run_host_pid(run_dir: &Path) -> anyhow::Result<u32> {
    let path = run_dir.join(run_files::PID);
    let text = read(&path)?;
    first_number(&text).with_context(|| format!("no pid in {}", path.display()))
}

/// `Scenario::launch`, plus variables set after the scenario's own, so they win.
pub fn launch_with(
    scenario: &Scenario,
    args: &[&str],
    extra: &[(&str, OsString)],
) -> anyhow::Result<Pty> {
    let binary = argonauts_bin().context("finding the argonauts binary")?;
    let mut command = PtyCommand::new(binary)
        .args(argonauts_scenarios::launch_defaults(args))
        .cwd(scenario.repo.root())
        .envs(scenario.envs())
        .env_remove("ARGONAUTS_SESSION")
        .size(ROWS, COLS);
    for (name, value) in extra {
        command = command.env(name, value.clone());
    }
    Pty::spawn(command)
}

/// Starts the app and waits until its prompt takes input.
pub fn start(scenario: &Scenario) -> anyhow::Result<Pty> {
    start_with(scenario, &[])
}

pub fn start_with(scenario: &Scenario, args: &[&str]) -> anyhow::Result<Pty> {
    let pty = scenario.launch(args)?;
    wait_ready(scenario, &pty)?;
    Ok(pty)
}

/// The app said hello and the bottom line is up without vim's INSERT label, or a question that
/// needs you has the keys.
pub fn wait_ready(scenario: &Scenario, pty: &Pty) -> anyhow::Result<()> {
    scenario
        .trace
        .wait_for_event(|event| matches!(event, AppEvent::Welcome { .. }), WAIT)
        .context("waiting for the app's welcome")?;
    check_ready(pty)
}

/// Before typing: the bottom line shows, vim's INSERT or NORMAL label doesn't, and nothing looks
/// like a history search, which could resend old prompts.
pub fn check_bottom_row(pty: &Pty) -> anyhow::Result<()> {
    check_screen_for(pty, r"shift\+tab to cycle")
}

/// [`check_bottom_row`], or a question that needs you holding the keys, as a waiting run's question
/// does once you open its run.
fn check_ready(pty: &Pty) -> anyhow::Result<()> {
    check_screen_for(
        pty,
        &format!(r"shift\+tab to cycle|{QUESTION_HAS_THE_KEYS}"),
    )
}

/// The key line of a question that has the keys, which then stands in for the input: "Esc to
/// cancel", or "Esc for not now" on a question that can wait, as the trust question can.
const QUESTION_HAS_THE_KEYS: &str = r"(?m)^  Enter to \w+.* · Esc (?:to|for) ";

fn check_screen_for(pty: &Pty, ready: &str) -> anyhow::Result<()> {
    let screen = pty.wait_for(ready, WAIT)?;
    let text = screen.visible_text();
    // The app starts vim keys in INSERT, and NORMAL has no label to see.
    if text.contains("-- INSERT --") || text.contains("-- VISUAL --") {
        bail!("vim keys are on in the app under test:\n{text}");
    }
    check_no_history_search(pty)
}

pub fn check_no_history_search(pty: &Pty) -> anyhow::Result<()> {
    let text = pty.screen().visible_text().to_lowercase();
    for sign in [
        "reverse-i-search",
        "bck-i-search",
        "history search",
        "search history",
    ] {
        if text.contains(sign) {
            bail!("the app under test opened a history search; stopping:\n{text}");
        }
    }
    Ok(())
}

/// Types a line at the prompt and presses Enter.
pub fn submit(pty: &mut Pty, text: &str) -> anyhow::Result<()> {
    check_bottom_row(pty)?;
    pty.send(text)?;
    check_no_history_search(pty)?;
    pty.press(Key::Enter)
}

/// Waits for the bottom line to show and hold for a moment, pressing Esc on whatever hides it
/// meanwhile: a view the app answers for opens when the answer comes, which can be after the
/// Esc that was to close it.
pub fn back_to_the_input(pty: &mut Pty) -> anyhow::Result<()> {
    let holds = argonauts_testkit::deadline::scaled(Duration::from_millis(300));
    let patience = argonauts_testkit::deadline::scaled(Duration::from_secs(1));
    let deadline = argonauts_testkit::deadline::after(WAIT);
    let mut shown_since = None;
    let mut hidden_since = Instant::now();
    loop {
        let now = Instant::now();
        if pty.screen().visible_text().contains("shift+tab to cycle") {
            let since = *shown_since.get_or_insert(now);
            if now >= since + holds {
                return check_no_history_search(pty);
            }
        } else {
            if shown_since.take().is_some() {
                hidden_since = now;
            }
            // Something opened after all, or is taking its time to close.
            if now >= hidden_since + patience {
                pty.press(Key::Esc)?;
                hidden_since = now;
            }
        }
        if now >= deadline {
            bail!(
                "the bottom line never came back to stay:\n{}",
                pty.screen().visible_text()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Waits for the app to take the slash command typed once the trace had `before` lines, and
/// to answer it.
pub fn wait_answered(scenario: &Scenario, before: usize, line: &str) -> anyhow::Result<()> {
    wait_until(&format!("the app's answer to {line}"), || {
        let lines = scenario.trace.lines()?;
        let after = lines.get(before..).unwrap_or_default();
        let taken = after.iter().position(|line| {
            matches!(
                line,
                TraceLine::In {
                    command: UiCommand::Slash { .. },
                    ..
                }
            )
        });
        let answered = taken.is_some_and(|at| {
            after[at..]
                .iter()
                .any(|line| matches!(line, TraceLine::Out { .. }))
        });
        Ok(answered.then_some(()))
    })
}

/// The number the question shows beside the option the event gives `key`. The letters stay in the
/// protocol, and the screen answers to numbers only.
pub fn number_of(options: &[AnswerOption], key: char) -> char {
    let index = options
        .iter()
        .position(|option| option.key == key)
        .unwrap_or_else(|| panic!("no option `{key}` in {options:?}"));
    char::from_digit(index as u32 + 1, 10).expect("at most 9 options")
}

/// Gives a waiting question focus, then presses option `number`. ↑ on a question you set off, which
/// already has focus, only moves its pointer, and the number answers either way. An option
/// that can't be undone only moves the pointer, so it also needs `confirm`.
pub fn answer_question(pty: &mut Pty, number: char) -> anyhow::Result<()> {
    assert!(
        number.is_ascii_digit(),
        "tasks answer to numbers, not `{number}`"
    );
    check_no_history_search(pty)?;
    // The trace has the question before the screen does, and an Up that comes first recalls the
    // last prompt instead. A question opens focused on an option.
    pty.wait_for_visible(r"❯ \d+\. ", WAIT)?;
    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char(number))
}

/// A question that needs you takes the keys as it shows, so a command typed while it waits needs
/// Esc first, which hands the question on and leaves a line saying who decides it now.
pub fn pass_task_on(pty: &mut Pty) -> anyhow::Result<()> {
    pty.wait_for_visible(QUESTION_HAS_THE_KEYS, WAIT)?;
    past_the_question_guard();
    pty.press(Key::Esc)?;
    pty.wait_for(r"left to the (main )?agent · you, ", WAIT)
        .map(drop)
}

/// For Merge, Abandon, and the other options that wait for Enter after their number.
pub fn answer_question_and_confirm(pty: &mut Pty, number: char) -> anyhow::Result<()> {
    answer_question(pty, number)?;
    pty.press(Key::Enter)
}

pub fn events(scenario: &Scenario) -> anyhow::Result<Vec<AppEvent>> {
    scenario.trace.events()
}

pub fn commands(scenario: &Scenario) -> anyhow::Result<Vec<UiCommand>> {
    scenario.trace.commands()
}

pub fn wait_event(
    scenario: &Scenario,
    what: &str,
    matches: impl Fn(&AppEvent) -> bool,
) -> anyhow::Result<AppEvent> {
    scenario
        .trace
        .wait_for_event(matches, WAIT)
        .with_context(|| format!("waiting for {what}"))
}

/// Waits until `count` events match, for a thing that happens again after an earlier one.
pub fn wait_events(
    scenario: &Scenario,
    what: &str,
    count: usize,
    matches: impl Fn(&AppEvent) -> bool,
) -> anyhow::Result<Vec<AppEvent>> {
    wait_until(what, || {
        let found: Vec<AppEvent> = events(scenario)?
            .into_iter()
            .filter(|e| matches(e))
            .collect();
        Ok((found.len() >= count).then_some(found))
    })
}

/// Polls `check` until it returns something or the scenario's wait runs out.
pub fn wait_until<T>(
    what: &str,
    mut check: impl FnMut() -> anyhow::Result<Option<T>>,
) -> anyhow::Result<T> {
    let deadline = argonauts_testkit::deadline::after(WAIT);
    loop {
        if let Some(found) = check()? {
            return Ok(found);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for {what}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Every conversation turn started so far, as (text, settings hash, uncommitted paths).
pub fn turns_started(scenario: &Scenario) -> anyhow::Result<Vec<TurnStart>> {
    Ok(events(scenario)?
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::Conversation {
                event:
                    ConversationEvent::TurnStarted {
                        text,
                        read_only,
                        uncommitted,
                        settings_sha256,
                        ..
                    },
            } => Some(TurnStart {
                text,
                read_only,
                uncommitted,
                settings_sha256,
            }),
            _ => None,
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnStart {
    pub text: String,
    pub read_only: bool,
    pub uncommitted: Vec<String>,
    pub settings_sha256: Option<String>,
}

pub fn is_turn_finished(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished { .. }
        }
    )
}

/// The `n`th turn to finish, counting from 1.
pub fn wait_turns_finished(scenario: &Scenario, n: usize) -> anyhow::Result<()> {
    wait_events(
        scenario,
        "a conversation turn to finish",
        n,
        is_turn_finished,
    )
    .map(|_| ())
}

/// The newest app choice of `kind` that is still open, as the trace has it. The screen may show
/// it a while later, so a key for it waits for the screen, as [`answer_question`] and
/// [`wait_shown_choice`] do.
pub fn wait_open_choice(scenario: &Scenario, kind: ChoiceKind) -> anyhow::Result<AppChoice> {
    wait_until(&format!("an open {kind:?} choice"), || {
        Ok(open_choices(scenario)?
            .into_iter()
            .rev()
            .find(|c| c.kind == kind))
    })
}

/// [`wait_open_choice`], once the screen shows it too and past its guard, for a key pressed at
/// once.
pub fn wait_shown_choice(
    scenario: &Scenario,
    pty: &Pty,
    kind: ChoiceKind,
) -> anyhow::Result<AppChoice> {
    let choice = wait_open_choice(scenario, kind)?;
    past_the_guard_of(pty, &choice.title)?;
    Ok(choice)
}

pub fn open_choices(scenario: &Scenario) -> anyhow::Result<Vec<AppChoice>> {
    let mut open: Vec<AppChoice> = Vec::new();
    for event in events(scenario)? {
        match event {
            AppEvent::Choice { choice } => open.push(choice),
            AppEvent::ChoiceClosed { id, .. } => open.retain(|c| c.id != id),
            _ => {}
        }
    }
    Ok(open)
}

pub fn wait_choice_closed(scenario: &Scenario, id: &ChoiceId) -> anyhow::Result<String> {
    let closed = wait_event(
        scenario,
        "the choice to close",
        |event| matches!(event, AppEvent::ChoiceClosed { id: closed, .. } if closed == id),
    )?;
    match closed {
        AppEvent::ChoiceClosed { outcome, .. } => Ok(outcome),
        _ => unreachable!("matched a ChoiceClosed"),
    }
}

/// The key of the choice's option whose label contains `label`.
pub fn option_key(choice: &AppChoice, label: &str) -> anyhow::Result<char> {
    choice
        .options
        .iter()
        .find(|option| option.label.contains(label))
        .map(|option| option.key)
        .with_context(|| format!("no option {label:?} in {choice:?}"))
}

pub fn notices(scenario: &Scenario) -> anyhow::Result<Vec<String>> {
    Ok(events(scenario)?
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::Notice { text, .. } => Some(text),
            _ => None,
        })
        .collect())
}

/// `SettingChanged` events as (key, value, reset).
pub fn setting_changes(scenario: &Scenario) -> anyhow::Result<Vec<(String, String, bool)>> {
    Ok(events(scenario)?
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::SettingChanged {
                key, value, reset, ..
            } => Some((key, value, reset)),
            _ => None,
        })
        .collect())
}

/// Invocations of `program`, oldest first.
pub fn invocations_of(scenario: &Scenario, program: &str) -> anyhow::Result<Vec<Invocation>> {
    Ok(scenario
        .clis
        .invocations()?
        .into_iter()
        .filter(|invocation| invocation.argv0 == program)
        .collect())
}

/// How Claude Code is asked for its plan usage live: initialize and get_usage on stdin, and no
/// prompt, so no model runs.
const CLAUDE_USAGE_PROBE: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--strict-mcp-config",
    "--setting-sources",
    "",
];

/// Whether `args` are the plan usage read's, alone or with the model it names after them.
fn claude_usage_probe(args: &[String]) -> bool {
    let Some((read, named)) = args.split_at_checked(CLAUDE_USAGE_PROBE.len()) else {
        return false;
    };
    let reads_usage = read
        .iter()
        .map(String::as_str)
        .eq(CLAUDE_USAGE_PROBE.iter().copied());
    reads_usage
        && match named {
            [] => true,
            [flag, _model] => flag == "--model",
            _ => false,
        }
}

/// Invocations that ran a model session: every one but `--version`, the `--help` doctor reads
/// for flags, login checks, the model lists `/model` refreshes, and the plan usage reads
/// (`codex app-server`, `agy -p /usage`, Claude Code's probe), which start no turn.
pub fn sessions_of(scenario: &Scenario, program: &str) -> anyhow::Result<Vec<Invocation>> {
    Ok(invocations_of(scenario, program)?
        .into_iter()
        .filter(|invocation| {
            let args = &invocation.args;
            let model_list =
                args == &["debug", "models"] || args == &["--output-format", "json", "models"];
            let plan_usage_read = args.first().is_some_and(|arg| arg == "app-server")
                || has_flag(args, "-p", "/usage")
                || claude_usage_probe(args);
            let login_check = args == &["auth", "status"];
            // agy started with nothing to do shows its start-up screen, which names the
            // account; nothing is sent.
            let account_read = program == "agy" && args.is_empty();
            // OpenCode asked which providers it has, what they offer and its default model, or
            // anything else outside a session through `opencode api` (its model list among
            // them, on a server started for the asks), with `opencode models` where that can't
            // answer.
            let providers_read = program == "opencode"
                && (args == &["auth", "list", "--standalone", "--format", "json"]
                    || args == &["models", "--standalone"]
                    || args.starts_with(&["models".to_owned(), "--server".to_owned()])
                    || args == &["serve", "--stdio", "--port", "0"]
                    || args == &["debug", "config"]
                    || args.first().is_some_and(|arg| arg == "api"));
            !model_list
                && !plan_usage_read
                && !login_check
                && !account_read
                && !providers_read
                && !args
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "--version" | "--help" | "login"))
        })
        .map(pids::as_seen_here)
        .collect())
}

/// Model sessions of every agent.
pub fn all_sessions(scenario: &Scenario) -> anyhow::Result<Vec<Invocation>> {
    let mut all = Vec::new();
    for program in ["claude", "codex", "agy", "opencode"] {
        all.extend(sessions_of(scenario, program)?);
    }
    Ok(all)
}

/// Claude Code sessions with the given permission mode: `dontAsk` is read-only, `acceptEdits`
/// builds.
pub fn claude_sessions(scenario: &Scenario, mode: &str) -> anyhow::Result<Vec<Invocation>> {
    Ok(sessions_of(scenario, "claude")?
        .into_iter()
        .filter(|invocation| invocation.args.iter().any(|arg| arg == mode))
        .collect())
}

/// True when `args` holds `flag` directly followed by `value`.
pub fn has_flag(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

/// True when `args` holds these words in a row, such as `resume <id>`.
pub fn has_words(args: &[String], words: &[&str]) -> bool {
    args.windows(words.len())
        .any(|window| window.iter().zip(words).all(|(arg, word)| arg == word))
}

pub fn pid_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn wait_pid_gone(pid: u32) -> anyhow::Result<()> {
    wait_until(&format!("process {pid} to exit"), || {
        Ok((!pid_alive(pid)).then_some(()))
    })
}

pub fn kill_hard(pid: u32) -> anyhow::Result<()> {
    let status = Command::new("/bin/kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .context("running kill")?;
    if !status.success() {
        bail!("kill -KILL {pid} failed");
    }
    Ok(())
}

/// Runs `argonauts` without a terminal: stdin is empty, as in a script.
pub fn run_cli(scenario: &Scenario, args: &[&str]) -> anyhow::Result<Output> {
    run_cli_with(scenario, args, &[])
}

pub fn run_cli_with(
    scenario: &Scenario,
    args: &[&str],
    extra: &[(&str, OsString)],
) -> anyhow::Result<Output> {
    let binary = argonauts_bin().context("finding the argonauts binary")?;
    let mut command = Command::new(binary);
    command
        .args(argonauts_scenarios::launch_defaults(args))
        .current_dir(scenario.repo.root())
        .env_remove("ARGONAUTS_SESSION")
        .envs(scenario.envs())
        .stdin(Stdio::null());
    for (name, value) in extra {
        command.env(name, value);
    }
    command.output().context("running argonauts")
}

pub fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A local address nothing listens on, for probes that must find no server.
pub fn closed_port_url() -> anyhow::Result<String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").context("binding a port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("http://127.0.0.1:{port}/v1"))
}

/// Points the setup's probe of every known local server at a closed port, so no test reaches
/// a server running on this computer.
pub fn no_local_servers() -> anyhow::Result<Vec<String>> {
    let mut flags = Vec::new();
    for id in ["ollama", "lmstudio", "llamacpp", "vllm"] {
        flags.push("--set".to_owned());
        flags.push(format!("endpoints.{id}.base_url={}", closed_port_url()?));
    }
    Ok(flags)
}

pub fn project_config(scenario: &Scenario) -> PathBuf {
    scenario.repo.root().join(".argonauts").join("config.toml")
}

pub fn project_local_config(scenario: &Scenario) -> PathBuf {
    scenario
        .repo
        .root()
        .join(".argonauts")
        .join("config.local.toml")
}

pub fn read(path: &Path) -> anyhow::Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Moves the trace aside, so a second launch's events start from an empty file.
pub fn rotate_trace(scenario: &Scenario, name: &str) -> anyhow::Result<()> {
    let path = scenario.trace.path();
    if path.exists() {
        std::fs::rename(path, path.with_extension(name)).context("moving the trace aside")?;
    }
    Ok(())
}

/// Exits the app with Ctrl+C twice and waits for it to go.
pub fn quit(pty: &mut Pty) -> anyhow::Result<()> {
    pty.press(Key::Ctrl('c'))?;
    pty.press(Key::Ctrl('c'))?;
    pty.wait_for_exit(WAIT).map(|_| ())
}

/// The run directories under the scenario's home, oldest first.
pub fn run_dirs(scenario: &Scenario) -> Vec<PathBuf> {
    argonauts_testkit::journal::run_dirs(scenario.repo.home())
}

pub fn wait_run_dir(scenario: &Scenario) -> anyhow::Result<PathBuf> {
    wait_until("a run directory", || Ok(run_dirs(scenario).pop()))
}

/// A run's approval, as the app forwarded it.
pub fn wait_approval(scenario: &Scenario, kind: ApprovalKind) -> anyhow::Result<ApprovalOpened> {
    wait_until(&format!("a {kind:?} approval"), || {
        Ok(events(scenario)?.into_iter().find_map(|event| match event {
            AppEvent::Run { event } => match event.kind {
                RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
                    Some(*opened)
                }
                _ => None,
            },
            _ => None,
        }))
    })
}

pub fn wait_mode(
    scenario: &Scenario,
    what: &str,
    matches: impl Fn(&AppMode) -> bool,
) -> anyhow::Result<AppMode> {
    let event = wait_event(
        scenario,
        what,
        |event| matches!(event, AppEvent::Mode { mode } if matches(mode)),
    )?;
    match event {
        AppEvent::Mode { mode } => Ok(mode),
        _ => unreachable!("matched a mode"),
    }
}

/// The run is working and its worker has started: returns the worker's fake pid.
pub fn wait_worker_started(scenario: &Scenario) -> anyhow::Result<u32> {
    wait_until("the worker to start", || {
        Ok(claude_sessions(scenario, "acceptEdits")?
            .first()
            .map(|i| i.pid))
    })
}

/// Answers the scope question a setting given as an argument asks, then the safety confirm
/// when one follows. `scope` is the option's key in the event: `s`, `m`, `p`, or `e`.
pub fn answer_scope(
    scenario: &Scenario,
    pty: &mut Pty,
    scope: char,
    confirm: Option<char>,
) -> anyhow::Result<()> {
    let question = wait_open_choice(scenario, ChoiceKind::SettingScope)?;
    answer_question(pty, number_of(&question.options, scope))?;
    wait_choice_closed(scenario, &question.id)?;
    if let Some(answer) = confirm {
        let safety = wait_open_choice(scenario, ChoiceKind::ConfirmSafetySetting)?;
        answer_question(pty, number_of(&safety.options, answer))?;
        wait_choice_closed(scenario, &safety.id)?;
    }
    Ok(())
}

/// Waits for the `n`th `SettingChanged` of `key` and returns its value.
pub fn wait_setting_changed(scenario: &Scenario, key: &str, n: usize) -> anyhow::Result<String> {
    let changes = wait_until(&format!("{key} to change"), || {
        let found: Vec<String> = setting_changes(scenario)?
            .into_iter()
            .filter(|(changed, _, _)| changed == key)
            .map(|(_, value, _)| value)
            .collect();
        Ok((found.len() >= n).then_some(found))
    })?;
    Ok(changes[n - 1].clone())
}

/// `GLOBAL_CONFIG` with a check that passes, for runs that reach the merge question.
pub fn run_config() -> String {
    format!("{GLOBAL_CONFIG}{PASSING_CHECK}")
}

/// Starts `task` with `/captain` and returns the new run's directory.
pub fn start_captain(scenario: &Scenario, pty: &mut Pty, task: &str) -> anyhow::Result<PathBuf> {
    let before = run_dirs(scenario).len();
    submit(pty, &format!("/captain {task}"))?;
    wait_until("the run to start", || {
        let dirs = run_dirs(scenario);
        Ok((dirs.len() > before).then(|| dirs[dirs.len() - 1].clone()))
    })
}

/// Runs the small fix Captain up to its merge question.
pub fn reach_merge_question(
    scenario: &Scenario,
    pty: &mut Pty,
) -> anyhow::Result<(PathBuf, ApprovalOpened)> {
    let run_dir = start_captain(scenario, pty, requests::SMALL_FIX)?;
    let merge = wait_approval(scenario, ApprovalKind::Merge)?;
    Ok((run_dir, merge))
}

/// Presses Merge on the waiting merge question and waits for the run to finish merged.
pub fn merge(pty: &mut Pty, run_dir: &Path) -> anyhow::Result<()> {
    answer_question_and_confirm(pty, '1')?;
    wait_journal(run_dir, "the run to finish merged", |event| {
        matches!(
            event.kind,
            RunEventKind::Run(argonauts_core::events::run::RunLifecycle::Finished {
                outcome: argonauts_core::events::run::RunOutcome::Merged,
                ..
            })
        )
    })
    .map(|_| ())
}

/// Turns vim keys on with `/vim` and straight back off. With them on the prompt must be in
/// INSERT, where typing is plain text; in NORMAL, `/` would start a search, so this stops
/// rather than type there. Returns the two values `ui.keys` took.
pub fn toggle_vim_on_and_off(scenario: &Scenario, pty: &mut Pty) -> anyhow::Result<[String; 2]> {
    let before = setting_changes(scenario)?
        .iter()
        .filter(|(key, _, _)| key == "ui.keys")
        .count();
    submit(pty, "/vim")?;
    let on = wait_setting_changed(scenario, "ui.keys", before + 1)?;
    pty.wait_for("-- INSERT --", WAIT)
        .context("vim keys came on outside INSERT; not typing")?;
    check_no_history_search(pty)?;
    pty.send("/vim")?;
    check_no_history_search(pty)?;
    pty.press(Key::Enter)?;
    let off = wait_setting_changed(scenario, "ui.keys", before + 2)?;
    check_bottom_row(pty)?;
    Ok([on, off])
}
