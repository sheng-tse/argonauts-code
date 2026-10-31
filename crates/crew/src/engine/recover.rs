//! Waking a run: after a crash, after you answered a waiting run's approval, when an agent is
//! back, when an `ask_timeout` fallback runs out, or when you reopen a stopped run.

use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{AgentId, ApprovalId};
use argonauts_core::models::limits::RecoveryAction;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{ApprovalKind, ApprovalPacket};
use argonauts_core::runs::events::run::{
    ResumeCause, ResumedSession, RunLifecycle, RunOutcome, RunResumed, WaitReason,
};
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::state::RunState;
use crate::{approvals, fallback, task};

/// Records `Run::Resumed` with what changed (settings hash, CLI versions, main moved) and
/// why the run woke, then applies the cause: `AgentBack` retries the agent or sends one test
/// task; `AskTimeout` withdraws that fallback approval and falls back to the next valid
/// candidate within the cost cap, or reopens it when there is none. Sessions that were running
/// are resumed when their steps are re-entered, never restarted. `answers` says a request
/// queued for this engine answers an approval; a run that waited on one and woke without an
/// answer, as Shift+Tab or guidance wakes it, woke for you.
pub async fn recover(host: &Host, answers: bool) -> EngineResult<()> {
    let now = host.now().unix_timestamp();
    let ask_timeout = host
        .settings()
        .fallback
        .ask_timeout
        .parse()
        .ok()
        .flatten()
        .map(|timeout| i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX));
    let (cause, timed_out) = host.read(|state| cause(state, now, ask_timeout));
    let Some(mut cause) = cause else {
        return Ok(());
    };
    let back = agents_back(host);
    // The app wakes a run waiting on its merge question once an original that question's debt waits
    // for is back, which the log alone can't tell from an answer.
    if cause == ResumeCause::Answer && host.read(|state| merge_waits_on(state, &back)) {
        cause = ResumeCause::AgentBack;
    }
    if cause == ResumeCause::AgentBack
        && host.read(|state| woke_for_you(state.run.waiting.as_ref(), &back, now))
    {
        cause = ResumeCause::Answer;
    }
    if cause == ResumeCause::Answer && !answers {
        cause = ResumeCause::User;
    }
    // Nothing it waited for is back and its time hasn't come, so you carried it on.
    if cause == ResumeCause::AgentBack && back.is_empty() {
        cause = ResumeCause::User;
    }
    // A crash, whether or not a pause was under way: the log ends before the run waited or
    // finished. A stop committed its sessions' work already.
    let crashed = host.read(|state| state.run.waiting.is_none() && state.run.finished.is_none());
    let (started, sessions) = host.read(|state| {
        let sessions: Vec<ResumedSession> = state
            .sessions
            .in_order()
            .into_iter()
            .filter(|session| session.resumable())
            .map(|session| ResumedSession {
                session: session.key.clone(),
                task: session.task.clone(),
                cli_session: session.cli_session.clone(),
                resumed: session.cli_session.is_some(),
            })
            .collect();
        (state.run.started.clone(), sessions)
    });

    // A crash can leave a building session's work uncommitted; commit it before anything
    // else touches the worktree.
    if crashed {
        for session in &sessions {
            let builds = host.read(|state| {
                state.sessions.get(&session.session).is_some_and(|record| {
                    matches!(record.started.access, Access::Build | Access::Experiment)
                })
            });
            if let (true, Some(task)) = (builds, &session.task) {
                task::worktree::commit_after_end(host, task, &session.session).await?;
            }
        }
    }

    let main_moved_by = match &started {
        Some(started) => {
            main_moved_by(host, started.base_branch.as_deref(), &started.base_commit).await?
        }
        None => None,
    };
    let resumed = RunResumed {
        cause,
        settings_changed: started
            .as_ref()
            .is_some_and(|started| started.settings_sha256 != host.shared().settings_sha256),
        // The run host probes CLI versions; the engine only sees what the run started with.
        cli_versions_changed: Vec::new(),
        main_moved_by,
        sessions,
    };
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::Resumed(resumed)),
    )?;

    match cause {
        ResumeCause::AgentBack | ResumeCause::Timer => {
            for agent_id in back {
                fallback::ask::agent_back(host, &agent_id)?;
                fallback::on_return::agent_back(host, &agent_id).await?;
            }
        }
        ResumeCause::AskTimeout => {
            for (id, task) in timed_out {
                ask_timed_out(host, &id, task.as_ref()).await?;
            }
        }
        ResumeCause::User | ResumeCause::Answer | ResumeCause::Crash => {}
    }
    // The engine that asked about a CLI that is down is gone, so this one takes the question up.
    fallback::down::ask_again(host)
}

