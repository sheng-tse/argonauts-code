//! `--format json` events to agent events. Anything it doesn't know stays as `Unknown`.
//!
//! OpenCode 1.18.33 writes one `{type, timestamp, sessionID, ...}` object per line: `step_start`,
//! `step_finish`, `text`, `reasoning` (with `--thinking`), `tool_use` (once a tool completes or
//! fails), and `error`. It sends no final result event and names no model, so `finish` builds
//! the result from the last text.
//!
//! OpenCode 2.0.26 leaves the last step's `step_finish` out of the stream, so the answer's
//! last step goes uncounted and a one-step answer counts nothing. When a turn ends with more
//! steps begun than finished, the session's own export (local, no model call) gives the
//! tokens of the steps it missed.

use argonauts_core::patch_files::patched_files;
use argonauts_core::{AgentEvent, CommandSpec, ErrorCategory, StreamParser};
use serde_json::Value;

use crate::private_server::STANDALONE_FLAG;

#[derive(Debug, Default)]
pub(crate) struct OpenCodeParser {
    started: bool,
    last_text: Option<String>,
    failed: bool,
    /// The last `Error: ...` line on stderr, which is all 1.18.34 says when it stops before a
    /// session starts, such as for a session id it doesn't know.
    cli_error: Option<String>,
    session: Option<String>,
    steps_begun: u32,
    steps_counted: u32,
}

impl StreamParser for OpenCodeParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return vec![AgentEvent::unknown_line(line)];
        };
        let Some(kind) = event["type"].as_str() else {
            return vec![AgentEvent::unknown_line(line)];
        };
        let mut out = Vec::new();
        if !self.started
            && let Some(session) = event["sessionID"].as_str()
        {
            self.started = true;
            self.session = Some(session.to_owned());
            out.push(AgentEvent::Init {
                session_id: Some(session.to_owned()),
                model: None,
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            });
        }
        let part = &event["part"];
        let parsed = match kind {
            "step_start" => {
                self.steps_begun += 1;
                Some(Vec::new())
            }
            "step_finish" => {
                self.steps_counted += 1;
                step_finish(part)
            }
            "text" => part["text"].as_str().map(|text| {
                self.last_text = Some(text.to_owned());
                vec![AgentEvent::Message {
                    text: text.to_owned(),
                    parent: None,
                }]
            }),
            "reasoning" => part["text"].as_str().map(|text| {
                vec![AgentEvent::Reasoning {
                    text: text.to_owned(),
                }]
            }),
            "tool_use" => tool_use(part),
            "error" => {
                self.failed = true;
                Some(vec![error(&event["error"])])
            }
            _ => None,
        };
        match parsed {
            Some(events) => out.extend(events),
            None => out.push(AgentEvent::Unknown { raw: event }),
        }
        out
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        if let Some(message) = without_ansi(line).strip_prefix("Error: ") {
            self.cli_error = Some(message.trim().to_owned());
        }
        vec![AgentEvent::unknown_line(line)]
    }

    /// The session's export, when a step went uncounted: `opencode session export` on the
    /// session's private server on OpenCode 2, `opencode export` on 1.x, which has no
    /// service.
    fn after_end(&mut self, command: &CommandSpec) -> Option<CommandSpec> {
        if self.steps_begun <= self.steps_counted {
            return None;
        }
        let session = self.session.as_deref()?;
        let private = command.args.iter().any(|arg| arg == STANDALONE_FLAG);
        let args: &[&str] = if private {
            &["session", "export", STANDALONE_FLAG, session]
        } else {
            &["export", session]
        };
        Some(CommandSpec {
            program: command.program.clone(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env_set: command.env_set.clone(),
            env_remove: command.env_remove.clone(),
            cwd: command.cwd.clone(),
            ..CommandSpec::default()
        })
    }

    fn read_after_end(&mut self, stdout: Option<&str>) -> Vec<AgentEvent> {
        let Some(export) = stdout.and_then(|json| serde_json::from_str::<Value>(json).ok()) else {
            return Vec::new();
        };
        let missed = uncounted_steps(&export, self.steps_counted);
        self.steps_counted += u32::try_from(missed.len()).unwrap_or(u32::MAX);
        missed
            .into_iter()
            .filter_map(step_finish)
            .flatten()
            .collect()
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let exited_ok = exit_code == Some(0);
        if !exited_ok && !self.failed {
            let mut message = match exit_code {
                Some(code) => format!("opencode exited with code {code}"),
                None => "opencode was stopped by a signal".to_owned(),
            };
            if let Some(why) = self.cli_error.take() {
                message = format!("{message}: {why}");
            }
            out.push(AgentEvent::Error {
                category: ErrorCategory::Cli,
                message,
                retryable: false,
            });
        }
        out.push(AgentEvent::Result {
            text: self.last_text.take(),
            structured: None,
            is_error: self.failed || !exited_ok,
        });
        out
    }
}

