use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::RunId;
use argonauts_core::app::{AppEvent, ReplayTurn};
use argonauts_core::events::agent_status::FallbackEvent;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::run::{Party, ResumeCause, RunLifecycle, RunOutcome, WaitReason};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::task::{TaskEvent, TaskState};
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{MergeEvent, RunEvent, RunEventKind};
use argonauts_core::names::{ApprovalId, MessageId, ModelRef, SessionKey};
use argonauts_core::sanitize::sanitize_for_terminal;

/// Conversation turns a resumed run repeats word for word; older ones are one Ctrl+O away.
const REPLAY_TURNS: usize = 6;

/// What a resumed run prints in place of its history: the last turns word for word, one line
/// per milestone, and `recap`. `None` when there's no history.
pub(crate) fn condensed_replay(
    events: &[RunEvent],
    run: &RunId,
    recap: String,
) -> Option<AppEvent> {
    if events.is_empty() {
        return None;
    }
    let turns = turns(events);
    Some(AppEvent::CondensedReplay {
        run: run.clone(),
        turns: turns[turns.len().saturating_sub(REPLAY_TURNS)..].to_vec(),
        milestones: milestones(events),
        recap,
        recap_model: None,
    })
}

/// What you and the outside agents said to the run, and what its agents asked you. A question
/// still open is left out, since it shows itself.
fn turns(events: &[RunEvent]) -> Vec<ReplayTurn> {
    let mut workers: BTreeMap<SessionKey, ModelRef> = BTreeMap::new();
    let mut seen: BTreeSet<MessageId> = BTreeSet::new();
    let mut open: BTreeMap<ApprovalId, usize> = BTreeMap::new();
    let mut turns = Vec::new();
    let say = |turns: &mut Vec<ReplayTurn>, from: Party, text: &str| {
        let text = sanitize_for_terminal(text.trim());
        if !text.is_empty() {
            turns.push(ReplayTurn::said(from, text));
        }
    };
    for event in events {
        match &event.kind {
            RunEventKind::Run(RunLifecycle::Started(started)) => {
                let from = match &started.outside_agent {
                    Some(client) => Party::OutsideAgent {
                        client: client.clone(),
                    },
                    None => Party::User,
                };
                say(&mut turns, from, &started.request);
            }
            RunEventKind::Session(SessionEvent::Started(started)) => {
                if let Some(key) = &event.session {
                    workers.insert(key.clone(), started.worker.clone());
                }
            }
            RunEventKind::Session(SessionEvent::Queued(message))
                if from_a_person(&message.from) && seen.insert(message.id.clone()) =>
            {
                say(&mut turns, message.from.clone(), &message.text);
            }
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                if let ApprovalPacket::Question(question) = &opened.packet {
                    let from = match &event.session {
                        Some(session) => Party::Agent {
                            session: session.clone(),
                            worker: workers
                                .get(session)
                                .cloned()
                                .unwrap_or_else(|| question.from.clone()),
                        },
                        None => Party::ArgonautsCode,
                    };
                    let at = turns.len();
                    say(&mut turns, from, &question.text);
                    if turns.len() > at {
                        open.insert(opened.id.clone(), at);
                    }
                }
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.remove(&decided.id);
                if from_a_person(&decided.by)
                    && let Some(text) = &decided.text
                {
                    say(&mut turns, decided.by.clone(), text);
                }
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                open.remove(id);
            }
            _ => {}
        }
    }
    // Turns only grow until here, so each question's place still holds it, and going from the
    // last keeps the earlier places right.
    let mut asked: Vec<usize> = open.into_values().collect();
    asked.sort_unstable();
    for at in asked.into_iter().rev() {
        if at < turns.len() {
            turns.remove(at);
        }
    }
    turns
}

fn from_a_person(party: &Party) -> bool {
    matches!(party, Party::User | Party::OutsideAgent { .. })
}

