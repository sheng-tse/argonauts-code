//! Fallbacks after a usage limit: the question and its candidates, auto mode, reassigning at any
//! time, the ask timeout and wait mode on a waiting run, and the checkpoint round trip.

use crate::limits_support;
use anyhow::Context as _;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent, ApprovalKind, ApprovalPacket};
use argonauts_core::events::run::{Party, ResumeCause, RunOutcome, WaitReason};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{
    AgentStatusEvent, FallbackEvent, RunEvent, RunEventKind, RunLifecycle, TaskEvent,
};
use argonauts_core::limits::PlanUsageStatus;
use argonauts_core::settings::fallback::FallbackMode;
use argonauts_scenarios::WAIT;
use argonauts_testkit::Key;
use limits_support::*;

const FALLBACK_CODEX: &str = "[fallback]\nmode = \"ask\"\norder = [\"codex\"]\n";

/// Waiting runs exit their run host quickly here, so the wake path is what the tests see.
const WAIT_FAST: &str = "\n[run_host]\nwait_grace = \"1s\"\n";

#[test]
fn limits_usage_limit_fallback_question() -> anyhow::Result<()> {
    let limits = setup(
        "usage-limit-fallback-question",
        "[fallback]\nmode = \"ask\"\norder = [\"codex\", \"antigravity-cli\"]\n",
    )?;
    let mut pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;

    let plan = fallback_plan(&question)?;
    assert_eq!(plan.stopped.agent, "claude-code");
    assert!(plan.reason.contains("usage limit"), "{}", plan.reason);
    assert!(plan.resets_at.is_some());
    assert!(plan.work.files_changed >= 1, "notes.txt was not committed");
    assert_eq!(candidate_workers(&question), ["codex", "antigravity-cli"]);
    let keys = option_keys(&question);
    assert!(in_order(&keys, &['a', 'b', 't', 'w', 'o', 'd']), "{keys:?}");
    pty.wait_for("Agent: build:\\S+ · (?:\\S.* on )?Claude Code", WAIT)?;
    for option in &question.options {
        pty.wait_for(&regex::escape(&option.label), WAIT)?;
    }
    limits.shot(&pty, "limits_usage_limit_fallback_question")?;

    answer(&mut pty, &question, 'a')?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;
    assert!(fell_back(&events).iter().any(|event| matches!(
        event,
        FallbackEvent::FellBack { to, by: Party::User, .. } if to.agent == "codex"
    )));
    limits.wait_for(
        "the merge approval",
        LONG_WAIT,
        has_opened(ApprovalKind::Merge),
    )?;
    Ok(())
}

#[test]
fn limits_auto_fallback_keeps_working() -> anyhow::Result<()> {
    let limits = setup(
        "auto-fallback-keeps-working",
        "[fallback]\nmode = \"auto\"\norder = [\"codex\"]\n",
    )?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;

    assert!(
        opened(&events, ApprovalKind::Fallback).is_none(),
        "auto mode asked"
    );
    assert!(fell_back(&events).iter().any(|event| matches!(
        event,
        FallbackEvent::FellBack { to, mode: FallbackMode::Auto, .. } if to.agent == "codex"
    )));
    assert!(before(
        seq_of(&events, committed),
        seq_of(&events, |kind| matches!(
            kind,
            RunEventKind::Fallback(FallbackEvent::FellBack { .. })
        ))
    ));
    said_for_the_workflows_view(&events)?;
    pty.wait_for_exit(LONG_WAIT)?;
    Ok(())
}

/// The task's workflow says its fallback in one line as well, which the workflow's view shows
/// under its header: who stopped and why, then who the task fell back to.
fn said_for_the_workflows_view(events: &[RunEvent]) -> anyhow::Result<()> {
    let about_fallbacks = find(events, |kind| match kind {
        RunEventKind::Workflow(WorkflowEvent::Note(note)) if note.text.contains("fell back to") => {
            Some(note)
        }
        _ => None,
    });
    let [said] = about_fallbacks.as_slice() else {
        anyhow::bail!("one line about the fallback, not {about_fallbacks:?}");
    };
    let started = sessions_started(events);
    let worker_on = |agent: &str| {
        started
            .iter()
            .find(|session| session.worker.agent == agent)
            .map(|session| DisplayNames::default().worker(&session.worker))
    };
    let stopped = worker_on("claude-code").context("no Claude Code session")?;
    let fallback = worker_on("codex").context("no Codex session")?;
    assert_eq!(
        said.text,
        format!("{stopped}'s session ended early (its usage limit) · fell back to {fallback}")
    );
    let builds = started
        .iter()
        .find_map(|session| session.workflow_step.as_ref())
        .context("no session works for a step")?;
    assert_eq!(said.workflow.as_ref(), Some(&builds.workflow));
    Ok(())
}

