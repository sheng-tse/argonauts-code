//! The fifteen edge cases of a task's agent stopping: each fake agent plays the signal the real
//! one sends, and the run does what the limits table says for it.

mod limits_support;

use anyhow::Context as _;
use argonauts_core::AgentEvent;
use argonauts_core::events::approval::{AnswerAction, ApprovalKind, ApprovalOpened};
use argonauts_core::events::run::{Party, ResumeCause, WaitReason};
use argonauts_core::events::session::{EndReason, Watchdog};
use argonauts_core::events::task::{TaskState, TryCause};
use argonauts_core::events::{
    AgentStatusEvent, FallbackEvent, RunEventKind, RunLifecycle, TaskEvent,
};
use argonauts_core::limits::HealthState;
use argonauts_scenarios::WAIT;
use limits_support::*;

const FALLBACK_CODEX: &str = "[fallback]\norder = [\"codex\"]\n";
/// Fallbacks are automatic by default, so a scenario about the fallback question asks for it.
const ASK_FALLBACK_CODEX: &str = "[fallback]\nmode = \"ask\"\norder = [\"codex\"]\n";

#[test]
fn limits_edge_usage_limit() -> anyhow::Result<()> {
    let limits = setup("edge-usage-limit", ASK_FALLBACK_CODEX)?;
    let pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;
    let events = limits.events();

    let ends = ends_of(&events, "claude-code");
    assert!(
        matches!(
            ends.first().map(|end| &end.reason),
            Some(EndReason::UsageLimit { resets_at: Some(_) })
        ),
        "{ends:?}"
    );
    assert!(before(
        seq_of(&events, committed),
        opened_at(&events, ApprovalKind::Fallback)
    ));
    assert_eq!(candidate_workers(&question), ["codex"]);
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    // The chip keeps its words at any width: a red bar while there's room, then `out`.
    pty.wait_for(r"Claude (\S+ )?out\b", WAIT)?;
    limits.shot(&pty, "limits_edge_usage_limit")
}

#[test]
fn limits_edge_rate_limit_no_fallback() -> anyhow::Result<()> {
    let limits = setup("edge-rate-limit-no-fallback", FALLBACK_CODEX)?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("the task to finish", LONG_WAIT, completed_by("claude-code"))?;

    let retries = find(&events, |kind| match kind {
        RunEventKind::Agent(AgentEvent::Retry { .. }) => Some(()),
        _ => None,
    });
    assert_eq!(retries.len(), 2);
    assert!(fallback_plans(&events).is_empty());
    assert!(opened(&events, ApprovalKind::Fallback).is_none());
    assert_eq!(limits.calls_with("claude", "acceptEdits")?.len(), 1);
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    pty.wait_for_exit(LONG_WAIT)?;
    Ok(())
}

#[test]
fn limits_edge_outage() -> anyhow::Result<()> {
    let limits = setup(
        "edge-outage",
        "[health]\ndown_after = 1\ndown_sessions = 1\n\n\
         [fallback]\nmode = \"ask\"\norder = [\"codex\"]\n",
    )?;
    let pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::AgentDown)?;
    let events = limits.events();

    let down = find(&events, |kind| match kind {
        RunEventKind::AgentStatus(AgentStatusEvent::Health(health))
            if health.agent_id.0 == "claude-code" && health.state == HealthState::Down =>
        {
            Some(health)
        }
        _ => None,
    });
    assert!(!down.is_empty(), "Claude Code never went down");
    let packet = agent_down_packet(&question)?;
    assert_eq!(packet.agent_id.0, "claude-code");
    assert_eq!(packet.paused.len(), 1);
    // Waiting comes first: a task that waits loses nothing.
    assert_eq!(option_keys(&question), ['w', 'a', 'e']);
    assert!(before(
        seq_of(&events, committed),
        opened_at(&events, ApprovalKind::AgentDown)
    ));
    pty.wait_for("Claude down", WAIT)?;
    limits.shot(&pty, "limits_edge_outage")
}

