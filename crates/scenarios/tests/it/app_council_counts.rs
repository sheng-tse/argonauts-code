//! The talk's counts on the real app, with seats that read files and report a large context
//! each turn, as real CLIs do: the talk prints no agent rows, the working line counts the
//! tokens and the seats that agree as the run's row under the input does, and the points still
//! open show under it. Every Claude Code session a seat starts runs the seat's model.

use std::fs;
use std::path::Path;

use anyhow::Context as _;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::session::SessionEvent;
use argonauts_testkit::approved::{self, Drawing, Entry, Manifest};
use regex::Regex;

use crate::drawings_support::bring;

/// The Claude Code seat's model, set by its alias as you'd write it.
const SEAT_MODEL: &str = "claude-code/haiku";

/// What a turn that read a whole folder reports as its context, past the talk's 200k limit.
const CONTEXT: &str = "373000";

/// A Read tool call and its result, played before a Claude Code seat's first answer.
const READ: &str = r#"[[rule.step]]
emit = '{"type":"assistant","parent_tool_use_id":null,"session_id":"sess-seat-claude","message":{"id":"msg-seat-read","content":[{"type":"tool_use","id":"toolu_seat_read","name":"Read","input":{"file_path":"src/server.ts"}}]}}'
[[rule.step]]
emit = '{"type":"user","parent_tool_use_id":null,"session_id":"sess-seat-claude","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_seat_read","content":"export const server = createServer();"}]}}'
"#;

/// The prompt words of each kind of seat session the talk's fakes play, and the kind's name.
const SEAT_SESSIONS: [(&str, &str); 4] = [
    ("Write your first answer", "the first answer"),
    ("You keep the council", "the note keeper's list"),
    ("A point changed. React", "a reaction"),
    ("your turn to write to the council", "a turn to write"),
];

/// The talk drawing's fakes, with every seat turn reporting [`CONTEXT`] tokens read and the
/// Claude Code seat's first answer reading a file first.
fn reading_rules(approved: &str) -> String {
    let result = r#"[[rule.step]]
emit = "{\"type\":\"result\""#;
    let rules: Vec<String> = approved
        .split("\n[[rule]]")
        .map(|rule| {
            let first_answer = rule.contains("argv0 = \"claude\"")
                && rule.contains("prompt_contains = 'Write your first answer'");
            if first_answer {
                rule.replacen(result, &format!("{READ}{result}"), 1)
            } else {
                rule.to_owned()
            }
        })
        .collect();
    rules
        .join("\n[[rule]]")
        .replace(
            r#"input_tokens\":1200"#,
            &format!(r#"input_tokens\":{CONTEXT}"#),
        )
        .replace(r#"{\"input\":1200,"#, &format!(r#"{{\"input\":{CONTEXT},"#))
        .replace("claude-opus-5-5", "claude-haiku-5-5")
}

/// The talk drawing's steps, on its fakes as [`reading_rules`] changes them and its home with
/// the Claude Code seat on [`SEAT_MODEL`], on the full screen or the inline one.
fn talk(inline: bool, scratch: &Path) -> anyhow::Result<(Entry, Drawing)> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let mut entry = manifest
        .drawings
        .iter()
        .find(|entry| entry.file == "council/talk@120x38.txt")
        .context("the talk drawing")?
        .clone();
    let approved = Drawing::load(&dir.join(&entry.file))?;

    let clis = dir.join(entry.clis.as_deref().context("the talk's fakes")?);
    let rules = reading_rules(&fs::read_to_string(&clis)?);
    let rules_file = scratch.join("talk.toml");
    fs::write(&rules_file, rules)?;
    entry.clis = Some(rules_file.to_string_lossy().into_owned());

    let own = dir.join(entry.home.as_deref().context("the talk's home")?);
    let config = fs::read_to_string(own.join("argonauts/config.toml"))?;
    let seated = format!("model = \"{SEAT_MODEL}\"");
    let mut config = config.replace("model = \"claude-code/claude-opus-5-5\"", &seated);
    anyhow::ensure!(
        config.contains(&seated),
        "the talk's home seats no Claude Code"
    );
    if inline {
        config.push_str("\n[ui]\nscreen = \"inline\"\n");
    }
    let home = scratch.join("home");
    fs::create_dir_all(home.join("argonauts"))?;
    fs::write(home.join("argonauts/config.toml"), config)?;
    entry.home = Some(home.to_string_lossy().into_owned());
    Ok((entry, approved))
}

/// The working line's tokens and seats that agree, the run row's, and the row under the
/// working line, from the screen's text.
struct Counts {
    line_tokens: String,
    line_agree: String,
    row_tokens: String,
    row_agree: String,
    under_line: String,
}

fn counts(text: &str) -> anyhow::Result<Counts> {
    let working =
        Regex::new(r"Parleying…\s*\(.*↓\s+(\S+) of \S+ tokens · (\d+/\d+) seats agree\)")?;
    let run_row = Regex::new(r"(\d+/\d+) · \S+ · ↓\s+(\S+) tokens")?;
    let lines: Vec<&str> = text.lines().collect();
    let (at, line) = lines
        .iter()
        .enumerate()
        .find_map(|(at, line)| Some((at, working.captures(line)?)))
        .context("no working line with its tokens and seats")?;
    let row = lines
        .iter()
        .find_map(|line| run_row.captures(line))
        .context("no run row with its seats and tokens")?;
    Ok(Counts {
        line_tokens: line[1].to_owned(),
        line_agree: line[2].to_owned(),
        row_tokens: row[2].to_owned(),
        row_agree: row[1].to_owned(),
        under_line: lines.get(at + 1).map_or("", |row| row.trim()).to_owned(),
    })
}

/// Every Claude Code session a seat started, by kind, with the model its command line names.
fn seat_models(
    at: &crate::drawings_support::AtScreen,
) -> anyhow::Result<Vec<(&'static str, String)>> {
    let mut seen = Vec::new();
    for call in at.scenario.clis.invocations()? {
        if call.argv0 != "claude" {
            continue;
        }
        let prompt = call.stdin.as_deref().unwrap_or_default();
        let Some((_, kind)) = SEAT_SESSIONS
            .iter()
            .find(|(words, _)| prompt.contains(words))
        else {
            continue;
        };
        let model = call
            .args
            .iter()
            .position(|arg| arg == "--model")
            .and_then(|flag| call.args.get(flag + 1))
            .cloned()
            .unwrap_or_else(|| format!("no --model in {:?}", call.args));
        seen.push((*kind, model));
    }
    Ok(seen)
}

/// Every session the run started: its label, its worker and the seat it speaks for.
fn sessions(
    at: &crate::drawings_support::AtScreen,
) -> anyhow::Result<Vec<(String, String, String)>> {
    let mut started = Vec::new();
    let mut logs = vec![at.scenario.repo.home().to_owned()];
    while let Some(dir) = logs.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                logs.push(path);
            } else if path.file_name().is_some_and(|name| name == "events.jsonl") {
                let run = path.parent().context("a run folder")?;
                for event in argonauts_testkit::home::journal::events(run)? {
                    if let RunEventKind::Session(SessionEvent::Started(session)) = event.kind {
                        started.push((
                            session.label.clone().unwrap_or_default(),
                            session.worker.to_string(),
                            session
                                .seat
                                .map(|seat| seat.to_string())
                                .unwrap_or_default(),
                        ));
                    }
                }
            }
        }
    }
    Ok(started)
}

