//! Summaries on the real binary, over the records the Behaviors drawings were made from. From
//! `argonauts behaviors --summaries`, with Claude Code's or Codex's cheapest model as the
//! summarizer, each request is a session of the fake CLI, started inside the sandbox, and the
//! summaries it answers are written. On the Behaviors tab, an update that stops says why.

use std::fs;

use anyhow::Context as _;
use argonauts_scenarios::{Scenario, WAIT, no_servers_of_yours};
use argonauts_testkit::approved::{self, Drawing, Manifest, Step};
use argonauts_testkit::fake_clis::scenario::Invocation;
use argonauts_testkit::{FakeKind, FakeModelServer, Pty, PtyCommand, argonauts_bin};

use crate::drawings_support::{bring_with, copy_folder, playing_first, reach};

/// The home of the label map's drawings, whose records hold the actions to summarize.
const HOME: &str = "homes/behaviors-label-map";

/// The first words of every summary request.
const PROMPT: &str = "You label what a coding agent was trying to do";

/// What both fakes answer each request with: a line for its first action.
const REPLY: &str = "1. Write a short notes document";

/// Claude Code's stream-json and Codex's JSON lines for a summary request, ahead of the default
/// agents' rules.
fn summary_rules() -> String {
    let reply = REPLY;
    format!(
        r#"
[[rule]]
argv0 = "claude"
args_contain = ["--session-id"]
prompt_contains = '{PROMPT}'
stdin_prompt = "line"
[[rule.step]]
emit = '{{"type":"system","subtype":"init","session_id":"sess-summaries","cwd":".","tools":[],"model":"claude-haiku-5-5"}}'
[[rule.step]]
emit = '{{"type":"assistant","parent_tool_use_id":null,"session_id":"sess-summaries","message":{{"id":"msg-summaries","content":[{{"type":"text","text":"{reply}"}}]}}}}'
[[rule.step]]
emit = '{{"type":"result","subtype":"success","is_error":false,"session_id":"sess-summaries","result":"{reply}","total_cost_usd":0.0001,"usage":{{"input_tokens":12,"output_tokens":8}}}}'
[[rule.step]]
exit = 0

[[rule]]
argv0 = "codex"
args_contain = ["exec"]
prompt_contains = '{PROMPT}'
stdin_prompt = "all"
[[rule.step]]
emit = '{{"type":"thread.started","thread_id":"thread-summaries"}}'
[[rule.step]]
emit = '{{"type":"turn.started"}}'
[[rule.step]]
emit = '{{"type":"item.completed","item":{{"id":"item_0","type":"agent_message","text":"{reply}"}}}}'
[[rule.step]]
emit = '{{"type":"turn.completed","usage":{{"input_tokens":12,"cached_input_tokens":0,"output_tokens":8,"reasoning_output_tokens":0}}}}'
[[rule.step]]
exit = 0
"#
    )
}

/// The fakes and the drawings' records, with `summarizer` named and the summaries placed by
/// their words, so nothing but the fake CLI is asked.
struct Summaries {
    scenario: Scenario,
    _rules: tempfile::TempDir,
}

fn summaries_by(summarizer: &str) -> anyhow::Result<Summaries> {
    let rules = tempfile::tempdir().context("making a folder for the fakes' rules")?;
    let scenario = playing_first(&summary_rules(), rules.path())?;
    let home = approved::dir().join(HOME).join("argonauts");
    copy_folder(&home, scenario.repo.home())?;
    // Without a map yet, nothing asks to make it again.
    fs::remove_file(scenario.repo.home().join("behaviors/layout.json"))
        .context("removing the map")?;
    let config = scenario.repo.home().join("config.toml");
    let mut settings = fs::read_to_string(&config).context("reading the home's config")?;
    settings.push_str(&format!(
        "\n[behaviors]\nsummarizer = \"{summarizer}\"\nembedder = \"words\"\n"
    ));
    fs::write(&config, settings).context("writing the home's config")?;
    Ok(Summaries {
        scenario,
        _rules: rules,
    })
}

/// Runs the command at a terminal, says yes to sending the actions, and returns what it printed
/// and its exit code.
fn summarize(scenario: &Scenario) -> anyhow::Result<(String, u32)> {
    let binary = argonauts_bin().context("finding the argonauts binary")?;
    let mut pty = Pty::spawn(
        PtyCommand::new(binary)
            .args(no_servers_of_yours(&["behaviors", "--summaries"]))
            .cwd(scenario.repo.root())
            .envs(scenario.envs())
            .env_remove("ARGONAUTS_SESSION")
            .size(40, 177),
    )?;
    pty.wait_for(r"Summarize with .*\[y/N\]", WAIT)?;
    pty.send("y\r")?;
    // The CLIs' probes and the sandbox's self-test come first, which a busy machine takes a
    // while over.
    let code = pty.wait_for_exit(WAIT * 3)?;
    Ok((pty.screen().text(), code))
}

