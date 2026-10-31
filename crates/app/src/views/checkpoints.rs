use argonauts_core::TaskId;
use argonauts_core::app::views::CheckpointRow;
use argonauts_core::runs::events::task::TaskEvent;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::safety::sanitize::sanitize_for_terminal;

/// `/rewind`: each fallback checkpoint of `task`, or of every task, oldest first.
pub(crate) fn checkpoint_rows(events: &[RunEvent], task: Option<&TaskId>) -> Vec<CheckpointRow> {
    let mut rows: Vec<CheckpointRow> = Vec::new();
    for event in events {
        let Some(this_task) = &event.task_id else {
            continue;
        };
        if task.is_some_and(|wanted| wanted != this_task) {
            continue;
        }
        match &event.kind {
            RunEventKind::Task(TaskEvent::CheckpointTagged {
                n,
                commit,
                from,
                to,
                reason,
            }) => rows.push(CheckpointRow {
                task: this_task.clone(),
                n: *n,
                at: event.ts,
                commit: commit.clone(),
                from: from.clone(),
                to: to.clone(),
                reason: sanitize_for_terminal(reason),
                rewound_here: false,
            }),
            RunEventKind::Task(TaskEvent::Rewound { to, .. }) => {
                if let Some(row) = rows
                    .iter_mut()
                    .rev()
                    .find(|row| row.task == *this_task && row.n == *to)
                {
                    row.rewound_here = true;
                }
            }
            _ => {}
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::task_event;
    use argonauts_core::base::names::ModelRef;

    fn tagged(seq: u64, task: &TaskId, n: u32, to: &str) -> RunEvent {
        task_event(
            seq,
            task,
            TaskEvent::CheckpointTagged {
                n,
                commit: "0123456789abcdef".to_owned(),
                from: ModelRef::new("claude-code", Some("opus".to_owned())),
                to: vec![ModelRef::new(to, None)],
                reason: "usage limit, resets at 3:45 PM".to_owned(),
            },
        )
    }

    #[test]
    fn checkpoint_rows_list_each_fallback_oldest_first() {
        let export = TaskId::new("task-001").unwrap();
        let docs = TaskId::new("task-002").unwrap();
        let events = vec![
            tagged(0, &export, 1, "codex"),
            tagged(1, &docs, 1, "antigravity-cli"),
            tagged(2, &export, 2, "opencode"),
            task_event(
                3,
                &export,
                TaskEvent::Rewound {
                    to: 1,
                    kept_on: "argonauts/run/task-001-replaced-1".to_owned(),
                },
            ),
        ];
        let all = checkpoint_rows(&events, None);
        assert_eq!(all.len(), 3);
        let mine = checkpoint_rows(&events, Some(&export));
        let picked: Vec<(u32, &str, bool)> = mine
            .iter()
            .map(|row| (row.n, row.to[0].agent.as_str(), row.rewound_here))
            .collect();
        assert_eq!(picked, [(1, "codex", true), (2, "opencode", false)]);
        assert_eq!(mine[0].from.to_string(), "claude-code/opus");
        assert_eq!(mine[0].reason, "usage limit, resets at 3:45 PM");
        assert!(checkpoint_rows(&events, Some(&TaskId::new("task-003").unwrap())).is_empty());
    }
}