/// A talk like a real one, on fakes shaped like real CLIs, at both sizes on both screens. On
/// counts of what the CLIs read, its first answers alone would pass the 200k limit and stop it.
#[test]
fn the_talks_counts_agree_and_every_seat_runs_its_model() -> anyhow::Result<()> {
    let dir = approved::dir();
    for inline in [false, true] {
        let screen = if inline { "inline" } else { "full" };
        for (cols, rows) in [(120, 38), (177, 40)] {
            let scratch = tempfile::tempdir().context("making a folder for the talk's fakes")?;
            let (entry, approved) = talk(inline, scratch.path())?;
            let mut drawing = approved;
            (drawing.cols, drawing.rows) = (cols, rows);
            let at = bring(&entry, &dir, &drawing)?;
            let text = at.pty.screen().visible_text();
            let place = format!("on the {screen} screen at {cols}x{rows}:\n{text}");

            for row in ["Done (", "talk:", "tool use"] {
                assert!(!text.contains(row), "an agent row in the talk {place}");
            }
            let counts = counts(&text).with_context(|| place.clone())?;
            assert_eq!(
                counts.line_tokens, counts.row_tokens,
                "the working line and the run row count other tokens {place}"
            );
            assert_eq!(
                counts.line_agree, counts.row_agree,
                "the working line and the run row count other seats {place}"
            );
            assert!(
                counts.under_line.starts_with("⎿  Still open:"),
                "the row under the working line is {:?} {place}",
                counts.under_line
            );

            let sessions = sessions(&at)?;
            let seated = sessions.iter().filter(|(_, _, seat)| !seat.is_empty());
            for (label, worker, seat) in seated {
                assert!(
                    label.starts_with("talk:") && label != "talk:1",
                    "{seat} on {worker} is labeled {label}: {sessions:?}"
                );
            }
            assert!(
                sessions
                    .iter()
                    .filter(|(_, worker, _)| worker.starts_with("claude-code/"))
                    .all(|(_, worker, _)| worker == SEAT_MODEL),
                "a Claude Code session of the talk on another model: {sessions:?}"
            );
            let models = seat_models(&at)?;
            // The note keeper and the seat given the floor are drawn for each run, so only
            // the first answer is sure to come from the Claude Code seat.
            assert!(
                models.iter().any(|(kind, _)| *kind == SEAT_SESSIONS[0].1),
                "no first answer from the Claude Code seat {screen} at {cols}x{rows}: {models:?}"
            );
            assert!(
                models.iter().all(|(_, model)| model == "haiku"),
                "a Claude Code seat session ran another model than its seat's: {models:?}"
            );
        }
    }
    Ok(())
}