/// How many summaries the command says it wrote now.
fn summarized_now(said: &str) -> Option<usize> {
    let (before, _) = said.split_once(" summarized now")?;
    before
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// The summary sessions the fake `program` was started for, each in its own empty folder.
fn summary_sessions(scenario: &Scenario, program: &str) -> anyhow::Result<Vec<Invocation>> {
    Ok(scenario
        .clis
        .invocations()?
        .into_iter()
        .filter(|call| call.argv0 == program)
        .filter(|call| {
            call.cwd.components().any(|part| {
                part.as_os_str()
                    .to_string_lossy()
                    .starts_with("argonauts-summaries-")
            })
        })
        .collect())
}

/// Whether `args` hold `flag` followed by `value`.
fn has(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

/// `no_tools` is the flag and value that give the session no tools of its own.
fn writes_summaries_with(
    summarizer: &str,
    program: &str,
    no_tools: (&str, &str),
) -> anyhow::Result<()> {
    let summaries = summaries_by(summarizer)?;
    let scenario = &summaries.scenario;
    let (said, code) = summarize(scenario)?;
    assert!(!said.contains("stopped"), "{said}");
    let written = summarized_now(&said).with_context(|| format!("no count in:\n{said}"))?;
    assert!(written > 0, "{said}");
    assert_eq!(code, 0, "{said}");
    let sessions = summary_sessions(scenario, program)?;
    assert!(!sessions.is_empty(), "{program} was never asked:\n{said}");
    for session in &sessions {
        // With no network of its own, a session in the sandbox reaches its CLI's servers
        // through the app's proxy.
        assert!(
            session.env.iter().any(|name| name == "HTTPS_PROXY"),
            "{program} ran outside the sandbox: {:?}",
            session.env
        );
        let (flag, value) = no_tools;
        assert!(has(&session.args, flag, value), "{:?}", session.args);
    }
    Ok(())
}

#[test]
fn claude_code_writes_summaries_from_inside_the_sandbox() -> anyhow::Result<()> {
    writes_summaries_with("claude-code/haiku", "claude", ("--tools", ""))
}

#[test]
fn codex_writes_summaries_from_inside_the_sandbox() -> anyhow::Result<()> {
    writes_summaries_with("codex/gpt-6-luna", "codex", ("--disable", "shell_tool"))
}

/// The label map's drawing, whose steps open the Behaviors map over the drawings' records.
const LABEL_MAP: &str = "behaviors/label-map@177x40.txt";

/// A large monitor's window, where the line under the headline has the map's whole width
/// whichever model the questions offer this machine first, so it holds the reason whole.
const LARGE: (u16, u16) = (265, 60);

/// The tab's own update with a model to download from Open models, on a fake Ollama that
/// answers a download with a 404: the update stops, and the line under the headline says the
/// model didn't download and why.
#[test]
fn an_update_whose_model_didnt_download_says_why_on_the_tab() -> anyhow::Result<()> {
    let ollama = FakeModelServer::start(FakeKind::Ollama, &["llama3.2:latest"])?;
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let mut entry = manifest
        .drawings
        .iter()
        .find(|entry| entry.file == LABEL_MAP)
        .with_context(|| format!("no {LABEL_MAP}"))?
        .clone();
    // As far as the map, without pointing at it.
    let map_at = entry
        .steps
        .iter()
        .position(|step| matches!(step, Step::Wait(words) if words.starts_with("Label map")))
        .context("a wait for the label map")?;
    entry.steps.truncate(map_at + 1);
    let mut drawing = Drawing::load(&dir.join(&entry.file))?;
    (drawing.cols, drawing.rows) = LARGE;
    let ollama_url = ollama.base_url();
    let at = bring_with(&entry, &dir, &drawing, |scenario| {
        fs::remove_file(scenario.repo.home().join("behaviors/layout.json"))
            .context("removing the map")?;
        let config = scenario.repo.home().join("config.toml");
        let mut settings = fs::read_to_string(&config).context("reading the home's config")?;
        settings.push_str(&format!(
            "\n[behaviors]\nembedder = \"words\"\n\n\
             [endpoints.ollama]\nbase_url = \"{ollama_url}\"\nlocal = true\n"
        ));
        fs::write(&config, settings).context("writing the home's config")
    })?;
    let mut pty = at.pty;
    // Llama 3.2 is on the fake Ollama, and the models Open models lists after it would be
    // downloaded; which ones depends on this machine's memory, so the first is taken. The
    // fake's tags carry no sizes, so the embedder asks to download too, and word vectors
    // keep it from that.
    let steps = [
        Step::Press("d".to_owned()),
        Step::Wait("Everything: ".to_owned()),
        Step::Press("1".to_owned()),
        Step::Wait("Summarize with".to_owned()),
        Step::Press("2".to_owned()),
        Step::Wait("one not installed asks before it downloads".to_owned()),
        Step::Press("2".to_owned()),
        Step::Wait("Yes, download it and summarize".to_owned()),
        Step::Press("Enter".to_owned()),
        Step::Wait("Use word vectors instead".to_owned()),
        Step::Press("2".to_owned()),
    ];
    reach(&mut pty, &steps)?;
    let screen = pty.wait_for_visible("Update stopped: ", WAIT)?;
    let rows = screen.visible;
    let at = rows
        .iter()
        .position(|row| row.contains("Update stopped: "))
        .context("the stopped line")?;
    let stopped = rows[at].trim();
    assert!(
        stopped.starts_with("Update stopped: ")
            && stopped.ends_with(" didn't download (Ollama answered 404 Not Found)."),
        "{stopped}"
    );
    assert_eq!(rows[at + 1].trim(), "The map is as it was.");
    let pulled = ollama
        .requests()
        .iter()
        .filter(|request| request.path == "/api/pull")
        .count();
    assert_eq!(pulled, 1);
    Ok(())
}
