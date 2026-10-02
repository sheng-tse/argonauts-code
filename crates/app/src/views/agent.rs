use argonauts_core::events::card::CardEvent;
use argonauts_core::events::session::SessionEnd;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::SessionKey;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::steps::{is_own_tool, last_json_object};
use argonauts_core::views::{ActivityLine, AgentView, SubagentsSection};
use argonauts_core::{DiffStat, RunId};
use argonauts_tui::bridge::tools::start_call;
use argonauts_tui::cell::ToolTitle;
use serde_json::Value;
use std::path::Path;

use super::sessions::{RunFacts, SessionFacts, ToolCall};

/// The latest tool calls sent to the view, which shows the last few until expanded.
const ACTIVITY_LINES: usize = 50;
/// Characters of a tool call's input kept on its one line.
const SUMMARY_CHARS: usize = 120;
/// Input fields that say what a call does, most telling first.
const SUMMARY_KEYS: &[&str] = &[
    "command",
    "file_path",
    "path",
    "pattern",
    "query",
    "url",
    "description",
    "prompt",
];

/// Report lists the Outcome shows under the summary, with their titles: what the agent wants
/// you to read. The files and commands are in the Diff and Activity already.
const REPORT_LISTS: &[(&str, &str)] = &[("concerns", "Concerns"), ("questions", "Questions")];

/// One session's Prompt, Activity, Diff, Subagents, and Outcome. The report tools are how the
/// agent hands back its result, which the Outcome shows, so Activity leaves them out.
pub(crate) fn agent_view(
    events: &[RunEvent],
    run: &RunId,
    session: &SessionKey,
) -> Option<AgentView> {
    let facts = RunFacts::of(events);
    let this = facts.session(session)?;
    let own_calls = this
        .tools
        .iter()
        .filter(|call| is_own_tool(&call.name))
        .count();
    let main_calls: Vec<&ToolCall> = this
        .tools
        .iter()
        .filter(|call| call.parent.is_none() && !is_own_tool(&call.name))
        .collect();
    let activity = main_calls[main_calls.len().saturating_sub(ACTIVITY_LINES)..]
        .iter()
        .map(|call| {
            let (tool, summary) = titled(&this.started.vendor.0, call, this.cwd.as_deref());
            ActivityLine {
                tool,
                summary,
                ok: call.ok,
            }
        })
        .collect();
    Some(AgentView {
        run: run.clone(),
        row: facts.agent_row(this),
        prompt: this.prompt.clone(),
        activity,
        tool_calls: this
            .tool_calls()
            .saturating_sub(u32::try_from(own_calls).unwrap_or(u32::MAX)),
        diff: diff_of(events, this),
        subagents: subagents_of(this),
        outcome: this.ended.as_ref().and_then(outcome_of),
    })
}