#[test]
fn limits_edge_all_down_waits() -> anyhow::Result<()> {
    let limits = setup(
        "edge-all-down-waits",
        "crew = [\"claude-code\"]\n\n[fallback]\nmode = \"auto\"\n",
    )?;
    let pty = limits.start()?;
    let events = limits.wait_for("the run to wait", LONG_WAIT, |events| {
        waits(events)
            .iter()
            .any(|reason| matches!(reason, WaitReason::AllAgentsOut { .. }))
    })?;
    let until = waits(&events)
        .iter()
        .find_map(|reason| match reason {
            WaitReason::AllAgentsOut { until } => *until,
            _ => None,
        })
        .context("the wait names no reset time")?;

    // No key is pressed from here on: the app wakes the run by itself once `until` passes.
    let events = limits.wait_for("the run to wake and finish", WAKE_WAIT, |events| {
        completed_by("claude-code")(events)
            && resumes(events)
                .iter()
                .any(|resumed| woke_by_itself(resumed.cause))
    })?;
    let woke = events
        .iter()
        .find(|event| {
            matches!(&event.kind,
                RunEventKind::Run(RunLifecycle::Resumed(resumed)) if woke_by_itself(resumed.cause))
        })
        .context("no wake")?;
    assert!(woke.ts >= until, "woke at {} before {until}", woke.ts);
    limits.shot(&pty, "limits_edge_all_down_waits")
}

#[test]
fn limits_edge_logged_out() -> anyhow::Result<()> {
    let question = retry_resumes_the_same_session("edge-logged-out", "I logged in, retry")?;
    let reason = &fallback_plan(&question)?.reason;
    assert!(reason.contains("log in to"), "{reason}");
    Ok(())
}

#[test]
fn limits_edge_billing_cap() -> anyhow::Result<()> {
    retry_resumes_the_same_session("edge-billing-cap", "I topped up, retry")?;
    Ok(())
}

/// After a logout or a billing cap, the retry option comes first, the other agents are still
/// offered, and `r` resumes the same CLI session. Never a silent switch to another agent.
fn retry_resumes_the_same_session(name: &str, retry: &str) -> anyhow::Result<ApprovalOpened> {
    let limits = setup(name, FALLBACK_CODEX)?;
    let mut pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;

    let first = question
        .options
        .first()
        .context("the task has no options")?;
    assert_eq!(first.key, 'r');
    assert_eq!(first.action, AnswerAction::Retry);
    assert_eq!(first.label, retry);
    assert_eq!(candidate_workers(&question), ["codex"]);
    pty.wait_for(&format!("1\\.\\s+{}", regex::escape(retry)), WAIT)?;
    limits.shot(&pty, &format!("limits_{}", name.replace('-', "_")))?;

    answer(&mut pty, &question, 'r')?;
    let resumed = limits.wait_for_call("claude", "--resume", LONG_WAIT)?;
    let id = resumed
        .args
        .iter()
        .skip_while(|arg| *arg != "--resume")
        .nth(1);
    assert_eq!(id.map(String::as_str), Some("limits-claude-1"));
    limits.wait_for(
        "the resumed session to finish",
        LONG_WAIT,
        completed_by("claude-code"),
    )?;
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    Ok(question)
}

#[test]
fn limits_edge_cli_update_broke_adapter() -> anyhow::Result<()> {
    let limits = setup(
        "edge-cli-update-broke-adapter",
        "[fallback]\nmode = \"auto\"\norder = [\"codex\"]\n",
    )?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;

    let disabled = find(&events, |kind| match kind {
        RunEventKind::AgentStatus(AgentStatusEvent::Disabled { agent_id, reason })
            if agent_id.0 == "claude-code" =>
        {
            Some(reason.clone())
        }
        _ => None,
    });
    let reason = disabled.first().context("Claude Code was never disabled")?;
    assert!(
        reason.to_lowercase().contains("pin"),
        "no pin suggested: {reason}"
    );
    pty.wait_for_exit(LONG_WAIT)?;
    Ok(())
}

#[test]
fn limits_edge_model_retired() -> anyhow::Result<()> {
    let limits = setup(
        "edge-model-retired",
        "[agents.claude-code]\nmodel = \"claude-old-1\"\n\n[fallback]\norder = [\"codex\"]\n",
    )?;
    // passes when: the registry's `successor` takes a full `worker/model` reference.
    write_file(
        &limits.paths().global_models(),
        "[[model]]\nid = \"claude-code/claude-old-1\"\nprovider = \"anthropic\"\nkind = \"cli\"\n\
         access = \"subscription\"\ntools = true\ncost = { unit = \"plan_usage\" }\n\
         successor = \"claude-code/haiku\"\n\n\
         [[model]]\nid = \"claude-code/haiku\"\nprovider = \"anthropic\"\nkind = \"cli\"\n\
         access = \"subscription\"\ntools = true\ncost = { unit = \"plan_usage\" }\n",
    )?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for(
        "the successor to finish",
        LONG_WAIT,
        completed_by("claude-code"),
    )?;

    let ends = ends_of(&events, "claude-code");
    assert!(
        matches!(
            ends.first().map(|end| &end.reason),
            Some(EndReason::ModelUnavailable { model }) if model == "claude-old-1"
        ),
        "{ends:?}"
    );
    let on_haiku = tries(&events).iter().any(|(worker, _)| {
        worker.agent == "claude-code" && worker.model.as_deref() == Some("haiku")
    });
    assert!(on_haiku, "{:?}", tries(&events));
    assert!(limits.call_with("claude", "haiku")?.is_some());
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    pty.wait_for_exit(LONG_WAIT)?;
    Ok(())
}