/// A run that waits for an agent also wakes for your answer or a message. With nobody back
/// and the next try still ahead, its tasks go on waiting, which a resume for an agent's return
/// would forget.
fn woke_for_you(waiting: Option<&WaitReason>, back: &[AgentId], now: i64) -> bool {
    let Some(WaitReason::WaitingForAgent { until, .. }) = waiting else {
        return false;
    };
    back.is_empty() && until.is_none_or(|until| until.unix_timestamp() > now)
}

/// Why the run woke, from how its log ends, and the fallback approvals whose ask timed out.
/// `None` for a new run, or one that finished other than stopped.
pub fn cause(
    state: &RunState,
    now: i64,
    ask_timeout: Option<i64>,
) -> (Option<ResumeCause>, Vec<(ApprovalId, Option<TaskId>)>) {
    match state.run.finished {
        // Only a stopped run you reopened gets an engine that goes on.
        Some(RunOutcome::Stopped) => return (Some(ResumeCause::User), Vec::new()),
        Some(_) => return (None, Vec::new()),
        None => {}
    }
    let timed_out: Vec<_> = state
        .approvals
        .open()
        .filter(|record| record.opened.kind == ApprovalKind::Fallback)
        .filter(|record| {
            let opened = state.run.opened_at.get(&record.opened.id).copied();
            matches!((opened, ask_timeout), (Some(opened), Some(timeout)) if now - opened >= timeout)
        })
        .map(|record| (record.opened.id.clone(), record.opened.task.clone()))
        .collect();
    let cause =
        match &state.run.waiting {
            Some(WaitReason::Paused) => ResumeCause::User,
            Some(WaitReason::Approval) if !timed_out.is_empty() => ResumeCause::AskTimeout,
            Some(WaitReason::Approval) => ResumeCause::Answer,
            // A run that stopped fully wakes for nothing but your `/resume`.
            Some(WaitReason::NoFallback { .. }) => ResumeCause::User,
            // An outage you chose to wait out: its time came, or an agent came back.
            Some(
                WaitReason::AllAgentsOut { until } | WaitReason::WaitingForAgent { until, .. },
            ) if until.is_some_and(|until| until.unix_timestamp() <= now) => ResumeCause::Timer,
            Some(WaitReason::AllAgentsOut { .. } | WaitReason::WaitingForAgent { .. }) => {
                ResumeCause::AgentBack
            }
            // Only the run host's `RunStarted`: a new run, nothing to recover.
            None if state.seq.unwrap_or(0) == 0 => return (None, Vec::new()),
            // You paused it, and the engine stopped before the run waited.
            None if state.run.paused || state.run.pause_requested.is_some() => ResumeCause::User,
            None => ResumeCause::Crash,
        };
    (Some(cause), timed_out)
}

/// The run waits on an open merge question whose debt needs one of `back`.
fn merge_waits_on(state: &RunState, back: &[AgentId]) -> bool {
    let Some(WaitReason::Approval) = &state.run.waiting else {
        return false;
    };
    let Some(open) = state.approvals.open_for(ApprovalKind::Merge, None) else {
        return false;
    };
    let ApprovalPacket::Merge(packet) = &open.opened.packet else {
        return false;
    };
    packet
        .rows
        .iter()
        .flat_map(|row| &row.debt)
        .any(|debt| back.contains(&debt.needs.agent_id()))
}

/// Agents the log saw out that the boards now call healthy.
fn agents_back(host: &Host) -> Vec<AgentId> {
    let mut out: Vec<AgentId> = host.read(|state| {
        let waiting = state
            .run
            .agent_waits
            .values()
            .map(|wait| wait.agent_id.clone());
        let out = state
            .run
            .health
            .keys()
            .chain(state.run.plan_usage.keys())
            .filter(|agent_id| state.run.agent_out(agent_id))
            .cloned();
        waiting.chain(out).collect()
    });
    out.sort();
    out.dedup();
    let services = host.services();
    let now = host.now();
    out.retain(|agent_id| {
        let health = services.health.state(agent_id);
        let plan_usage_ok = services
            .plan_usage
            .get(agent_id)
            .is_none_or(|reading| !fallback::rules::plan_usage_out(&reading, now));
        matches!(
            health.state,
            argonauts_core::models::limits::HealthState::Healthy
                | argonauts_core::models::limits::HealthState::Testing
        ) && plan_usage_ok
    });
    out
}

