use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::{BriefFrom, RunLifecycle, StartsFrom};
use argonauts_core::events::session::{InputPartKind, SessionEnd};
use argonauts_core::events::task::TaskEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::session_runner::Access;
use argonauts_core::steps::{is_own_tool, last_json_object};
use argonauts_core::views::{
    ActivityLine, AgentView, AskedFor, Moved, PromptAbout, ReviewGiven, StartedFrom,
    SubagentsSection, Tone,
};
use argonauts_core::{DiffStat, RunId};
use argonauts_tui::bridge::tools::start_call;
use argonauts_tui::cell::ToolTitle;
use serde_json::Value;
use std::path::Path;

use super::sessions::{PartAt, RunFacts, SessionFacts, ToolCall};

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

/// The agent of a task's CLI workflow that handed `session` its job, when one did: a review or
/// a sign-off asked through Argonauts Code, joined to the agent that asked as the workflow's
/// rows join it. With it comes the session that runs that workflow, whose task the asked
/// session works on.
fn asked_for<'a>(
    events: &[RunEvent],
    facts: &'a RunFacts,
    session: &SessionFacts,
) -> Option<(AskedFor, &'a SessionFacts)> {
    let workflows: Vec<_> = super::workflow::cli_workflow_runs(events)
        .into_iter()
        .filter(|(host, _)| facts.session(host).is_some_and(|host| host.task.is_some()))
        .collect();
    let (host, agent) = super::asked::asker_of(facts, &workflows, &session.key)?;
    let host = facts.session(host)?;
    let model = agent.model.clone().or_else(|| host.worker.model.clone());
    // An agent at work has no time of its own in the report yet, so it counts from its start.
    let now = facts.last_ts.and_then(super::workflow::epoch_ms);
    let elapsed_ms = match (agent.state.ended(), agent.started_at_ms, now) {
        (false, Some(started), Some(now)) => now.saturating_sub(started).max(agent.duration_ms),
        _ => agent.duration_ms,
    };
    let asked = AskedFor {
        cli: host.worker.agent.clone(),
        label: agent.label.trim().to_owned(),
        worker: ModelRef::new(host.worker.agent.clone(), model),
        tokens: agent.tokens,
        elapsed_ms,
    };
    Some((asked, host))
}

/// One session's Prompt, Activity, Diff, Subagents, and Outcome. The report tools are how the
/// agent hands back its result, which the Outcome shows, so Activity leaves them out.
pub(crate) fn agent_view(
    events: &[RunEvent],
    run: &RunId,
    session: &SessionKey,
) -> Option<AgentView> {
    let facts = RunFacts::of(events);
    let Some(this) = facts.session(session) else {
        return left_out_view(events, run, session);
    };
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
            let (tool, summary) = titled(&this.started.agent_id.0, call, this.cwd.as_deref());
            ActivityLine {
                tool,
                summary,
                ok: call.ok,
                note: None,
            }
        })
        .collect();
    let row = facts.agent_row(this);
    let asked = asked_for(events, &facts, this);
    let task = this
        .task
        .as_ref()
        .or_else(|| asked.as_ref().and_then(|(_, host)| host.task.as_ref()));
    let earlier = facts.verdicts.earlier(&facts, this, task);
    let rounds = earlier.as_ref().map_or(0, |earlier| earlier.round);
    let asked_for = asked.map(|(asked, _)| asked);
    // A challenger's points are its outcome, listed under what its row says of them.
    let (challenged, points) = challenge_of(&facts, this).unzip();
    Some(AgentView {
        asked_for,
        earlier,
        run: run.clone(),
        goal: super::goal_feed::goal_of(events, &row),
        row,
        prompt: this.prompt.clone(),
        activity,
        tool_calls: this
            .tool_calls()
            .saturating_sub(u32::try_from(own_calls).unwrap_or(u32::MAX)),
        diff: diff_of(events, this),
        subagents: subagents_of(this),
        outcome: points.or_else(|| this.ended.as_ref().and_then(outcome_of)),
        plan: this.ended.as_ref().and_then(plan_of),
        facts: facts.verdicts.facts(this),
        to_fix: facts.verdicts.to_fix(&facts, this),
        review: facts.verdicts.given(this).or(challenged),
        moved: moved_of(this),
        given: given_of(this),
        prompt_about: prompt_about(this, rounds),
        started_from: started_from(events, &facts, this),
    })
}