#[test]
fn limits_edge_context_full() -> anyhow::Result<()> {
    // passes when: the fallback note of a rotation carries the stopped session's last message,
    // which is how the fake tells the fresh session from the first.
    let limits = setup("edge-context-full", FALLBACK_CODEX)?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("the rotated session to finish", LONG_WAIT, |events| {
        has_try("claude-code", TryCause::Rotation)(events) && completed_by("claude-code")(events)
    })?;

    let ends = ends_of(&events, "claude-code");
    assert_eq!(
        ends.first().map(|end| &end.reason),
        Some(&EndReason::ContextFull)
    );
    assert!(
        fallback_plans(&events).is_empty(),
        "a rotation is not a fallback"
    );
    assert!(!task_states(&events).contains(&TaskState::Failed));
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    pty.wait_for_exit(LONG_WAIT)?;
    Ok(())
}

#[test]
fn limits_edge_stuck_or_looping() -> anyhow::Result<()> {
    let limits = setup(
        "edge-stuck-or-looping",
        "[watchdogs]\ninactivity_min = 0.05\n\n[fallback]\norder = [\"codex\"]\n",
    )?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;

    let ends = ends_of(&events, "claude-code");
    assert_eq!(
        ends.first().map(|end| &end.reason),
        Some(&EndReason::Stuck {
            watchdog: Watchdog::Inactivity
        })
    );
    assert!(
        tries(&events)
            .iter()
            .any(|(worker, _)| worker.agent == "codex")
    );
    assert_eq!(limits.calls_with("claude", "acceptEdits")?.len(), 1);
    pty.wait_for_exit(LONG_WAIT)?;
    Ok(())
}

#[test]
fn limits_edge_broken_work() -> anyhow::Result<()> {
    let limits = setup(
        "edge-broken-work",
        "[fallback]\nmode = \"auto\"\norder = [\"codex\"]\nbroken_work = \"restart\"\n\n\
         [checks]\nrun = [{ name = \"no-broken-file\", command = [\"test\", \"!\", \"-e\", \"broken.txt\"] }]\n",
    )?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;

    let plan = fallback_plans(&events)
        .into_iter()
        .next()
        .context("no fallback")?;
    assert!(
        plan.checks_ran,
        "the checks never ran on the stopped task's work"
    );
    let failed_check = seq_of(
        &events,
        |kind| matches!(kind, RunEventKind::Check(check) if !check.passed),
    );
    let handed = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Fallback(FallbackEvent::FellBack { .. }))
    });
    assert!(before(failed_check, handed));

    // Codex's work is committed after its session ends, and in place once the run is over.
    pty.wait_for_exit(LONG_WAIT)?;
    let branch = task_branch(&events).context("the task has no branch")?;
    let files = limits
        .repo
        .git(&["ls-tree", "-r", "--name-only", &branch])?;
    assert!(files.lines().any(|file| file == "hello.txt"), "{files}");
    assert!(!files.lines().any(|file| file == "broken.txt"), "{files}");
    Ok(())
}

#[test]
fn limits_edge_rule_breaking_fallback() -> anyhow::Result<()> {
    // passes when: `exclude_paths` covering every path makes an agent an invalid worker of any
    // task, which is the rule this scenario breaks.
    let limits = setup(
        "edge-rule-breaking-fallback",
        "[agents.codex]\nexclude_paths = [\"**\"]\n\n[fallback]\norder = [\"codex\"]\n",
    )?;
    let pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;

    assert!(
        fallback_plan(&question)?.candidates.is_empty(),
        "{question:?}"
    );
    assert!(candidate_workers(&question).is_empty());
    pty.wait_for("(?i)relax the rule", WAIT)?;
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    limits.shot(&pty, "limits_edge_rule_breaking_fallback")
}