/// What the agent ended with, in its own words: its report as Markdown, else its final text.
/// A report's JSON is for code and the raw view, so it never shows here.
fn outcome_of(end: &SessionEnd) -> Option<String> {
    let texts = || [&end.result_text, &end.last_message].into_iter().flatten();
    let report = end
        .structured
        .clone()
        .filter(Value::is_object)
        .or_else(|| texts().find_map(|text| last_json_object(text)));
    report.as_ref().and_then(report_markdown).or_else(|| {
        texts()
            .map(|text| words_before_report(text))
            .find(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

/// A final message without the JSON report it ends with, fenced or not.
fn words_before_report(text: &str) -> &str {
    let body = text.trim_end();
    let unfenced = body.strip_suffix("```").map_or(body, str::trim_end);
    let report_at = unfenced.match_indices('{').map(|(at, _)| at).find(|&at| {
        serde_json::from_str::<Value>(&unfenced[at..]).is_ok_and(|value| value.is_object())
    });
    let Some(at) = report_at else {
        return text.trim();
    };
    let before = unfenced[..at].trim_end();
    before
        .strip_suffix("```json")
        .or_else(|| before.strip_suffix("```"))
        .unwrap_or(before)
        .trim()
}

/// A report's summary, or a chat step's answer, then the lists a person reads in it.
fn report_markdown(report: &Value) -> Option<String> {
    let text = ["summary", "answer_markdown"]
        .iter()
        .filter_map(|key| report.get(*key)?.as_str())
        .map(str::trim)
        .find(|text| !text.is_empty())?;
    let mut out = text.to_owned();
    for (key, title) in REPORT_LISTS {
        let items: Vec<String> = report
            .get(*key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(list_item)
            .collect();
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("\n\n**{title}**\n"));
        for item in items {
            out.push_str(&format!("\n- {item}"));
        }
    }
    Some(out)
}

/// One list entry as a line: a string, or an object's question, summary, or text.
fn list_item(value: &Value) -> Option<String> {
    let text = value.as_str().or_else(|| {
        ["question", "summary", "text"]
            .iter()
            .find_map(|key| value.get(*key)?.as_str())
    })?;
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then_some(line)
}

/// The card's latest diff. A card with a worktree and no diff yet shows an empty one. While
/// the session runs, what it changed since the last commit counts too; the conductor commits
/// it when the session ends.
fn diff_of(events: &[RunEvent], session: &SessionFacts) -> Option<DiffStat> {
    let card = session.card.as_ref()?;
    let mut diff = None;
    for event in events
        .iter()
        .filter(|event| event.card_id.as_ref() == Some(card))
    {
        match &event.kind {
            RunEventKind::Card(CardEvent::Worktree { .. }) => {
                diff.get_or_insert_with(DiffStat::default);
            }
            RunEventKind::Card(CardEvent::Diff { stat, .. }) => diff = Some(*stat),
            _ => {}
        }
    }
    let mut diff = diff?;
    if session.ended.is_none() && !session.file_diffs.is_empty() {
        // A file changed before the last commit and again since counts once.
        let files = u32::try_from(session.file_diffs.len()).unwrap_or(u32::MAX);
        diff.files_changed = diff.files_changed.max(files);
        for (added, removed) in session.file_diffs.values() {
            diff.insertions += added;
            diff.deletions += removed;
        }
    }
    Some(diff)
}

fn subagents_of(session: &SessionFacts) -> Option<SubagentsSection> {
    if session.subagents.is_empty() {
        return None;
    }
    let threads = session.started.vendor.0 == "codex";
    let hidden_steps = session.subagents.iter().any(|child| !child.steps_visible);
    Some(SubagentsSection {
        title: if threads { "Threads" } else { "Subagents" }.to_owned(),
        note: if hidden_steps {
            "steps not streamed"
        } else {
            "structure not exposed"
        }
        .to_owned(),
        children: session.subagents.clone(),
    })
}

/// The call as the transcript titles it, such as `Bash(ls)` for Codex's `command_execution`,
/// with paths inside the session's folder written from there.
fn titled(vendor: &str, call: &ToolCall, cwd: Option<&Path>) -> (String, String) {
    match start_call(vendor, &call.name, &call.input).title {
        ToolTitle::Call { name, args } => {
            let args = match cwd.map(|cwd| format!("{}/", cwd.display())) {
                Some(prefix) => args.replace(&prefix, ""),
                None => args,
            };
            (sanitize_for_terminal(&name), sanitize_for_terminal(&args))
        }
        _ => (sanitize_for_terminal(&call.name), summarize(&call.input)),
    }
}

/// One sanitized line saying what a call does, such as its command or file.
fn summarize(input: &Value) -> String {
    let text = SUMMARY_KEYS
        .iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| input.as_str().map(str::to_owned))
        .unwrap_or_else(|| match input {
            Value::Null => String::new(),
            other => other.to_string(),
        });
    let clean = sanitize_for_terminal(&text);
    let line = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(SUMMARY_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{
        agent, card_event, input, run_id, session_ended, session_event, session_started,
    };
    use argonauts_core::AgentEvent;
    use argonauts_core::budget::{BudgetAction, BudgetLimit, BudgetStatus};
    use argonauts_core::events::card::{AttemptCause, CardState};
    use argonauts_core::events::session::{
        EndReason, SubagentKind, SubagentStatus, SubagentUpdate,
    };
    use argonauts_core::events::session::{FileDiff, LimitEvent, SessionEvent};
    use argonauts_core::names::{RoleName, WorkerRef};
    use argonauts_core::steps::StepName;
    use argonauts_core::views::RowStatus;
    use argonauts_core::{CardId, Spend};

    fn tool(id: &str, command: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.to_owned(),
            name: "Bash".to_owned(),
            input: serde_json::json!({ "command": command }),
            parent: None,
        }
    }

    #[test]
    fn the_agent_view_shows_prompt_latest_activity_diff_subagents_and_outcome() {
        let card = argonauts_core::CardId::new("card-002").unwrap();
        let key = SessionKey::numbered(1);
        let child = SubagentUpdate {
            id: "thread-1".to_owned(),
            parent: None,
            kind: SubagentKind::Thread,
            label: Some("explorer".to_owned()),
            prompt: None,
            status: SubagentStatus::Running,
            last_message: None,
            log_uri: None,
            steps_visible: false,
        };
        let events = vec![
            session_started(0, &key, Some(&card), RoleName::Owner, "codex"),
            input(1, &key, "Build the export.\n"),
            card_event(
                2,
                &card,
                CardEvent::Worktree {
                    path: "/tmp/wt".into(),
                    branch: "argonauts/run/card-002".to_owned(),
                    base: "abc".to_owned(),
                },
            ),
            agent(3, &key, tool("t1", "ls")),
            agent(
                4,
                &key,
                AgentEvent::ToolFinished {
                    id: "t1".to_owned(),
                    ok: true,
                    output: None,
                    parent: None,
                },
            ),
            agent(5, &key, tool("t2", "cargo build")),
            agent(6, &key, tool("t3", "cargo test\u{1b}[31m --all")),
            agent(7, &key, tool("t4", "git status")),
            agent(
                8,
                &key,
                AgentEvent::Subagent {
                    update: child.clone(),
                },
            ),
            input(9, &key, "Also add --json.\n"),
            session_ended(10, &key, EndReason::Completed, 5_000),
        ];
        let view = agent_view(&events, &run_id(), &key).unwrap();
        assert_eq!(view.prompt, "Build the export.\nAlso add --json.\n");
        assert_eq!(view.tool_calls, 4);
        let lines: Vec<_> = view
            .activity
            .iter()
            .map(|line| line.summary.as_str())
            .collect();
        assert_eq!(
            lines,
            ["ls", "cargo build", "cargo test --all", "git status"]
        );
        assert_eq!(view.diff, Some(DiffStat::default()));
        let subagents = view.subagents.unwrap();
        assert_eq!(subagents.title, "Threads");
        assert_eq!(subagents.note, "steps not streamed");
        assert_eq!(subagents.children, vec![child]);
        assert_eq!(view.row.status, RowStatus::Done);
        assert_eq!(view.row.elapsed_ms, 5_000);
        assert_eq!(view.outcome.as_deref(), Some("done"));
        assert!(agent_view(&events, &run_id(), &SessionKey::numbered(9)).is_none());
    }

    fn owner_started(seq: u64, key: &SessionKey, card: &CardId, worker: &str) -> RunEvent {
        let mut event = session_started(seq, key, Some(card), RoleName::Owner, worker);
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut event.kind {
            started.step = Some(StepName::Build);
        }
        event
    }

    fn called(seq: u64, key: &SessionKey, id: &str, name: &str, input: Value) -> RunEvent {
        agent(
            seq,
            key,
            AgentEvent::ToolStarted {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
                parent: None,
            },
        )
    }

    /// Activity names calls as the transcript does: Codex's shell items read as `Bash`, without
    /// the shell around them, and paths read from the session's folder.
    #[test]
    fn activity_reads_as_the_transcript_names_calls() {
        let card = CardId::new("card-001").unwrap();
        let (codex, claude) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let events = vec![
            owner_started(0, &codex, &card, "codex"),
            input(1, &codex, "Build it.\n"),
            called(
                2,
                &codex,
                "c1",
                "command_execution",
                serde_json::json!({"command": "/bin/zsh -lc 'rg --files | head -50'"}),
            ),
            owner_started(3, &claude, &card, "claude-code"),
            input(4, &claude, "Build it.\n"),
            called(
                5,
                &claude,
                "e1",
                "Edit",
                serde_json::json!({"file_path": "/tmp/src/mathutil.py", "old_string": "a", "new_string": "b"}),
            ),
        ];
        let line = |key: &SessionKey| {
            let view = agent_view(&events, &run_id(), key).unwrap();
            let line = view.activity.last().cloned().unwrap();
            format!("{}({})", line.tool, line.summary)
        };
        assert_eq!(line(&codex), "Bash(rg --files | head -50)");
        assert_eq!(line(&claude), "Update(src/mathutil.py)");
    }

    /// While the owner works, the Diff counts what it changed since the worktree's last
    /// commit, the latest diff of each file; once it ends, the conductor's commit counts.
    #[test]
    fn a_working_owners_diff_counts_its_changes_so_far() {
        let card = CardId::new("card-001").unwrap();
        let key = SessionKey::numbered(1);
        let changed = |seq, path: &str, added, removed| {
            session_event(
                seq,
                &key,
                Some(&card),
                RunEventKind::Session(SessionEvent::FileDiff(FileDiff {
                    path: path.to_owned(),
                    added,
                    removed,
                    hunk: String::new(),
                    truncated: false,
                })),
            )
        };
        let mut events = vec![
            owner_started(0, &key, &card, "claude-code"),
            card_event(
                1,
                &card,
                CardEvent::Worktree {
                    path: "/tmp/wt".into(),
                    branch: "argonauts/run/card-001".to_owned(),
                    base: "abc".to_owned(),
                },
            ),
            changed(2, "a.py", 3, 0),
            changed(3, "b.py", 1, 1),
            changed(4, "a.py", 5, 1),
        ];
        let diff = agent_view(&events, &run_id(), &key).unwrap().diff.unwrap();
        assert_eq!(
            (diff.files_changed, diff.insertions, diff.deletions),
            (2, 6, 2)
        );

        let committed = DiffStat {
            files_changed: 2,
            insertions: 6,
            deletions: 2,
        };
        events.push(session_ended(5, &key, EndReason::Completed, 1_000));
        events.push(card_event(
            6,
            &card,
            CardEvent::Diff {
                stat: committed,
                path: "diffs/card.diff".into(),
            },
        ));
        assert_eq!(
            agent_view(&events, &run_id(), &key).unwrap().diff,
            Some(committed)
        );
    }

    /// A stream with no dollars, as Claude Code's is until it ends, is priced by the meter, as
    /// the run's budget prices it.
    #[test]
    fn a_working_agent_is_priced_by_its_meter() {
        let key = SessionKey::numbered(1);
        let tokens = Spend {
            input_tokens: 40_000,
            output_tokens: 2_000,
            cost_partial: true,
            ..Spend::default()
        };
        let metered = Spend {
            cost_usd: 0.05,
            cost_partial: false,
            cost_estimated: true,
            ..tokens
        };
        let events = vec![
            session_started(0, &key, None, RoleName::Owner, "claude-code"),
            agent(
                1,
                &key,
                AgentEvent::Usage {
                    input_tokens: tokens.input_tokens,
                    output_tokens: tokens.output_tokens,
                    cached_tokens: 0,
                    cost_usd: None,
                    cumulative: true,
                },
            ),
        ];
        let spent = agent_view(&events, &run_id(), &key).unwrap().row.spent;
        assert!(spent.cost_partial, "no meter reading yet");
        let mut events = events;
        events.push(session_event(
            2,
            &key,
            None,
            RunEventKind::Session(SessionEvent::Limit(LimitEvent::Budget(BudgetStatus {
                level: BudgetAction::Continue,
                limit: BudgetLimit::None,
                spend: metered,
                fraction_used: None,
                work: Default::default(),
                work_limits: Default::default(),
                binding: None,
            }))),
        ));
        let row = agent_view(&events, &run_id(), &key).unwrap().row;
        assert_eq!(row.spent, metered);
        assert_eq!(row.tokens, 42_000);
    }

    /// A retry reads as its attempt, and an attempt that ended as if done but never reported,
    /// so that its card failed, reads as failed.
    #[test]
    fn a_retry_reads_as_its_attempt_and_a_silent_end_as_failed() {
        let card = CardId::new("card-001").unwrap();
        let (first, second) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let attempt = |seq, attempt, cause| {
            card_event(
                seq,
                &card,
                CardEvent::Attempt {
                    attempt,
                    worker: WorkerRef::new("opencode", None),
                    cause,
                },
            )
        };
        let state = |seq, to| {
            card_event(
                seq,
                &card,
                CardEvent::State {
                    from: None,
                    to,
                    reason: String::new(),
                },
            )
        };
        let events = vec![
            attempt(0, 1, AttemptCause::First),
            owner_started(1, &first, &card, "opencode-acp"),
            session_ended(2, &first, EndReason::Completed, 1_000),
            state(3, CardState::Failed),
            attempt(4, 2, AttemptCause::AfterFailure),
            owner_started(5, &second, &card, "opencode"),
            session_ended(6, &second, EndReason::Completed, 1_000),
        ];
        let row = |key: &SessionKey| agent_view(&events, &run_id(), key).unwrap().row;
        assert_eq!(row(&first).label, "owner · card-001");
        assert_eq!(row(&first).status, RowStatus::Failed);
        assert_eq!(row(&second).label, "owner · card-001 · attempt 2");
        assert_eq!(
            row(&second).status,
            RowStatus::Done,
            "its card hasn't moved on yet"
        );

        let mut reported = events.clone();
        reported.push(card_event(
            7,
            &card,
            CardEvent::Reported(Box::new(
                serde_json::from_value(serde_json::json!({
                    "status": "done", "summary": "Fixed add.", "files_changed": [],
                    "tests_run": [], "concerns": []
                }))
                .unwrap(),
            )),
        ));
        reported.push(state(8, CardState::Failed));
        let view = agent_view(&reported, &run_id(), &second).unwrap();
        assert_eq!(view.row.status, RowStatus::Done, "it reported");
    }

    fn ended(
        result_text: Option<&str>,
        structured: Option<Value>,
        last_message: Option<&str>,
    ) -> SessionEnd {
        SessionEnd {
            vendor_session: None,
            exit_code: Some(0),
            signal: None,
            duration_ms: 1_000,
            reason: EndReason::Completed,
            spend: argonauts_core::Spend::default(),
            tool_calls: 0,
            result_text: result_text.map(str::to_owned),
            structured,
            last_message: last_message.map(str::to_owned),
            raw_log: None,
        }
    }

    /// Codex ends with its report as raw JSON. The Outcome reads as the report's summary.
    #[test]
    fn the_outcome_is_the_reports_summary_never_its_json() {
        let json = r#"{"status":"done","summary":"Limit requests per API key.\n\nTests pass."}"#;
        let report: Value = serde_json::from_str(json).unwrap();
        let summary = "Limit requests per API key.\n\nTests pass.";
        let outcome = outcome_of;
        assert_eq!(
            outcome(&ended(Some(json), Some(report), None)).as_deref(),
            Some(summary)
        );
        assert_eq!(
            outcome(&ended(Some(json), None, None)).as_deref(),
            Some(summary),
            "the report is read from the final text too"
        );
        let fenced = format!("Built it.\n\n```json\n{json}\n```\n");
        assert_eq!(
            outcome(&ended(Some(&fenced), None, None)).as_deref(),
            Some(summary)
        );

        let no_summary = r#"{"status":"done"}"#;
        let fenced = format!("Built it.\n\n```json\n{no_summary}\n```");
        assert_eq!(
            outcome(&ended(Some(&fenced), None, None)).as_deref(),
            Some("Built it.")
        );
        assert_eq!(
            outcome(&ended(Some(no_summary), None, Some("Creating it now."))).as_deref(),
            Some("Creating it now.")
        );
        assert_eq!(outcome(&ended(Some(no_summary), None, None)), None);
        assert_eq!(
            outcome(&ended(Some("Set `{a: 1}` in the config."), None, None)).as_deref(),
            Some("Set `{a: 1}` in the config.")
        );
    }

    fn ended_with(key: &SessionKey, result_text: &str, structured: Option<Value>) -> RunEvent {
        let mut event = session_ended(9, key, EndReason::Completed, 1_000);
        if let RunEventKind::Session(argonauts_core::events::session::SessionEvent::Ended(end)) =
            &mut event.kind
        {
            end.result_text = Some(result_text.to_owned());
            end.structured = structured;
            end.tool_calls = 2;
        }
        event
    }

    #[test]
    fn a_report_reads_as_its_summary_and_its_own_tools_stay_out_of_activity() {
        let key = SessionKey::numbered(1);
        let report = serde_json::json!({
            "status": "done",
            "summary": "Built src/limiter/index.ts.",
            "files_changed": ["src/limiter/index.ts"],
            "tests_run": [],
            "concerns": ["Counters reset on restart."],
            "questions": [{"question": "Keep the old limit?"}],
        });
        let finish = AgentEvent::ToolStarted {
            id: "t2".to_owned(),
            name: "finish".to_owned(),
            input: report.clone(),
            parent: None,
        };
        let events = vec![
            session_started(0, &key, None, RoleName::Owner, "antigravity-cli"),
            agent(1, &key, tool("t1", "npm test")),
            agent(2, &key, finish),
            ended_with(&key, &report.to_string(), None),
        ];
        let view = agent_view(&events, &run_id(), &key).unwrap();
        let tools: Vec<_> = view
            .activity
            .iter()
            .map(|line| line.tool.as_str())
            .collect();
        assert_eq!(tools, ["Bash"]);
        assert_eq!(view.tool_calls, 1);
        assert_eq!(
            view.outcome.as_deref(),
            Some(
                "Built src/limiter/index.ts.\n\n**Concerns**\n\n- Counters reset on restart.\n\n\
                 **Questions**\n\n- Keep the old limit?"
            )
        );

        // The structured output wins over the text, and a report with nothing to read shows
        // no JSON either.
        let events = vec![
            session_started(0, &key, None, RoleName::Drafter, "codex"),
            ended_with(
                &key,
                "{\"plan\": []}",
                Some(serde_json::json!({"summary": "Counters in Redis."})),
            ),
        ];
        let view = agent_view(&events, &run_id(), &key).unwrap();
        assert_eq!(view.outcome.as_deref(), Some("Counters in Redis."));
        let events = vec![
            session_started(0, &key, None, RoleName::Drafter, "codex"),
            ended_with(&key, "{\"plan\": []}", None),
        ];
        assert_eq!(agent_view(&events, &run_id(), &key).unwrap().outcome, None);
    }
}