/// One line per milestone, oldest first, such as "Plan approved · 5 tasks".
pub(crate) fn milestones(events: &[RunEvent]) -> Vec<String> {
    let mut opened: BTreeMap<ApprovalId, (ApprovalKind, usize)> = BTreeMap::new();
    let mut titles: BTreeMap<String, String> = BTreeMap::new();
    let mut lines = Vec::new();
    for event in events {
        let task = event.task_id.as_ref().map(ToString::to_string);
        let line = match &event.kind {
            RunEventKind::Goal(GoalEvent::Approved { .. }) => Some("Goal approved".to_owned()),
            RunEventKind::Approval(ApprovalEvent::Opened(approval)) => {
                opened.insert(approval.id.clone(), opened_entry(approval));
                None
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                match (opened.get(&decided.id), &decided.action) {
                    (
                        Some((ApprovalKind::Plan, tasks)),
                        AnswerAction::Approve | AnswerAction::ApproveWithEdits,
                    ) => Some(format!("Plan approved · {}", count(*tasks, "task"))),
                    _ => None,
                }
            }
            RunEventKind::Task(TaskEvent::Created(spec)) => {
                titles.insert(spec.id.to_string(), sanitize_for_terminal(&spec.title));
                None
            }
            RunEventKind::Task(TaskEvent::State { to, .. }) => {
                let task = task.unwrap_or_default();
                let title = titles
                    .get(&task)
                    .map(|title| format!(" · {title}"))
                    .unwrap_or_default();
                match to {
                    TaskState::Merged => Some(format!("{task} merged into the run{title}")),
                    TaskState::Failed => Some(format!("{task} failed{title}")),
                    TaskState::NeedsYou => Some(format!("{task} needs you{title}")),
                    _ => None,
                }
            }
            RunEventKind::Task(TaskEvent::Rewound { to, .. }) => Some(format!(
                "{} rewound to fallback-{to}",
                task.unwrap_or_default()
            )),
            RunEventKind::Fallback(FallbackEvent::FellBack { task, to, .. }) => {
                Some(format!("{task} fell back to {to}"))
            }
            RunEventKind::Merge(MergeEvent::FinalCheck { passed }) => Some(
                if *passed {
                    "Final check passed"
                } else {
                    "Final check failed"
                }
                .to_owned(),
            ),
            RunEventKind::Merge(MergeEvent::RunMerged { target, .. }) => {
                Some(format!("Merged into {}", sanitize_for_terminal(target)))
            }
            RunEventKind::Merge(MergeEvent::PullRequest { url, .. }) => Some(match url {
                Some(url) => format!("Pull request opened · {}", sanitize_for_terminal(url)),
                None => "Pull request suggested".to_owned(),
            }),
            RunEventKind::Workflow(WorkflowEvent::Changed(change)) => Some(format!(
                "Workflow changed · {}",
                sanitize_for_terminal(&change.reason)
            )),
            // A stream that starts here never saw these open, so a later answer still counts.
            RunEventKind::Run(RunLifecycle::Snapshot(snapshot)) => {
                for approval in &snapshot.open_approvals {
                    opened.insert(approval.id.clone(), opened_entry(approval));
                }
                None
            }
            RunEventKind::Run(lifecycle) => lifecycle_line(lifecycle),
            _ => None,
        };
        lines.extend(line);
    }
    lines
}

/// The approval's kind, and a plan's task count.
fn opened_entry(approval: &ApprovalOpened) -> (ApprovalKind, usize) {
    let tasks = match &approval.packet {
        ApprovalPacket::Plan(plan) => plan.graph.tasks.len(),
        _ => 0,
    };
    (approval.kind, tasks)
}

