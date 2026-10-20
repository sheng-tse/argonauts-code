//! A generic ACP agent owning a task: the fake agent installed as `hermes` speaks ACP, and the
//! CLI runner answers its permission requests, delivers your messages, and stops it after
//! its running tool call.

use crate::limits_support;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_scenarios::WAIT;
use limits_support::*;
use serde_json::Value;

/// Hermes is an experimental profile, off until you turn it on.
const HERMES_OWNS: &str = "crew.main = \"hermes\"\n\n[agents.hermes]\nenabled = true\n";

/// Merging is automatic by default, so the merge question needs supervised.
const SUPERVISED_MERGE: &str = "\n[approvals.merge]\nmode = \"supervised\"\n";

#[test]
fn limits_acp_agent_owns_task() -> anyhow::Result<()> {
    let limits = setup(
        "acp-agent-owns-task",
        &format!("{HERMES_OWNS}{SUPERVISED_MERGE}"),
    )?;
    let pty = limits.start()?;
    limits.wait_for(
        "the agent to finish the task",
        LONG_WAIT,
        completed_by("hermes"),
    )?;

    let reports = limits.report_calls()?;
    assert_eq!(reports.len(), 1, "{reports:?}");
    let (_, reply) = &reports[0];
    assert_ne!(
        reply.get("isError").and_then(Value::as_bool),
        Some(true),
        "{reply}"
    );
    assert!(
        limits.wrong_picks()?.is_empty(),
        "{:?}",
        limits.wrong_picks()?
    );
    limits.wait_for_question(&pty, ApprovalKind::Merge)?;
    limits.shot(&pty, "limits_acp_agent_owns_task")
}

#[test]
fn limits_acp_denies_forbidden_path() -> anyhow::Result<()> {
    let limits = setup("acp-denies-forbidden-path", HERMES_OWNS)?;
    let _pty = limits.start()?;
    limits.wait_for(
        "the agent to finish the task",
        LONG_WAIT,
        completed_by("hermes"),
    )?;

    // The fake agent logs any pick other than the one it expects: reject once for
    // `/etc/hosts`, allow once for hello.txt.
    assert!(
        limits.wrong_picks()?.is_empty(),
        "{:?}",
        limits.wrong_picks()?
    );
    let answers: Vec<String> = limits
        .frames()?
        .iter()
        .filter(|frame| !frame.from_agent && frame.frame.get("result").is_some())
        .filter_map(|frame| {
            frame
                .frame
                .pointer("/result/outcome/optionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(answers, ["reject-once", "allow-once"]);
    Ok(())
}

#[test]
fn limits_acp_steer_then_stop_after_tool() -> anyhow::Result<()> {
    let limits = setup("acp-steer-then-stop", HERMES_OWNS)?;
    let mut pty = limits.start()?;
    limits.wait_for_frame("t1 done", LONG_WAIT, |frame| {
        tool_update(frame, "t1", "completed")
    })?;

    let steer = "also list the files you changed";
    pty.send(&format!("@hermes {steer}\r"))?;
    limits.wait_for_frame("your message", LONG_WAIT, |frame| {
        client_call(frame, "session/prompt") && frame.text().contains(steer)
    })?;

    let running = limits.wait_for_frame("t2 running", LONG_WAIT, |frame| {
        tool_update(frame, "t2", "in_progress") || is_tool_call(frame, "t2")
    })?;
    // A pause stops each session after its running tool call.
    argonauts_scenarios::pause_run(&mut pty)?;
    let cancel = limits.wait_for_frame("the cancel after t2", LONG_WAIT, |frame| {
        client_call(frame, "session/cancel") && frame.index > running.index
    })?;
    let finished = limits
        .frames()?
        .into_iter()
        .find(|frame| tool_update(frame, "t2", "completed"))
        .ok_or_else(|| anyhow::anyhow!("t2 never finished"))?;
    assert!(
        finished.index < cancel.index,
        "the turn was cancelled while t2 still ran"
    );
    pty.wait_for("(?i)paus", WAIT)?;
    limits.shot(&pty, "limits_acp_steer_then_stop_after_tool")
}

/// The agent's `tool_call` announcing `id`.
fn is_tool_call(frame: &Frame, id: &str) -> bool {
    frame.from_agent
        && frame.update().is_some_and(|update| {
            update.get("sessionUpdate").and_then(Value::as_str) == Some("tool_call")
                && update.get("toolCallId").and_then(Value::as_str) == Some(id)
        })
}