#[test]
fn limits_edge_costly_fallback() -> anyhow::Result<()> {
    // passes when: `agents.codex.auth = "api-key"` makes the registry count Codex as billed
    // by API key.
    let limits = setup(
        "edge-costly-fallback",
        "[agents.codex]\nauth = \"api-key\"\n\n[fallback]\nmode = \"auto\"\norder = [\"codex\"]\n",
    )?;
    let mut pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;

    let plan = fallback_plan(&question)?;
    let codex = plan
        .candidates
        .iter()
        .find(|candidate| candidate.worker.agent == "codex")
        .context("Codex is not offered")?;
    assert!(codex.needs_confirm);
    assert!(
        limits.calls_with("codex", "exec")?.is_empty(),
        "auto switched to a paid API"
    );
    pty.wait_for("API key", WAIT)?;
    limits.shot(&pty, "limits_edge_costly_fallback")?;

    let key = key_for(
        &question,
        |action| matches!(action, AnswerAction::Candidate { worker } if worker.agent == "codex"),
    )
    .context("no key for Codex")?;
    answer(&mut pty, &question, key)?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;
    assert!(fell_back(&events).iter().any(|event| matches!(
        event,
        FallbackEvent::FellBack { to, by: Party::User, .. } if to.agent == "codex"
    )));
    Ok(())
}

#[test]
fn limits_edge_original_returns() -> anyhow::Result<()> {
    let limits = setup(
        "edge-original-returns",
        "[fallback]\nmode = \"auto\"\norder = [\"codex\"]\nunknown_tier = \"equal\"\n",
    )?;
    let mut pty = limits.start()?;
    let events = limits.wait_for("the fallback to Codex", LONG_WAIT, fell_back_to("codex"))?;
    let task = first_task(&events).context("no task")?;

    // Matched in two parts, since the notice can wrap.
    pty.wait_for(
        &format!("Claude Code is back · {task} stays with codex"),
        WAKE_WAIT,
    )?;
    pty.wait_for(&format!("/reassign {task} claude-code"), WAIT)?;
    assert!(opened(&limits.events(), ApprovalKind::OriginalReturned).is_none());
    limits.shot(&pty, "limits_edge_original_returns")?;

    pty.send(&format!("/reassign {task} claude-code\r"))?;
    let events = limits.wait_for(
        "Claude Code to finish the task",
        LONG_WAIT,
        completed_by("claude-code"),
    )?;
    let moved_back = find(&events, |kind| match kind {
        RunEventKind::Task(TaskEvent::Reassigned { to, by, .. }) => Some((to, by)),
        _ => None,
    });
    assert!(
        moved_back
            .iter()
            .any(|(to, by)| to.agent == "claude-code" && **by == Party::User),
        "{moved_back:?}"
    );
    assert!(limits.calls_with("claude", "acceptEdits")?.len() >= 2);
    Ok(())
}

#[test]
fn limits_edge_you_take_over() -> anyhow::Result<()> {
    let limits = setup("edge-you-take-over", ASK_FALLBACK_CODEX)?;
    let mut pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;
    answer(&mut pty, &question, 't')?;
    confirm_take_over(&mut pty)?;

    let back = limits.wait_for_question(&pty, ApprovalKind::TakeOverReturn)?;
    let events = limits.events();
    let command = find(&events, |kind| match kind {
        RunEventKind::Task(TaskEvent::TakenOver { command }) => Some(command.clone()),
        _ => None,
    });
    let command = command
        .first()
        .cloned()
        .flatten()
        .context("no take-over command")?;
    assert!(
        command.iter().any(|arg| arg == "limits-claude-1"),
        "{command:?}"
    );
    let ui = limits
        .call_with("claude", "--resume")?
        .context("the agent UI never ran")?;
    assert!(!ui.args.iter().any(|arg| arg == "-p"), "{:?}", ui.args);
    let changed = find(&events, |kind| match kind {
        RunEventKind::Task(TaskEvent::TakeOverEnded { changed, .. }) => Some(changed.files_changed),
        _ => None,
    });
    assert_eq!(changed, [1]);
    assert_eq!(option_keys(&back), ['c', 't', 'd']);

    answer(&mut pty, &back, 't')?;
    let events = limits.wait_for("the checks after your work", LONG_WAIT, |events| {
        seq_of(events, |kind| matches!(kind, RunEventKind::Check(_))).is_some()
            || opened(events, ApprovalKind::Merge).is_some()
    })?;
    assert!(task_states(&events).contains(&TaskState::TakenOver));
    limits.shot(&pty, "limits_edge_you_take_over")
}

/// A wake the app gave the run by itself: its time came, or the agent it waited for is back.
fn woke_by_itself(cause: ResumeCause) -> bool {
    matches!(cause, ResumeCause::AgentBack | ResumeCause::Timer)
}