#[test]
fn limits_usage_limit_pauses_then_fails_over() -> anyhow::Result<()> {
    let limits = setup(
        "usage-limit-pauses-then-fails-over",
        "crew.main = \"codex\"\n\n[fallback]\nmode = \"auto\"\norder = [\"claude-code\"]\n",
    )?;
    let pty = limits.start()?;
    let events = limits.wait_for(
        "Claude Code to finish the task",
        LONG_WAIT,
        completed_by("claude-code"),
    )?;

    let ends = ends_of(&events, "codex");
    assert!(
        matches!(
            ends.first().map(|end| &end.reason),
            Some(EndReason::UsageLimit { .. })
        ),
        "{ends:?}"
    );
    let rejected = find(&events, |kind| match kind {
        RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(reading))
            if reading.agent_id.0 == "codex" && reading.status == PlanUsageStatus::Rejected =>
        {
            Some(reading)
        }
        _ => None,
    });
    let reading = rejected
        .first()
        .context("no rejected Codex plan usage reading")?;
    assert!(
        reading.resets_at.is_some(),
        "the reset time was not read from the message"
    );
    assert!(before(
        seq_of(&events, committed),
        seq_of(&events, |kind| matches!(
            kind,
            RunEventKind::Fallback(FallbackEvent::FellBack { .. })
        ))
    ));
    assert!(fell_back_to("claude-code")(&events));
    // The chip keeps its words at any width: a red bar while there's room, then `out`.
    pty.wait_for(r"Codex( +\S+)? +out\b", WAIT)?;
    limits.shot(&pty, "limits_usage_limit_pauses_then_fails_over")
}

#[test]
fn limits_fallback_question_candidates() -> anyhow::Result<()> {
    let limits = setup(
        "fallback-question-candidates",
        "[agents.opencode]\nenabled = false\n\n\
         [fallback]\nmode = \"ask\"\norder = [\"codex\", \"opencode\", \"antigravity-cli\"]\n",
    )?;
    let mut pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;

    assert_eq!(candidate_workers(&question), ["codex", "antigravity-cli"]);
    let plan = fallback_plan(&question)?;
    assert!(
        plan.candidates
            .iter()
            .all(|candidate| !candidate.plan_usage_text.is_empty())
    );
    for option in &question.options {
        if let AnswerAction::Candidate { .. } = option.action {
            pty.wait_for(&regex::escape(&option.label), WAIT)?;
            let detail = option
                .detail
                .as_deref()
                .context("a candidate shows no plan usage or cost")?;
            pty.wait_for(&regex::escape(detail), WAIT)?;
        }
    }
    limits.shot(&pty, "limits_fallback_question_candidates")?;

    let agy = key_for(&question, |action| {
        matches!(action, AnswerAction::Candidate { worker } if worker.agent == "antigravity-cli")
    })
    .context("no key for agy")?;
    answer(&mut pty, &question, agy)?;
    limits.wait_for(
        "agy to finish the task",
        LONG_WAIT,
        completed_by("antigravity-cli"),
    )?;
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    Ok(())
}

#[test]
fn limits_reassign_any_time() -> anyhow::Result<()> {
    reassign_while_working()?;
    reassign_while_waiting()
}

fn reassign_while_working() -> anyhow::Result<()> {
    let limits = setup("reassign-any-time", FALLBACK_CODEX)?;
    let mut pty = limits.start()?;
    let events = limits.wait_for("Claude Code to start", LONG_WAIT, |events| {
        sessions_started(events)
            .iter()
            .any(|started| started.worker.agent == "claude-code")
    })?;
    limits.wait_for_call("claude", "acceptEdits", WAIT)?;
    let task = first_task(&events).context("no task")?;

    pty.send(&format!("/reassign {task} codex\r"))?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;
    let ends = ends_of(&events, "claude-code");
    assert_eq!(
        ends.first().map(|end| &end.reason),
        Some(&EndReason::Stopped)
    );
    assert!(reassigned_by_you(&events, "codex"));
    Ok(())
}