/// Nobody answered the fallback question in time: withdraw it and give the task to the first
/// candidate that needs no confirmation, or open the question again when there is none.
async fn ask_timed_out(host: &Host, id: &ApprovalId, task: Option<&TaskId>) -> EngineResult<()> {
    let plan = host.read(|state| {
        state
            .approvals
            .by_id
            .get(id)
            .and_then(|record| match &record.opened.packet {
                ApprovalPacket::Fallback(plan) => Some(plan.clone()),
                _ => None,
            })
    });
    let (Some(plan), Some(task)) = (plan, task) else {
        return Ok(());
    };
    let Some(next) = plan
        .candidates
        .iter()
        .find(|candidate| !candidate.needs_confirm)
    else {
        // Nobody fits within the cost cap, so the question stays open for you.
        return Ok(());
    };
    approvals::withdraw(
        host,
        id,
        "nobody answered in the time /fallback allows, so the next agent in line took it",
    )?;
    let spec = host.read(|state| state.tasks.get(task).map(|record| record.spec.clone()));
    let Some(spec) = spec else {
        return Ok(());
    };
    let events = task::support::task_events(host, task)?;
    let note = host.services().fallback.note(&spec, &events);
    let action = RecoveryAction::Fallback {
        to: next.worker.clone(),
        note: Box::new(note),
        checkpoint: None,
    };
    fallback::apply(host, task, action).await
}

