//! When the run waits: nothing can happen until something outside changes. Never because time
//! passed, so a waiting run never expires.

use time::OffsetDateTime;

use argonauts_core::events::approval::ApprovalPacket;
use argonauts_core::events::run::WaitReason;
use argonauts_core::names::ApprovalId;

use crate::state::RunState;
use crate::state::approvals::ApprovalRecord;

/// Why the run should wait now, if it should: an approval needs you and no session runs, you
/// paused, every candidate is out (`limits.all_out`), or the remaining tasks wait for an agent.
/// A question about an agent your settings already wait for comes after that wait, so the run
/// wakes when the agent is tried again whether you answered or not.
pub fn reason(
    state: &RunState,
    live_sessions: usize,
    all_out: Option<WaitReason>,
) -> Option<WaitReason> {
    if state.run.finished.is_some() {
        return None;
    }
    if state.run.paused {
        return Some(WaitReason::Paused);
    }
    if live_sessions > 0 {
        return None;
    }
    let asks: Vec<&ApprovalRecord> = state.approvals.open().filter(|r| needs_you(r)).collect();
    if asks.iter().any(|record| !waits_anyway(record)) {
        return Some(WaitReason::Approval);
    }
    if all_out.is_some() {
        return all_out;
    }
    let wait = state
        .run
        .agent_waits
        .values()
        .min_by_key(|wait| wait.until.unwrap_or(i64::MAX));
    match wait {
        Some(wait) => Some(WaitReason::WaitingForAgent {
            agent_id: wait.agent_id.clone(),
            until: wait
                .until
                .and_then(|until| OffsetDateTime::from_unix_timestamp(until).ok()),
        }),
        // With no wait behind it, the question is all that is left to move the run.
        None => (!asks.is_empty()).then_some(WaitReason::Approval),
    }
}

/// The question about a CLI that is down, asked while a setting of yours has its tasks wait
/// for it: the wait goes on by itself.
fn waits_anyway(record: &ApprovalRecord) -> bool {
    matches!(&record.opened.packet, ApprovalPacket::AgentDown(down) if down.kept_by.is_some())
}

/// The approvals a waiting run waits on.
pub fn waiting_on(state: &RunState) -> Vec<ApprovalId> {
    state
        .approvals
        .open()
        .filter(|record| needs_you(record))
        .map(|record| record.opened.id.clone())
        .collect()
}

/// Opened and waiting for you: the policy said so when it opened it, or has spoken since and
/// left it open (it asked you, or the question needed you). One the policy is still deciding
/// doesn't make the run wait.
fn needs_you(record: &ApprovalRecord) -> bool {
    record.opened.needs_you || record.checks.iter().any(|check| !check.auto_preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::TaskId;
    use argonauts_core::events::approval::ApprovalKind;
    use argonauts_core::names::AgentId;

    use crate::state::run::AgentWait;
    use crate::state::tests::open_approval;

    fn with_approval(needs_you: bool) -> RunState {
        let mut state = RunState::default();
        let record = open_approval(1, ApprovalKind::Understand, needs_you);
        state
            .approvals
            .by_id
            .insert(record.opened.id.clone(), record);
        state
    }

    #[test]
    fn an_approval_waits_only_when_it_needs_you_and_nothing_runs() {
        let state = with_approval(true);
        assert_eq!(reason(&state, 0, None), Some(WaitReason::Approval));
        assert_eq!(reason(&state, 1, None), None, "a session still runs");
        assert_eq!(waiting_on(&state).len(), 1);
        let deciding = with_approval(false);
        assert_eq!(
            reason(&deciding, 0, None),
            None,
            "the policy is still deciding"
        );
    }

    #[test]
    fn pause_all_out_and_agent_waits_wait_in_that_order() {
        let mut state = RunState::default();
        assert_eq!(reason(&state, 0, None), None);
        let out = WaitReason::AllAgentsOut { until: None };
        assert_eq!(reason(&state, 0, Some(out.clone())), Some(out.clone()));
        assert_eq!(reason(&state, 2, Some(out)), None);

        let agent_id = AgentId("codex".to_owned());
        state.run.agent_waits.insert(
            TaskId::numbered(2),
            AgentWait {
                agent_id: agent_id.clone(),
                until: Some(3_600),
            },
        );
        match reason(&state, 0, None) {
            Some(WaitReason::WaitingForAgent { agent_id: v, until }) => {
                assert_eq!(v, agent_id);
                assert_eq!(until.map(OffsetDateTime::unix_timestamp), Some(3_600));
            }
            other => panic!("{other:?}"),
        }

        state.run.paused = true;
        assert_eq!(reason(&state, 3, None), Some(WaitReason::Paused));
        state.run.finished = Some(argonauts_core::events::run::RunOutcome::Stopped);
        assert_eq!(reason(&state, 0, None), None, "a finished run never waits");
    }

    /// The question about a CLI your settings wait for is listed, yet the run waits for the
    /// CLI, so it wakes when the CLI is tried again. Only with no wait behind it does the run
    /// wait for you.
    #[test]
    fn a_question_your_settings_already_answer_never_holds_the_wait() {
        use argonauts_core::events::approval::AgentDownPacket;

        let claude = AgentId("claude-code".to_owned());
        let mut record = open_approval(1, ApprovalKind::AgentDown, true);
        record.opened.packet = ApprovalPacket::AgentDown(AgentDownPacket {
            agent_id: claude.clone(),
            evidence: "two of its models are out".to_owned(),
            test_at: None,
            paused: Vec::new(),
            fits_all: None,
            kept_by: Some("same_cli".to_owned()),
        });
        let mut state = RunState::default();
        state
            .approvals
            .by_id
            .insert(record.opened.id.clone(), record);
        assert_eq!(reason(&state, 0, None), Some(WaitReason::Approval));

        let wait = AgentWait {
            agent_id: claude.clone(),
            until: Some(3_600),
        };
        state.run.agent_waits.insert(TaskId::numbered(1), wait);
        assert!(
            matches!(
                reason(&state, 0, None),
                Some(WaitReason::WaitingForAgent { agent_id, .. }) if agent_id == claude
            ),
            "it waits for the CLI"
        );
        assert_eq!(waiting_on(&state).len(), 1, "and still shows the question");

        let other = open_approval(2, ApprovalKind::Merge, true);
        state.approvals.by_id.insert(other.opened.id.clone(), other);
        assert_eq!(reason(&state, 0, None), Some(WaitReason::Approval));
    }
}