fn reassign_while_waiting() -> anyhow::Result<()> {
    let limits = setup(
        "reassign-waiting",
        &format!("[fallback]\nmode = \"wait\"\n{WAIT_FAST}"),
    )?;
    let mut pty = limits.start()?;
    let events = limits.wait_for("the run to wait", LONG_WAIT, |events| {
        waits(events)
            .iter()
            .any(|reason| matches!(reason, WaitReason::WaitingForAgent { .. }))
    })?;
    let task = first_task(&events).context("no task")?;

    pty.send(&format!("/reassign {task} codex\r"))?;
    let events = limits.wait_for("Codex to finish the task", LONG_WAIT, completed_by("codex"))?;
    assert!(reassigned_by_you(&events, "codex"));
    let waiting = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Run(RunLifecycle::Waiting { .. }))
    });
    let moved = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Task(TaskEvent::Reassigned { .. }))
    });
    assert!(before(waiting, moved));
    limits.shot(&pty, "limits_reassign_any_time")
}

fn reassigned_by_you(events: &[RunEvent], worker: &str) -> bool {
    find(events, |kind| match kind {
        RunEventKind::Task(TaskEvent::Reassigned { to, by, .. }) => Some((to, by)),
        _ => None,
    })
    .iter()
    .any(|(to, by)| to.agent == worker && **by == Party::User)
}

#[test]
fn limits_ask_timeout_while_waiting_falls_back() -> anyhow::Result<()> {
    let limits = setup(
        "ask-timeout-while-waiting",
        &format!(
            "[fallback]\nmode = \"ask\"\nask_timeout = \"5s\"\norder = [\"codex\"]\n{WAIT_FAST}"
        ),
    )?;
    let pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;
    limits.wait_for("the run to wait on the task", LONG_WAIT, |events| {
        waits(events)
            .iter()
            .any(|reason| matches!(reason, WaitReason::Approval))
    })?;

    // No key is pressed: the timeout wakes the run, which falls back by itself.
    let events = limits.wait_for("Codex to finish the task", WAKE_WAIT, completed_by("codex"))?;
    assert!(
        resumes(&events)
            .iter()
            .any(|resumed| resumed.cause == ResumeCause::AskTimeout)
    );
    let withdrawn = find(&events, |kind| match kind {
        RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => Some(id),
        _ => None,
    });
    assert!(
        withdrawn.contains(&&question.id),
        "the fallback question was not withdrawn"
    );
    assert!(fell_back(&events).iter().any(|event| matches!(
        event,
        FallbackEvent::FellBack { to, mode: FallbackMode::Auto, .. } if to.agent == "codex"
    )));
    Ok(())
}