/// How many commits the base branch gained since the run started, if any. When git can't tell,
/// a note says why and the count is left out.
async fn main_moved_by(host: &Host, branch: Option<&str>, base: &str) -> EngineResult<Option<u32>> {
    let Some(branch) = branch else {
        return Ok(None);
    };
    let root = &host.repo().root;
    let git = host.git();
    let counted = async {
        let head = crate::merge::final_check::main_commit(host, branch).await?;
        if head == base {
            return Ok(0);
        }
        let moved = git.log_messages(root, &format!("{base}..{head}")).await?;
        Ok::<_, crate::git::GitError>(moved.len())
    };
    match counted.await {
        Ok(0) => Ok(None),
        Ok(moved) => Ok(Some(u32::try_from(moved).unwrap_or(u32::MAX))),
        Err(err) => {
            let note = Note {
                level: NoteLevel::Warn,
                text: format!("couldn't tell how far {branch} moved since the run started: {err}"),
            };
            host.record(None, None, RunEventKind::Note(note))?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::runs::events::approval::ApprovalKind;

    use crate::state::tests::open_approval;

    #[test]
    fn the_cause_follows_how_the_log_ends() {
        let mut state = RunState::default();
        assert_eq!(cause(&state, 0, None).0, None, "an empty log");
        state.seq = Some(0);
        assert_eq!(cause(&state, 0, None).0, None, "only RunStarted");
        state.seq = Some(9);
        assert_eq!(cause(&state, 0, None).0, Some(ResumeCause::Crash));
        state.run.pause_requested = Some(true);
        assert_eq!(
            cause(&state, 0, None).0,
            Some(ResumeCause::User),
            "a pause under way"
        );
        state.run.pause_requested = None;
        state.run.waiting = Some(WaitReason::Paused);
        assert_eq!(cause(&state, 0, None).0, Some(ResumeCause::User));
        state.run.waiting = Some(WaitReason::AllAgentsOut { until: None });
        assert_eq!(cause(&state, 0, None).0, Some(ResumeCause::AgentBack));
        // Each try records what started it: its time came, or you carried it on.
        let at = time::OffsetDateTime::from_unix_timestamp(60).unwrap();
        state.run.waiting = Some(WaitReason::AllAgentsOut { until: Some(at) });
        assert_eq!(cause(&state, 60, None).0, Some(ResumeCause::Timer));
        state.run.waiting = Some(WaitReason::NoFallback {
            detail: "nobody to fall back to as the task's worker".into(),
            until: Some(at),
            stop: None,
        });
        assert_eq!(
            cause(&state, 60, None).0,
            Some(ResumeCause::User),
            "a run that stopped fully wakes only for you"
        );
        state.run.waiting = Some(WaitReason::Approval);
        assert_eq!(cause(&state, 0, None).0, Some(ResumeCause::Answer));

        let fallback = open_approval(4, ApprovalKind::Fallback, true);
        let id = fallback.opened.id.clone();
        state.approvals.by_id.insert(id.clone(), fallback);
        state.run.opened_at.insert(id.clone(), 100);
        assert_eq!(cause(&state, 150, Some(60)).0, Some(ResumeCause::Answer));
        let (woke, timed_out) = cause(&state, 160, Some(60));
        assert_eq!(woke, Some(ResumeCause::AskTimeout));
        assert_eq!(timed_out, vec![(id, None)]);
        assert_eq!(cause(&state, 10_000, None).0, Some(ResumeCause::Answer));

        state.run.finished = Some(argonauts_core::runs::events::run::RunOutcome::Merged);
        assert_eq!(cause(&state, 0, None).0, None);
    }

    #[test]
    fn a_waiting_merge_task_waits_on_the_originals_its_debt_needs() {
        use argonauts_core::runs::events::approval::{MergePacket, MergeRow};
        use argonauts_core::runs::events::task::TaskKind;
        use argonauts_core::runs::fallback::QualityDebt;
        use argonauts_core::{DiffStat, Spend};

        let worker = |name: &str| name.parse().expect("a model");
        let row = MergeRow {
            task: TaskId::numbered(1),
            title: "Redis store".to_owned(),
            kind: TaskKind::Build,
            stat: DiffStat::default(),
            worker: Some(worker("codex/gpt-5.6")),
            checks_passed: true,
            review: None,
            debt: vec![QualityDebt {
                since: 1,
                by: worker("codex/gpt-5.6"),
                needs: worker("claude-code/opus"),
            }],
            questions: Vec::new(),
        };
        let mut record = open_approval(2, ApprovalKind::Merge, true);
        record.opened.packet = ApprovalPacket::Merge(MergePacket {
            target: Some("main".to_owned()),
            branch: "argonauts/run-01/merged".to_owned(),
            rows: vec![row],
            suite_passed_on_latest_main: true,
            checks: Vec::new(),
            combined: DiffStat::default(),
            conductor: DiffStat::default(),
            diff_path: std::path::PathBuf::new(),
            tree_sha: String::new(),
            spent: Spend::default(),
            limit: None,
            partial: false,
            in_the_way: Vec::new(),
            edited: Vec::new(),
            refused: None,
            both_changed: Vec::new(),
            asks_which_branch: false,
            again: None,
        });
        let mut state = RunState::default();
        state
            .approvals
            .by_id
            .insert(record.opened.id.clone(), record);
        let claude = AgentId("claude-code".to_owned());
        let codex = AgentId("codex".to_owned());

        assert!(
            !merge_waits_on(&state, std::slice::from_ref(&claude)),
            "not waiting"
        );
        state.run.waiting = Some(WaitReason::Approval);
        assert!(merge_waits_on(&state, std::slice::from_ref(&claude)));
        assert!(!merge_waits_on(&state, &[codex]));
        assert!(!merge_waits_on(&state, &[]));
    }

    /// A run that waits for an agent and wakes with nobody back, before its next try, woke
    /// for you. Once the try is due, or someone is back, it woke for the agent.
    #[test]
    fn a_wake_before_the_next_try_with_nobody_back_is_for_you() {
        let claude = AgentId("claude-code".to_owned());
        let until = time::OffsetDateTime::from_unix_timestamp(3_600).ok();
        let waits = WaitReason::WaitingForAgent {
            agent_id: claude.clone(),
            until,
        };
        assert!(woke_for_you(Some(&waits), &[], 1_800));
        assert!(!woke_for_you(Some(&waits), &[], 3_600), "the try is due");
        assert!(!woke_for_you(
            Some(&waits),
            std::slice::from_ref(&claude),
            1_800
        ));
        let open_ended = WaitReason::WaitingForAgent {
            agent_id: claude,
            until: None,
        };
        assert!(woke_for_you(Some(&open_ended), &[], 1_800));
        assert!(!woke_for_you(Some(&WaitReason::Approval), &[], 1_800));
        assert!(!woke_for_you(None, &[], 1_800));
    }
}