/// What an orchestrator began from, when your words in a conversation started the run: a
/// copy of the main session where its CLI made one, else the brief, which the run's first
/// orchestrator read alone.
fn started_from(events: &[RunEvent], facts: &RunFacts, this: &SessionFacts) -> Option<StartedFrom> {
    let starts = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => started.starts_from.clone(),
        _ => None,
    })?;
    if this.started.forked {
        let tokens = match &starts {
            StartsFrom::Fork(fork) => fork.tokens,
            StartsFrom::Brief(_) => None,
        };
        return Some(StartedFrom::Fork { tokens });
    }
    let first = facts
        .sessions
        .iter()
        .find(|session| session.started.role == RoleName::Orchestrator)?;
    if first.key != this.key {
        return None;
    }
    Some(StartedFrom::Brief(match starts {
        StartsFrom::Brief(brief) => brief,
        // The copy never started, so the brief was all it had.
        StartsFrom::Fork(fork) => BriefFrom {
            written_by: fork.worker,
            why: None,
        },
    }))
}

/// How a reviewer or a judge that ran with no tools had its work, all of it in its prompt:
/// `Reviewed with the change and the checks in its prompt, no tools`.
fn given_of(session: &SessionFacts) -> Option<String> {
    let started = &session.started;
    if started.access != Access::NoTools {
        return None;
    }
    let verb = match (started.role, session.ended.is_some()) {
        (RoleName::Reviewer, true) => "Reviewed",
        (RoleName::Reviewer, false) => "Reviews",
        (RoleName::Judge, true) => "Judged",
        (RoleName::Judge, false) => "Judges",
        _ => return None,
    };
    let sent = |kind: InputPartKind| session.parts.iter().any(|part| part.kind == kind);
    let what: Vec<String> = [
        (InputPartKind::Evidence, "the change"),
        (InputPartKind::CheckOutput, "the checks"),
    ]
    .into_iter()
    .filter(|(kind, _)| sent(*kind))
    .map(|(_, what)| what.to_owned())
    .collect();
    Some(if what.is_empty() {
        format!("{verb} with no tools")
    } else {
        format!(
            "{verb} with {} in its prompt, no tools",
            argonauts_core::text::listed(&what)
        )
    })
}

/// Where the session's prompt has its own words, past the frame every agent of its role gets,
/// and what its later parts hold, such as `your request` and `the change`. `rounds` is how
/// many rounds of review it was given.
fn prompt_about(session: &SessionFacts, rounds: u32) -> Option<PromptAbout> {
    let parts = &session.parts;
    let own = parts
        .iter()
        .position(|part| part.kind != InputPartKind::Frame)
        .unwrap_or(parts.len());
    let first = parts.get(own).map_or(0, |part| part.line);
    let mut holds: Vec<String> = Vec::new();
    for part in parts.iter().skip(own + 1) {
        if let Some(words) = part_holds(part, rounds).filter(|words| !holds.contains(words)) {
            holds.push(words);
        }
    }
    (first > 0 || !holds.is_empty()).then_some(PromptAbout { first, holds })
}

/// What one part of a prompt holds, as its preview names it. Rules, the run's status, the
/// answer's shape and what the agent before it said of its own work say how to go about it,
/// so the preview leaves them out.
fn part_holds(part: &PartAt, rounds: u32) -> Option<String> {
    let words = match part.kind {
        InputPartKind::Goal | InputPartKind::Request => "your request",
        InputPartKind::Task => "the task",
        InputPartKind::Notes => "notes",
        InputPartKind::Decisions => "decisions",
        InputPartKind::EarlierFindings if rounds > 1 => "the earlier reviews",
        InputPartKind::EarlierFindings => "the earlier review",
        InputPartKind::CheckOutput => "the test run",
        InputPartKind::Evidence => "the change",
        InputPartKind::StepInput => "its input",
        InputPartKind::File => {
            let file = sanitize_for_terminal(part.label.trim());
            return (!file.is_empty()).then_some(file);
        }
        _ => return None,
    };
    Some(words.to_owned())
}