#[test]
fn limits_wait_mode_resumes_when_agent_back() -> anyhow::Result<()> {
    // passes when: a waiting task goes back to Claude Code by resuming its session, or in a
    // fresh session whose fallback note carries the stopped session's last message.
    let limits = setup(
        "wait-mode-resumes",
        &format!("[fallback]\nmode = \"wait\"\norder = [\"codex\"]\n{WAIT_FAST}"),
    )?;
    let pty = limits.start()?;
    let events = limits.wait_for("the run to wait for Claude Code", LONG_WAIT, |events| {
        waits(events).iter().any(|reason| {
            matches!(reason, WaitReason::WaitingForAgent { agent_id, until: Some(_) } if agent_id.0 == "claude-code")
        })
    })?;
    let until = waits(&events)
        .iter()
        .find_map(|reason| match reason {
            WaitReason::WaitingForAgent { until, .. } => *until,
            _ => None,
        })
        .context("no reset time")?;
    assert!(
        find(&events, |kind| match kind {
            RunEventKind::Fallback(FallbackEvent::Waiting { agent_id, .. }) => Some(agent_id),
            _ => None,
        })
        .iter()
        .any(|agent_id| agent_id.0 == "claude-code")
    );

    let events = limits.wait_for("Claude Code to finish the task", WAKE_WAIT, |events| {
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
    assert!(woke.ts >= until);
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    limits.shot(&pty, "limits_wait_mode_resumes_when_agent_back")
}

/// A fallback's whole round trip: a usage limit, a fallback with a checkpoint tag, the fallback's
/// commit, the original's return and review, and the merge.
#[test]
fn limits_fallback_checkpoint_roundtrip() -> anyhow::Result<()> {
    let limits = setup("fallback-checkpoint-roundtrip", FALLBACK_CODEX)?;
    let mut pty = limits.start()?;

    // 1. Claude Code hits its usage limit mid-task.
    let question = limits.wait_for_question(&pty, ApprovalKind::Fallback)?;
    let events = limits.events();
    let ends = ends_of(&events, "claude-code");
    assert!(matches!(
        ends.first().map(|end| &end.reason),
        Some(EndReason::UsageLimit { .. })
    ));

    // 2. You pick Codex on the question, and the tag `fallback-1` exists.
    let codex = key_for(
        &question,
        |action| matches!(action, AnswerAction::Candidate { worker } if worker.agent == "codex"),
    )
    .context("no key for Codex")?;
    answer(&mut pty, &question, codex)?;
    let events = limits.wait_for("the checkpoint tag", LONG_WAIT, |events| {
        seq_of(events, |kind| {
            matches!(
                kind,
                RunEventKind::Task(TaskEvent::CheckpointTagged { n: 1, .. })
            )
        })
        .is_some()
    })?;
    let task_id = first_task(&events).context("no task")?;
    let tags = limits.repo.git(&["tag", "--list", "*/fallback-1"])?;
    assert!(tags.contains(&format!("{task_id}/fallback-1")), "{tags:?}");

    // 3. Codex commits.
    let events = limits.wait_for("Codex's commit", LONG_WAIT, |events| {
        find(events, |kind| match kind {
            RunEventKind::Task(TaskEvent::Committed(info)) => info.by.as_ref(),
            _ => None,
        })
        .iter()
        .any(|by| by.agent == "codex")
    })?;
    assert!(
        find(&events, |kind| match kind {
            RunEventKind::Task(TaskEvent::QualityDebt(debt)) => Some(debt),
            _ => None,
        })
        .iter()
        .any(|debt| debt.by.agent == "codex" && debt.since == 1)
    );

    // 4. Claude Code comes back, and the return question appears. You pick review.
    let back = limits.wait_for_question_within(&pty, ApprovalKind::OriginalReturned, WAKE_WAIT)?;
    let ApprovalPacket::OriginalReturned(packet) = &back.packet else {
        anyhow::bail!("not a return question: {:?}", back.packet);
    };
    assert_eq!(packet.original.agent, "claude-code");
    assert_eq!(packet.fallback.agent, "codex");
    assert_eq!(packet.since, 1);
    assert_eq!(option_keys(&back), ['r', 'o', 'u', 'y', 't', 'd']);
    limits.shot(&pty, "limits_fallback_checkpoint_roundtrip")?;
    answer(&mut pty, &back, 'r')?;

    // 5. The debt clears, and the merge question opens again without it. You merge.
    let events = limits.wait_for("the debt to clear", LONG_WAIT, |events| {
        find(events, |kind| match kind {
            RunEventKind::Task(TaskEvent::DebtCleared { by }) => Some(by),
            _ => None,
        })
        .iter()
        .any(|by| by.agent == "claude-code")
    })?;
    let cleared = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Task(TaskEvent::DebtCleared { .. }))
    })
    .context("no DebtCleared")?;
    let merge_after = |events: &[RunEvent]| {
        events
            .iter()
            .filter(|event| event.seq > cleared)
            .find_map(|event| match &event.kind {
                RunEventKind::Approval(ApprovalEvent::Opened(opened))
                    if opened.kind == ApprovalKind::Merge =>
                {
                    Some(opened.as_ref().clone())
                }
                _ => None,
            })
    };
    let events = limits.wait_for("the merge approval after the review", LONG_WAIT, |events| {
        merge_after(events).is_some()
    })?;
    let merge = merge_after(&events).context("the merge approval went missing")?;
    let ApprovalPacket::Merge(packet) = &merge.packet else {
        anyhow::bail!("not a merge approval: {:?}", merge.packet);
    };
    assert!(
        packet.rows.iter().all(|row| row.debt.is_empty()),
        "{packet:?}"
    );
    pty.wait_for(&regex::escape(&merge.title), WAIT)?;
    answer(&mut pty, &merge, 'y')?;
    pty.press(Key::Enter)?;
    limits.wait_for("the merge", LONG_WAIT, |events| {
        seq_of(events, |kind| {
            matches!(
                kind,
                RunEventKind::Run(RunLifecycle::Finished {
                    outcome: RunOutcome::Merged,
                    ..
                })
            )
        })
        .is_some()
    })?;
    assert!(
        limits.call_with("claude", "dontAsk")?.is_some(),
        "Claude Code never reviewed"
    );
    Ok(())
}

/// A wake the app gave the run by itself: its time came, or the agent it waited for is back.
fn woke_by_itself(cause: ResumeCause) -> bool {
    matches!(cause, ResumeCause::AgentBack | ResumeCause::Timer)
}