/// The steps of the export's last turn past the first `counted`, each with its tokens. A turn
/// starts at its prompt, and each step is one answer of the model's: on 2.x a message of
/// `type` `assistant` with its `tokens`, on 1.x a message whose `info` has the `role` and the
/// tokens.
fn uncounted_steps(export: &Value, counted: u32) -> Vec<&Value> {
    let Some(messages) = export["messages"].as_array() else {
        return Vec::new();
    };
    let role = |message: &Value| {
        message["info"]["role"]
            .as_str()
            .or(message["type"].as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let turn = messages
        .iter()
        .rposition(|message| role(message) == "user")
        .map_or(messages.as_slice(), |prompt| &messages[prompt + 1..]);
    turn.iter()
        .filter(|message| role(message) == "assistant")
        .map(|message| {
            message
                .get("info")
                .filter(|info| info.is_object())
                .unwrap_or(message)
        })
        .filter(|step| step["tokens"].is_object())
        .skip(usize::try_from(counted).unwrap_or(usize::MAX))
        .collect()
}

/// OpenCode stores `input` without cache reads and writes, and `output` without reasoning, so
/// both are added back for Argonauts Code's totals. Each step's numbers add to the last.
fn step_finish(part: &Value) -> Option<Vec<AgentEvent>> {
    let tokens = &part["tokens"];
    let count = |value: &Value| value.as_f64().map_or(0, |n| n.max(0.0) as u64);
    let input = tokens["input"].as_f64()?;
    let cache_read = count(&tokens["cache"]["read"]);
    let cache_write = count(&tokens["cache"]["write"]);
    let input_tokens = input.max(0.0) as u64 + cache_read + cache_write;
    let output_tokens = count(&tokens["output"]) + count(&tokens["reasoning"]);
    let used = tokens["total"]
        .as_f64()
        .map_or(input_tokens + output_tokens, |n| n.max(0.0) as u64);
    Some(vec![
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            cached_tokens: cache_read,
            cost_usd: part["cost"].as_f64(),
            cumulative: false,
        },
        AgentEvent::ContextFill {
            used,
            size: None,
            sent: None,
        },
    ])
}

/// OpenCode reports a tool once, when it has finished, so both events come together.
fn tool_use(part: &Value) -> Option<Vec<AgentEvent>> {
    let id = part["callID"].as_str().or(part["id"].as_str())?.to_owned();
    let name = part["tool"].as_str()?.to_owned();
    let state = &part["state"];
    let (ok, output) = match state["status"].as_str()? {
        "completed" => (true, state["output"].as_str()),
        "error" => (false, state["error"].as_str()),
        _ => return None,
    };
    let changed = if ok {
        file_changes(&name, state)
    } else {
        Vec::new()
    };
    let mut events = vec![
        AgentEvent::ToolStarted {
            id: id.clone(),
            name,
            input: state["input"].clone(),
            parent: None,
        },
        AgentEvent::ToolFinished {
            id,
            ok,
            output: output.map(str::to_owned),
            parent: None,
        },
    ];
    events.extend(changed);
    Some(events)
}

/// OpenCode's tools that write a file, which name it in `filePath`.
const FILE_TOOLS: &[&str] = &["write", "edit", "multiedit"];

/// OpenCode 2.0.32's name for its apply_patch tool, which names its files in `patchText`.
const PATCH_TOOL: &str = "patch";

/// The files a finished edit tool wrote, as the other CLIs' adapters report theirs. It's how a
/// new file is known to be the agent's own work and not something a command left behind.
fn file_changes(tool: &str, state: &Value) -> Vec<AgentEvent> {
    if tool == PATCH_TOOL {
        let patch = state["input"]["patchText"].as_str().unwrap_or_default();
        return patched_files(patch)
            .into_iter()
            .map(|(path, kind)| AgentEvent::FileChanged {
                path,
                kind: kind.to_owned(),
            })
            .collect();
    }
    if !FILE_TOOLS.contains(&tool) {
        return Vec::new();
    }
    let Some(path) = state["input"]["filePath"].as_str() else {
        return Vec::new();
    };
    let kind = match state["metadata"]["exists"].as_bool() {
        Some(false) => "add",
        _ => "update",
    };
    vec![AgentEvent::FileChanged {
        path: path.to_owned(),
        kind: kind.to_owned(),
    }]
}

/// OpenCode's named errors, `{name, data: {message, ...}}`.
fn error(error: &Value) -> AgentEvent {
    let name = error["name"].as_str().unwrap_or("UnknownError");
    let data = &error["data"];
    let message = data["message"]
        .as_str()
        .map_or_else(|| name.to_owned(), str::to_owned);
    let status = data["statusCode"].as_u64();
    let category = match name {
        "ProviderAuthError" => ErrorCategory::Auth,
        "ContextOverflowError" | "MessageOutputLengthError" => ErrorCategory::ContextFull,
        "MessageAbortedError" => ErrorCategory::Interrupted,
        "ContentFilterError" => ErrorCategory::InvalidRequest,
        "APIError" => status.map_or(ErrorCategory::Unknown, category_for_status),
        _ => ErrorCategory::Unknown,
    };
    AgentEvent::Error {
        category,
        message,
        retryable: data["isRetryable"].as_bool().unwrap_or(false),
    }
}

fn category_for_status(status: u64) -> ErrorCategory {
    match status {
        401 | 403 => ErrorCategory::Auth,
        402 => ErrorCategory::Billing,
        404 => ErrorCategory::ModelUnavailable,
        408 => ErrorCategory::Timeout,
        413 => ErrorCategory::ContextFull,
        429 => ErrorCategory::RateLimit,
        503 | 529 => ErrorCategory::Overloaded,
        400..=499 => ErrorCategory::InvalidRequest,
        500..=599 => ErrorCategory::Server,
        _ => ErrorCategory::Unknown,
    }
}

/// `line` without its color codes (`ESC [ ... letter`).
fn without_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAST_STEPS: &str = include_str!("../fixtures/export_last_steps.stdout.jsonl");
    const LAST_STEPS_EXPORT: &str = include_str!("../fixtures/export_last_steps.export.json");
    const ONE_STEP: &str = include_str!("../fixtures/export_one_step.stdout.jsonl");
    const ONE_STEP_EXPORT: &str = include_str!("../fixtures/export_one_step.export.json");
    const SESSION: &str = "ses_eae3d05b5ffeH7KfC0JkWTEV2E";

    fn read(stdout: &str) -> (OpenCodeParser, Vec<AgentEvent>) {
        let mut parser = OpenCodeParser::default();
        let events = stdout
            .lines()
            .flat_map(|line| parser.parse_stdout_line(line))
            .collect();
        (parser, events)
    }

    /// The session's command, on a private server or not.
    fn session_command(private: bool) -> CommandSpec {
        let mut command =
            CommandSpec::new("/opt/opencode", "/tmp/repo").args(["run", "--format", "json"]);
        if private {
            command = command.arg(STANDALONE_FLAG);
        }
        command
            .env_set
            .push(("OPENCODE_DISABLE_AUTOUPDATE".to_owned(), "1".to_owned()));
        command
    }

    fn usage(events: &[AgentEvent]) -> Vec<(u64, u64)> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => Some((*input_tokens, *output_tokens)),
                _ => None,
            })
            .collect()
    }

    /// OpenCode 2.0.32 names its apply_patch tool `patch`, and names the files in the patch.
    #[test]
    fn a_patch_names_every_file_it_changed() {
        let patch = "*** Begin Patch\n*** Update File: slug.py\n@@\n-a\n+b\n\
                     *** Add File: tests/test_slug.py\n+import slug\n*** End Patch";
        let line = serde_json::json!({
            "type": "tool_use",
            "part": {
                "type": "tool",
                "tool": "patch",
                "callID": "call_p1",
                "state": {"status": "completed", "input": {"patchText": patch}, "output": "Done"},
            },
        });
        let (_, events) = read(&line.to_string());
        let changed: Vec<(&str, &str)> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::FileChanged { path, kind } => Some((path.as_str(), kind.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            changed,
            [("slug.py", "update"), ("tests/test_slug.py", "add")]
        );
    }

    /// Three steps begun and two finished: the export gives the third's tokens.
    #[test]
    fn the_step_the_stream_left_out_is_read_from_the_export() {
        let (mut parser, events) = read(LAST_STEPS);
        assert_eq!(usage(&events), [(7_757, 113), (7_910, 597)]);
        let export = parser.after_end(&session_command(true)).expect("an export");
        assert_eq!(export.program, "/opt/opencode");
        assert_eq!(export.args, ["session", "export", "--standalone", SESSION]);
        assert_eq!(export.cwd, std::path::Path::new("/tmp/repo"));
        assert_eq!(export.env_set, session_command(true).env_set);

        let missed = parser.read_after_end(Some(LAST_STEPS_EXPORT));
        assert_eq!(usage(&missed), [(8_526, 120)]);
        assert!(missed.contains(&AgentEvent::ContextFill {
            used: 8_646,
            size: None,
            sent: None,
        }));
        // Counted now, so nothing is asked again.
        assert_eq!(parser.after_end(&session_command(true)), None);
    }

    /// A one-step answer prints no step's tokens at all.
    #[test]
    fn a_one_step_answer_counts_in_full() {
        let (mut parser, events) = read(ONE_STEP);
        assert!(usage(&events).is_empty());
        let export = parser
            .after_end(&session_command(false))
            .expect("an export");
        assert_eq!(export.args, ["export", SESSION]);
        assert_eq!(
            usage(&parser.read_after_end(Some(ONE_STEP_EXPORT))),
            [(8_719, 5)]
        );
    }

    #[test]
    fn an_export_that_failed_or_says_nothing_adds_nothing() {
        let (mut parser, _) = read(ONE_STEP);
        assert!(parser.read_after_end(None).is_empty());
        assert!(parser.read_after_end(Some("not json")).is_empty());
        assert!(parser.read_after_end(Some(r#"{"messages":[]}"#)).is_empty());
        // Every step counted, or no session: nothing to fetch.
        let (mut whole, _) = read(&LAST_STEPS.replace("\"step_start\"", "\"patch\""));
        assert_eq!(whole.after_end(&session_command(true)), None);
        assert_eq!(
            OpenCodeParser::default().after_end(&session_command(true)),
            None
        );
    }

    /// OpenCode 1.x keeps a message's role and tokens under its `info`.
    #[test]
    fn an_older_export_keeps_its_tokens_under_info() {
        let export = serde_json::json!({"messages": [
            {"info": {"role": "user"}, "parts": []},
            {"info": {"role": "assistant", "tokens": {"input": 100, "output": 7}}, "parts": []},
            {"info": {"role": "assistant", "tokens": {"input": 140, "output": 9}}, "parts": []},
        ]});
        let steps = uncounted_steps(&export, 1);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["tokens"]["input"], 140);
    }

    #[test]
    fn unknown_types_and_plain_lines_are_kept() {
        let mut parser = OpenCodeParser::default();
        let events = parser.parse_stdout_line(r#"{"type":"patch","sessionID":"ses_1"}"#);
        assert!(matches!(events[0], AgentEvent::Init { .. }));
        assert!(matches!(events[1], AgentEvent::Unknown { .. }));
        assert!(matches!(
            parser.parse_stdout_line("not json").as_slice(),
            [AgentEvent::Unknown { .. }]
        ));
        let finished = parser.finish(None);
        assert!(matches!(
            finished[0],
            AgentEvent::Error {
                category: ErrorCategory::Cli,
                ..
            }
        ));
        assert!(matches!(
            finished[1],
            AgentEvent::Result { is_error: true, .. }
        ));
    }

    #[test]
    fn api_errors_are_categorized_by_status() {
        let event = error(&serde_json::json!({
            "name": "APIError",
            "data": {"message": "slow down", "statusCode": 429, "isRetryable": true}
        }));
        assert_eq!(
            event,
            AgentEvent::Error {
                category: ErrorCategory::RateLimit,
                message: "slow down".to_owned(),
                retryable: true,
            }
        );
        assert_eq!(category_for_status(500), ErrorCategory::Server);
        assert_eq!(category_for_status(400), ErrorCategory::InvalidRequest);
    }
}