/// What a council's challenger gave: the words its row says, `against p0 · 3 points`, and each
/// point, which its view lists under them.
fn challenge_of(facts: &RunFacts, session: &SessionFacts) -> Option<(ReviewGiven, String)> {
    let said = facts.verdicts.gave(session)?;
    if !said.text.starts_with("against ") {
        return None;
    }
    let answer = session.ended.as_ref()?.structured.as_ref()?;
    let points: Vec<String> = answer
        .get("points")?
        .as_array()?
        .iter()
        .filter_map(|point| match point {
            Value::String(text) => Some(text.clone()),
            other => other
                .get("point")
                .or_else(|| other.get("text"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .map(|point| sanitize_for_terminal(point.trim()))
        .filter(|point| !point.is_empty())
        .collect();
    let given = ReviewGiven {
        verdict: said.text,
        tone: Tone::Text,
        findings: Vec::new(),
    };
    Some((given, points.join("\n")))
}

/// The view of a job no model could take, which its workflow went on without. Nothing ran, so
/// it has no prompt, calls or outcome: only the model it was meant for, what it was in its
/// phase, why it was left out and what that means for the run.
fn left_out_view(events: &[RunEvent], run: &RunId, session: &SessionKey) -> Option<AgentView> {
    let (row, left) = super::workflow::left_out_job(events, session)?;
    // A run recorded before the whole words were kept has only the few its row says.
    let because = if left.said.trim().is_empty() {
        left.why
    } else {
        left.said
    };
    let facts = Some(left.acts_as).filter(|acts_as| !acts_as.trim().is_empty());
    Some(AgentView {
        run: run.clone(),
        row,
        prompt: String::new(),
        activity: Vec::new(),
        tool_calls: 0,
        diff: None,
        subagents: None,
        outcome: None,
        goal: None,
        plan: None,
        facts: facts.into_iter().collect(),
        to_fix: None,
        review: None,
        asked_for: None,
        earlier: None,
        moved: Some(Moved::LeftOut {
            because,
            lines: left.means,
        }),
        given: None,
        prompt_about: None,
        started_from: None,
    })
}

/// Why the session does its job in another model's place, when it does: that model and what
/// became of it, and on one CLI, that only that model is out.
fn moved_of(this: &SessionFacts) -> Option<Moved> {
    let asked = this.fallback_for.clone()?;
    let why = this
        .fallback_why
        .as_deref()
        .map(str::trim)
        .filter(|why| !why.is_empty());
    let which = match why {
        Some(why @ ("overloaded" | "rate-limited" | "signed out" | "out of credits")) => {
            format!("was {why}")
        }
        Some(why) => why.to_owned(),
        None => "couldn't take it".to_owned(),
    };
    let names = DisplayNames::default();
    let lines = if asked.agent == this.worker.agent {
        vec![format!(
            "only {} is out, so {}'s next model takes it",
            names.short(&asked),
            names.cli(&asked.agent)
        )]
    } else {
        Vec::new()
    };
    Some(Moved::FallbackFor {
        asked,
        which,
        lines,
    })
}

/// The plan the agent ended with, when its result is one: an orchestrator's tasks with their
/// assignments, or a draft's pieces. The screen draws it as a table.
fn plan_of(end: &SessionEnd) -> Option<Value> {
    let texts = || [&end.result_text, &end.last_message].into_iter().flatten();
    let result = end
        .structured
        .clone()
        .filter(Value::is_object)
        .or_else(|| texts().find_map(|text| last_json_object(text)))?;
    let lists = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    };
    (lists("tasks") || lists("pieces")).then_some(result)
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

/// A report's summary, or a conversation step's answer, then the lists a person reads in it.
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

/// The task's latest diff. A task with a worktree and no diff yet shows an empty one. While
/// the session runs, what it changed since the last commit counts too; Argonauts Code commits
/// it when the session ends.
fn diff_of(events: &[RunEvent], session: &SessionFacts) -> Option<DiffStat> {
    let task = session.task.as_ref()?;
    let mut diff = None;
    for event in events
        .iter()
        .filter(|event| event.task_id.as_ref() == Some(task))
    {
        match &event.kind {
            RunEventKind::Task(TaskEvent::Worktree { .. }) => {
                diff.get_or_insert_with(DiffStat::default);
            }
            RunEventKind::Task(TaskEvent::Diff { stat, .. }) => diff = Some(*stat),
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
    let threads = session.started.agent_id.0 == "codex";
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
fn titled(agent_id: &str, call: &ToolCall, cwd: Option<&Path>) -> (String, String) {
    match start_call(agent_id, &call.name, &call.input).title {
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
        agent, input, run_id, session_ended, session_event, session_started, task_event,
    };
    use argonauts_core::AgentEvent;
    use argonauts_core::events::session::{
        EndReason, SubagentKind, SubagentStatus, SubagentUpdate,
    };
    use argonauts_core::events::session::{FileDiff, LimitEvent, SessionEvent};
    use argonauts_core::events::task::{TaskState, TryCause};
    use argonauts_core::meter::{MeterAction, MeterLimit, SpendStatus};
    use argonauts_core::names::{ModelRef, RoleName};
    use argonauts_core::steps::StepName;
    use argonauts_core::views::RowStatus;
    use argonauts_core::{Spend, TaskId};

    fn tool(id: &str, command: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.to_owned(),
            name: "Bash".to_owned(),
            input: serde_json::json!({ "command": command }),
            parent: None,
        }
    }

    /// A reviewer's prompt as the built-in review sends it to a model with no tools: its
    /// instructions, then the change and, when `checks`, the checks' output.
    fn reviewed_in_its_prompt(seq: u64, key: &SessionKey, checks: bool) -> RunEvent {
        let mut sent = input(seq, key, "Review the change below.\n\n");
        if let RunEventKind::Session(SessionEvent::Input(input)) = &mut sent.kind {
            let mut add = |kind, label: &str, text: &str| {
                input
                    .parts
                    .push(argonauts_core::events::session::InputPart {
                        kind,
                        label: label.to_owned(),
                        text: text.to_owned(),
                        approx_tokens: 1,
                        source: None,
                    });
            };
            add(
                InputPartKind::Evidence,
                "the change",
                "diff --git a/x b/x\n\n",
            );
            if checks {
                add(
                    InputPartKind::CheckOutput,
                    "the checks",
                    "tests passed: 3 passed\n",
                );
            }
        }
        sent
    }

    /// A reviewer that ran with no tools had its work in its prompt, which its view says, in
    /// the tense of where it is; one with tools reads for itself and says nothing of the kind.
    #[test]
    fn a_reviewer_with_no_tools_says_what_its_prompt_held() {
        let key = SessionKey::numbered(1);
        let mut started = session_started(0, &key, None, RoleName::Reviewer, "ollama");
        let read_only = started.clone();
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut started.kind {
            started.access = Access::NoTools;
        }
        let view = |events: &[RunEvent]| agent_view(events, &run_id(), &key).unwrap();
        let mut events = vec![started.clone(), reviewed_in_its_prompt(1, &key, true)];
        assert_eq!(
            view(&events).given.as_deref(),
            Some("Reviews with the change and the checks in its prompt, no tools")
        );
        events.push(session_ended(2, &key, EndReason::Completed, 1_000));
        let ended = view(&events);
        assert_eq!(
            ended.given.as_deref(),
            Some("Reviewed with the change and the checks in its prompt, no tools")
        );
        // Its prompt starts with its own words, and the preview names what comes after.
        assert_eq!(
            ended.prompt_about,
            Some(PromptAbout {
                first: 0,
                holds: vec!["the change".to_owned(), "the test run".to_owned()],
            })
        );

        let without_checks = [started, reviewed_in_its_prompt(1, &key, false)];
        assert_eq!(
            view(&without_checks).given.as_deref(),
            Some("Reviews with the change in its prompt, no tools")
        );
        let with_tools = [read_only, reviewed_in_its_prompt(1, &key, true)];
        assert_eq!(view(&with_tools).given, None);
    }

    /// A prompt of one part, with no frame before it, has nothing for its preview to say.
    #[test]
    fn a_prompt_of_its_own_words_alone_has_no_preview_facts() {
        let key = SessionKey::numbered(1);
        let events = [
            session_started(0, &key, None, RoleName::Worker, "claude-code"),
            input(1, &key, "Build the export.\n"),
        ];
        let view = agent_view(&events, &run_id(), &key).unwrap();
        assert_eq!((view.given, view.prompt_about), (None, None));
    }

    #[test]
    fn the_agent_view_shows_prompt_latest_activity_diff_subagents_and_outcome() {
        let task = argonauts_core::TaskId::new("task-002").unwrap();
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
            tokens: None,
        };
        let events = vec![
            session_started(0, &key, Some(&task), RoleName::Worker, "codex"),
            input(1, &key, "Build the export.\n"),
            task_event(
                2,
                &task,
                TaskEvent::Worktree {
                    path: "/tmp/wt".into(),
                    branch: "argonauts/run/task-002".to_owned(),
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

    fn worker_started(seq: u64, key: &SessionKey, task: &TaskId, worker: &str) -> RunEvent {
        let mut event = session_started(seq, key, Some(task), RoleName::Worker, worker);
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
        let task = TaskId::new("task-001").unwrap();
        let (codex, claude) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let events = vec![
            worker_started(0, &codex, &task, "codex"),
            input(1, &codex, "Build it.\n"),
            called(
                2,
                &codex,
                "c1",
                "command_execution",
                serde_json::json!({"command": "/bin/zsh -lc 'rg --files | head -50'"}),
            ),
            worker_started(3, &claude, &task, "claude-code"),
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

    /// While the worker works, the Diff counts what it changed since the worktree's last
    /// commit, the latest diff of each file; once it ends, Argonauts Code's commit counts.
    #[test]
    fn a_working_workers_diff_counts_its_changes_so_far() {
        let task = TaskId::new("task-001").unwrap();
        let key = SessionKey::numbered(1);
        let changed = |seq, path: &str, added, removed| {
            session_event(
                seq,
                &key,
                Some(&task),
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
            worker_started(0, &key, &task, "claude-code"),
            task_event(
                1,
                &task,
                TaskEvent::Worktree {
                    path: "/tmp/wt".into(),
                    branch: "argonauts/run/task-001".to_owned(),
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
        events.push(task_event(
            6,
            &task,
            TaskEvent::Diff {
                stat: committed,
                path: "diffs/task.diff".into(),
            },
        ));
        assert_eq!(
            agent_view(&events, &run_id(), &key).unwrap().diff,
            Some(committed)
        );
    }

    /// A stream with no dollars, as Claude Code's is until it ends, is priced by the meter, as
    /// the run's limit prices it.
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
            session_started(0, &key, None, RoleName::Worker, "claude-code"),
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
            RunEventKind::Session(SessionEvent::Limit(LimitEvent::Spend(SpendStatus {
                level: MeterAction::Continue,
                limit: MeterLimit::None,
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

    /// A retry reads as its try, and a try that ended as if done but never reported,
    /// so that its task failed, reads as failed.
    #[test]
    fn a_retry_reads_as_its_try_and_a_silent_end_as_failed() {
        let task = TaskId::new("task-001").unwrap();
        let (first, second) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let try_event = |seq, try_number, cause| {
            task_event(
                seq,
                &task,
                TaskEvent::Try {
                    try_number,
                    worker: ModelRef::new("opencode", None),
                    cause,
                    told: Vec::new(),
                },
            )
        };
        let state = |seq, to| {
            task_event(
                seq,
                &task,
                TaskEvent::State {
                    from: None,
                    to,
                    reason: String::new(),
                },
            )
        };
        let events = vec![
            try_event(0, 1, TryCause::First),
            worker_started(1, &first, &task, "opencode-acp"),
            session_ended(2, &first, EndReason::Completed, 1_000),
            state(3, TaskState::Failed),
            try_event(4, 2, TryCause::AfterFailure),
            worker_started(5, &second, &task, "opencode"),
            session_ended(6, &second, EndReason::Completed, 1_000),
        ];
        let row = |key: &SessionKey| agent_view(&events, &run_id(), key).unwrap().row;
        assert_eq!(row(&first).label, "worker · task-001");
        assert_eq!(row(&first).status, RowStatus::Failed);
        assert_eq!(row(&second).label, "worker · task-001 · try 2");
        assert_eq!(
            row(&second).status,
            RowStatus::Done,
            "its task hasn't moved on yet"
        );

        let mut reported = events.clone();
        reported.push(task_event(
            7,
            &task,
            TaskEvent::Reported(Box::new(
                serde_json::from_value(serde_json::json!({
                    "status": "done", "summary": "Fixed add.", "files_changed": [],
                    "tests_run": [], "concerns": []
                }))
                .unwrap(),
            )),
        ));
        reported.push(state(8, TaskState::Failed));
        let view = agent_view(&reported, &run_id(), &second).unwrap();
        assert_eq!(view.row.status, RowStatus::Done, "it reported");
    }

    /// The session of a try you started with `r` says so after its role.
    #[test]
    fn a_try_you_restarted_says_so() {
        let task = TaskId::new("task-001").unwrap();
        let (first, second) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let try_event = |seq, try_number, cause| {
            task_event(
                seq,
                &task,
                TaskEvent::Try {
                    try_number,
                    worker: ModelRef::new("opencode", None),
                    cause,
                    told: Vec::new(),
                },
            )
        };
        let events = vec![
            try_event(0, 1, TryCause::First),
            worker_started(1, &first, &task, "opencode"),
            session_ended(2, &first, EndReason::Stopped, 1_000),
            try_event(3, 2, TryCause::Restarted),
            worker_started(4, &second, &task, "opencode"),
        ];
        let facts = |key: &SessionKey| agent_view(&events, &run_id(), key).unwrap().facts;
        assert_eq!(facts(&first), ["worker"]);
        assert_eq!(facts(&second), ["worker", "try 2 (restarted)"]);
    }

    /// A workflow can have its workers review each other's work. Such a session reads as what
    /// it is there, and says what it may touch from how it was started, not from its role.
    #[test]
    fn a_worker_asked_to_review_reads_as_a_read_only_reviewer() {
        use argonauts_core::session_runner::Access;

        let asked = |seq, key: &SessionKey, label: &str, access| {
            let mut event = session_started(seq, key, None, RoleName::Worker, "codex");
            if let RunEventKind::Session(SessionEvent::Started(started)) = &mut event.kind {
                started.label = Some(label.to_owned());
                started.access = access;
            }
            event
        };
        let (reviewer, judge, builder) = (
            SessionKey::numbered(1),
            SessionKey::numbered(2),
            SessionKey::numbered(3),
        );
        let events = vec![
            asked(0, &reviewer, "review:rate-limit", Access::ReadOnly),
            asked(1, &judge, "sign-off:all", Access::ReadOnly),
            asked(2, &builder, "build:rate-limit", Access::Build),
        ];
        let facts = |key: &SessionKey| agent_view(&events, &run_id(), key).unwrap().facts;
        assert_eq!(facts(&reviewer), ["reviewer", "read-only"]);
        assert_eq!(facts(&judge), ["judge", "read-only"]);
        assert_eq!(facts(&builder), ["worker"]);

        // A reviewer by role that an endpoint runs has no tools at all, and says that.
        let mut endpoint = session_started(0, &reviewer, None, RoleName::Reviewer, "ollama");
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut endpoint.kind {
            started.access = Access::NoTools;
        }
        let view = agent_view(&[endpoint], &run_id(), &reviewer).unwrap();
        assert_eq!(view.facts, ["reviewer", "no tools"]);
    }

    fn ended(
        result_text: Option<&str>,
        structured: Option<Value>,
        last_message: Option<&str>,
    ) -> SessionEnd {
        SessionEnd {
            cli_session: None,
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

    /// An orchestrator's plan, or a draft's pieces, is kept whole for the screen's table. A
    /// report, which reads as its summary, is no plan.
    #[test]
    fn a_plan_is_kept_for_the_table_and_a_report_is_not_one() {
        let plan = serde_json::json!({
            "plan": {"summary": "Two tasks."},
            "tasks": [{"key": "iface", "title": "Due-date stubs"}],
            "assignments": [{"task": "iface", "worker": "codex", "reviewers": []}]
        });
        let text = plan.to_string();
        assert_eq!(
            plan_of(&ended(None, Some(plan.clone()), None)),
            Some(plan.clone())
        );
        let written = format!("Here is the plan.\n\n```json\n{text}\n```");
        assert_eq!(plan_of(&ended(Some(&written), None, None)), Some(plan));

        let draft = serde_json::json!({"summary": "One piece.", "pieces": [{"name": "store"}]});
        assert!(plan_of(&ended(None, Some(draft), None)).is_some());

        let report = serde_json::json!({"status": "done", "summary": "Built it.", "tasks": []});
        assert_eq!(plan_of(&ended(None, Some(report), None)), None);
        assert_eq!(plan_of(&ended(Some("All done."), None, None)), None);
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
            session_started(0, &key, None, RoleName::Worker, "antigravity-cli"),
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
            session_started(0, &key, None, RoleName::Draft, "codex"),
            ended_with(
                &key,
                "{\"plan\": []}",
                Some(serde_json::json!({"summary": "Counters in Redis."})),
            ),
        ];
        let view = agent_view(&events, &run_id(), &key).unwrap();
        assert_eq!(view.outcome.as_deref(), Some("Counters in Redis."));
        let events = vec![
            session_started(0, &key, None, RoleName::Draft, "codex"),
            ended_with(&key, "{\"plan\": []}", None),
        ];
        assert_eq!(agent_view(&events, &run_id(), &key).unwrap().outcome, None);
    }

    /// A run your words started records what its orchestrator began from. A session its CLI
    /// copied from the main session says so with the conversation's size; else the run's
    /// first orchestrator read the brief, and later sessions say nothing of it.
    #[test]
    fn an_orchestrators_view_says_what_it_began_from() {
        use crate::client::test_support::run_started;
        use argonauts_core::events::run::{BriefFrom, ForkFrom, RunLifecycle, StartsFrom};

        let opus: ModelRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let fork = StartsFrom::Fork(ForkFrom {
            worker: opus.clone(),
            cli_session: "main-1".to_owned(),
            folder: "/repo".into(),
            tokens: Some(48_200),
        });
        let start = |starts_from: StartsFrom| {
            let mut started = run_started(0, "Add due dates", None);
            if let RunEventKind::Run(RunLifecycle::Started(run)) = &mut started.kind {
                run.starts_from = Some(starts_from);
            }
            started
        };
        let orchestrator = |seq, key: &SessionKey, forked: bool| {
            let mut started =
                session_started(seq, key, None, RoleName::Orchestrator, "claude-code");
            if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
                session.forked = forked;
            }
            started
        };
        let (first, later) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let events = vec![
            start(fork.clone()),
            orchestrator(1, &first, true),
            orchestrator(2, &later, false),
        ];
        let from =
            |events: &[RunEvent], key| agent_view(events, &run_id(), key).unwrap().started_from;
        assert_eq!(
            from(&events, &first),
            Some(StartedFrom::Fork {
                tokens: Some(48_200)
            })
        );
        assert_eq!(from(&events, &later), None);

        // A copy that never started leaves the brief, which the main agent wrote.
        let events = vec![start(fork), orchestrator(1, &first, false)];
        assert_eq!(
            from(&events, &first),
            Some(StartedFrom::Brief(BriefFrom {
                written_by: opus,
                why: None
            }))
        );
    }
}