fn lifecycle_line(lifecycle: &RunLifecycle) -> Option<String> {
    match lifecycle {
        RunLifecycle::Resumed(resumed) => Some(
            match resumed.cause {
                ResumeCause::User => "Resumed",
                ResumeCause::Answer => "Resumed for your answer",
                ResumeCause::AgentBack => "Resumed as an agent came back",
                ResumeCause::AskTimeout => "Resumed as a fallback question timed out",
                ResumeCause::Crash => "Resumed after a crash",
            }
            .to_owned(),
        ),
        RunLifecycle::ApprovalModeChanged { mode, .. } => {
            Some(format!("Approval mode set to {}", mode.label()))
        }
        RunLifecycle::Waiting { reason, .. } => Some(format!(
            "Waiting · {}",
            match reason {
                WaitReason::Approval => "waiting for you".to_owned(),
                WaitReason::Paused => "paused".to_owned(),
                WaitReason::AllAgentsOut { .. } => "every agent is out".to_owned(),
                WaitReason::WaitingForAgent { agent_id, .. } => format!("waiting for {agent_id}"),
                WaitReason::NoFallback { detail, .. } => detail.clone(),
            }
        )),
        RunLifecycle::Finished { outcome, .. } => Some(
            match outcome {
                RunOutcome::Merged => "Merged",
                RunOutcome::PullRequest => "Finished with a pull request",
                RunOutcome::BranchLeft => "Finished with its branches kept",
                RunOutcome::Abandoned => "Abandoned",
                RunOutcome::Stopped => "Stopped",
                RunOutcome::Failed => "Failed",
            }
            .to_owned(),
        ),
        _ => None,
    }
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{
        approval_decided, event, plan_approval, queued, run_id, run_started, task_event,
    };
    use argonauts_core::events::run::RunResumed;

    #[test]
    fn milestones_are_one_line_each_oldest_first() {
        let task = argonauts_core::TaskId::new("task-001").unwrap();
        let plan = ApprovalId::numbered(1);
        let events = vec![
            run_started(0, "Add --json to export", None),
            plan_approval(1, &plan, 5),
            approval_decided(2, &plan, AnswerAction::Approve, Party::User, None),
            task_event(
                3,
                &task,
                TaskEvent::State {
                    from: Some(TaskState::Merging),
                    to: TaskState::Merged,
                    reason: "merged".to_owned(),
                },
            ),
            event(
                4,
                RunEventKind::Run(RunLifecycle::Waiting {
                    reason: WaitReason::Paused,
                    waiting_on: Vec::new(),
                }),
            ),
            event(
                5,
                RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
                    cause: ResumeCause::User,
                    settings_changed: false,
                    cli_versions_changed: Vec::new(),
                    main_moved_by: None,
                    sessions: Vec::new(),
                })),
            ),
        ];
        assert_eq!(
            milestones(&events),
            [
                "Plan approved · 5 tasks",
                "task-001 merged into the run",
                "Waiting · paused",
                "Resumed"
            ]
        );
    }

    #[test]
    fn a_plan_open_at_the_snapshot_is_approved_after_it() {
        let plan = ApprovalId::numbered(1);
        let RunEventKind::Approval(ApprovalEvent::Opened(opened)) = plan_approval(1, &plan, 5).kind
        else {
            unreachable!()
        };
        let snapshot = argonauts_core::events::run::RunSnapshot {
            upto: 1,
            engine: serde_json::Value::Null,
            summary: argonauts_core::api::StatusSummary::default(),
            open_approvals: vec![*opened],
            running: Vec::new(),
        };
        let events = vec![
            event(
                2,
                RunEventKind::Run(RunLifecycle::Snapshot(Box::new(snapshot))),
            ),
            approval_decided(3, &plan, AnswerAction::Approve, Party::User, None),
        ];
        assert_eq!(milestones(&events), ["Plan approved · 5 tasks"]);
    }

    #[test]
    fn no_condensed_replay_without_history() {
        assert_eq!(
            condensed_replay(&[], &run_id(), "※ nothing yet".to_owned()),
            None
        );
    }

    #[test]
    fn a_condensed_replay_repeats_the_last_turns_word_for_word() {
        let plan = ApprovalId::numbered(1);
        let mut events = vec![run_started(0, "Add --json to export", Some("hermes"))];
        for seq in 1..=7 {
            events.push(queued(seq, &format!("note {seq}"), Party::User));
        }
        events.push(plan_approval(8, &plan, 2));
        events.push(approval_decided(
            9,
            &plan,
            AnswerAction::Approve,
            Party::User,
            Some("ship it"),
        ));
        let Some(AppEvent::CondensedReplay {
            run,
            turns,
            milestones,
            recap,
            recap_model,
        }) = condensed_replay(&events, &run_id(), "※ recap".to_owned())
        else {
            panic!("expected a condensed replay");
        };
        assert_eq!(run, run_id());
        let texts: Vec<_> = turns.iter().map(|turn| turn.text.as_str()).collect();
        assert_eq!(
            texts,
            ["note 3", "note 4", "note 5", "note 6", "note 7", "ship it"]
        );
        assert_eq!(milestones, ["Plan approved · 2 tasks"]);
        assert_eq!((recap.as_str(), recap_model), ("※ recap", None));

        let short = condensed_replay(&events[..2], &run_id(), String::new()).unwrap();
        let AppEvent::CondensedReplay { turns, .. } = short else {
            unreachable!()
        };
        assert_eq!(
            turns[0].from,
            Party::OutsideAgent {
                client: "hermes".to_owned()
            }
        );
        assert_eq!(turns[0].text, "Add --json to export");
    }

    /// An agent's question still open isn't repeated above where it asks; once
    /// answered, the question and your answer read as turns.
    #[test]
    fn an_open_question_is_left_to_its_task() {
        use argonauts_core::events::approval::{FreeText, QuestionPacket};
        use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};

        let id = ApprovalId::numbered(1);
        let task = argonauts_core::TaskId::new("task-001").unwrap();
        let opened = ApprovalOpened {
            id: id.clone(),
            kind: ApprovalKind::Question,
            title: "Question from build:greet".to_owned(),
            task: Some(task.clone()),
            packet: ApprovalPacket::Question(QuestionPacket {
                from: ModelRef::new("codex", None),
                task,
                text: "Keep the old name as an alias?".to_owned(),
                options: Vec::new(),
            }),
            evidence_sha256: String::new(),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::Answer,
            needs_you: true,
        };
        let mut events = vec![
            run_started(0, "Rename the config loader", None),
            event(
                1,
                RunEventKind::Approval(ApprovalEvent::Opened(Box::new(opened))),
            ),
            queued(2, "rename it everywhere", Party::User),
        ];
        let texts = |events: &[RunEvent]| -> Vec<String> {
            turns(events).into_iter().map(|turn| turn.text).collect()
        };
        assert_eq!(
            texts(&events),
            ["Rename the config loader", "rename it everywhere"]
        );
        events.push(approval_decided(
            3,
            &id,
            AnswerAction::Answer,
            Party::User,
            Some("yes, keep it"),
        ));
        assert_eq!(
            texts(&events),
            [
                "Rename the config loader",
                "Keep the old name as an alias?",
                "rename it everywhere",
                "yes, keep it"
            ]
        );
    }
}
