//! Who sees what. Drafting agents never see other drafts until all are sealed; judges never see
//! authors; blind reviewers never see each other's reviews before the judge settles them; hidden
//! tests are never shown; the narrator sees digests only, and narrator lines appear nowhere.

use std::collections::BTreeSet;

use argonauts_core::TaskId;
use argonauts_core::api::StatusSummary;
use argonauts_core::names::{PlanLabel, RoleName};
use argonauts_core::settings::protocol::VisibilitySettings;

use crate::status::Viewer;
use crate::status::folder::Tagged;

/// Who may see one line or task of the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    Everyone,
    /// A sealed draft plan of this label and round: its author, and the other drafting agents once
    /// every draft of the round is sealed.
    Draft {
        label: PlanLabel,
        round: u8,
    },
    /// A blind review of this task, hidden from the task's reviewers until the judge settles them.
    BlindReview(TaskId),
    /// Hidden tests. Never shown to an agent.
    UserOnly,
}

/// The run's progress that the rules depend on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Moment {
    /// Every draft of rounds up to this one is sealed.
    pub sealed_through: u8,
    pub settled: BTreeSet<TaskId>,
}

/// Sessions of a role switched off in `visibility.status_summary` get no summary at all.
pub fn enabled(role: RoleName, rules: &VisibilitySettings) -> bool {
    rules.status_summary.get(&role).copied().unwrap_or(true)
}

pub fn sees(
    viewer: &Viewer,
    audience: &Audience,
    moment: &Moment,
    rules: &VisibilitySettings,
) -> bool {
    let viewer = normalized(viewer);
    match (viewer, audience) {
        (_, Audience::Everyone) | (Viewer::User, _) => true,
        (_, Audience::UserOnly) => false,
        (Viewer::Narrator, _) => true,
        (
            Viewer::Session { role, label, .. },
            Audience::Draft {
                label: draft,
                round,
            },
        ) => {
            *role != RoleName::Draft
                || !rules.drafts_hidden_until_sealed
                || *round <= moment.sealed_through
                || *label == Some(*draft)
        }
        (Viewer::Session { role, task, .. }, Audience::BlindReview(reviewed)) => {
            let fellow_reviewer = *role == RoleName::Reviewer && task.as_ref() == Some(reviewed);
            !fellow_reviewer
                || !rules.reviews_hidden_until_settled
                || moment.settled.contains(reviewed)
        }
    }
}

/// Plan authors: you and the narrator always, judges only with `judges_see_authors`, and no
/// other agent.
pub fn shows_authors(viewer: &Viewer, rules: &VisibilitySettings) -> bool {
    match normalized(viewer) {
        Viewer::User | Viewer::Narrator => true,
        Viewer::Session { role, .. } => *role == RoleName::Judge && rules.judges_see_authors,
    }
}

/// The folded summary as this viewer may see it.
pub fn filter(tagged: &Tagged, viewer: &Viewer, rules: &VisibilitySettings) -> StatusSummary {
    if let Viewer::Session { role, .. } = normalized(viewer)
        && !enabled(*role, rules)
    {
        return StatusSummary {
            run: tagged.run.clone(),
            ..StatusSummary::default()
        };
    }
    let authors = shows_authors(viewer, rules);
    let seen = |audience: &Audience| sees(viewer, audience, &tagged.moment, rules);
    StatusSummary {
        run: tagged.run.clone(),
        goal: tagged.goal.clone(),
        plan: tagged.plan.clone(),
        decisions: tagged.decisions.clone(),
        tasks: tagged
            .tasks
            .iter()
            .filter(|task| seen(&task.audience))
            .map(|task| task.brief.clone())
            .collect(),
        recent: tagged
            .recent
            .iter()
            .chain(&tagged.now)
            .filter(|line| seen(&line.audience))
            .map(|line| match (&line.with_authors, authors) {
                (Some(named), true) => named.clone(),
                _ => line.text.clone(),
            })
            .collect(),
        spend: tagged.spend.clone(),
        plan_windows: tagged.plan_windows.clone(),
        waiting: tagged
            .waiting
            .iter()
            .map(|(_, title)| title.clone())
            .collect(),
    }
}

/// A narrator session is the narrator, whatever the caller called it.
fn normalized(viewer: &Viewer) -> &Viewer {
    match viewer {
        Viewer::Session {
            role: RoleName::Narrator,
            ..
        } => &Viewer::Narrator,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use time::OffsetDateTime;

    use argonauts_core::RunId;
    use argonauts_core::events::plan::PlanEvent;
    use argonauts_core::events::task::{CheckKind, ReviewEvent, TaskEvent, TaskKind, TaskSpec};
    use argonauts_core::events::{RunEvent, RunEventKind};
    use argonauts_core::names::{DraftId, ModelRef};
    use argonauts_core::steps::judgment::{ReviewOutput, ReviewVerdict, SettleOutput};

    use super::*;
    use crate::status::folder::Folder;

    fn event(seq: u64, task: Option<&str>, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: task.map(|task| TaskId::new(task).unwrap()),
            session: None,
            source: "argonauts-code".to_owned(),
            kind,
        }
    }

    fn sealed(seq: u64, label: PlanLabel, worker: &str, round: u8) -> RunEvent {
        event(
            seq,
            None,
            RunEventKind::Plan(PlanEvent::Sealed {
                draft: DraftId::numbered(u32::from(label)),
                label,
                worker: worker.parse().unwrap(),
                round,
                summary: format!("draft {label}"),
                path: PathBuf::from(format!("drafts/{label}.md")),
                sha256: String::new(),
            }),
        )
    }

    fn spec(id: &str, owned: &[&str], hidden: &[&str]) -> TaskSpec {
        TaskSpec {
            id: TaskId::new(id).unwrap(),
            title: format!("{id} title"),
            task: String::new(),
            kind: TaskKind::Build,
            owned_paths: owned.iter().map(|path| (*path).to_owned()).collect(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: hidden.iter().map(|path| (*path).to_owned()).collect(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some(ModelRef::new("codex", None)),
            reviewers: Vec::new(),
            limit: Default::default(),
            predictions: None,
            item: None,
            way: None,
        }
    }

    fn review(seq: u64, task: &str, reviewer: &str) -> RunEvent {
        event(
            seq,
            Some(task),
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer: reviewer.parse().unwrap(),
                blind: true,
                round: 1,
                review: ReviewOutput {
                    verdict: ReviewVerdict::Approve,
                    summary: "fine".to_owned(),
                    comments: Vec::new(),
                    tamper_suspected: false,
                },
                picked_by: None,
            }),
        )
    }

    fn drafting_agent(label: Option<PlanLabel>) -> Viewer {
        Viewer::Session {
            role: RoleName::Draft,
            task: None,
            label,
        }
    }

    fn reviewer(task: &str) -> Viewer {
        Viewer::Session {
            role: RoleName::Reviewer,
            task: Some(TaskId::new(task).unwrap()),
            label: None,
        }
    }

    fn judge() -> Viewer {
        Viewer::Session {
            role: RoleName::Judge,
            task: None,
            label: None,
        }
    }

    fn mentions(summary: &StatusSummary, text: &str) -> bool {
        summary.recent.iter().any(|line| line.contains(text))
    }

    #[test]
    fn status_summary_visibility_rules() {
        let mut folder = Folder::default();
        folder.push(&sealed(0, 1, "claude-code", 1));
        folder.push(&sealed(1, 2, "codex", 1));

        // Drafting agents see only their own draft until every draft is sealed.
        let one = folder.summary(&drafting_agent(Some(1)));
        assert!(mentions(&one, "Plan 1 sealed"));
        assert!(!mentions(&one, "Plan 2"));
        assert!(!mentions(&folder.summary(&drafting_agent(None)), "Plan"));
        // You see both, with their authors.
        let you = folder.summary(&Viewer::User);
        assert!(mentions(&you, "Plan 1 (claude-code) sealed"));
        assert!(mentions(&you, "Plan 2 (codex) sealed"));

        folder.push(&event(
            2,
            None,
            RunEventKind::Plan(PlanEvent::Mapped {
                round: 1,
                worker: "agy".parse().unwrap(),
                topics: Vec::new(),
            }),
        ));
        let one = folder.summary(&drafting_agent(Some(1)));
        assert!(mentions(&one, "Plan 2 sealed"), "{:?}", one.recent);

        // Judges never see authors; the narrator does.
        let judged = folder.summary(&judge());
        assert!(mentions(&judged, "Plan 1 sealed"));
        assert!(!judged.recent.iter().any(|line| line.contains("codex")));
        assert!(mentions(&folder.summary(&Viewer::Narrator), "(codex)"));
        let open = VisibilitySettings {
            judges_see_authors: true,
            ..VisibilitySettings::default()
        };
        let mut open_folder = Folder::new(open);
        open_folder.push(&sealed(0, 1, "codex", 1));
        assert!(mentions(&open_folder.summary(&judge()), "(codex)"));

        // Blind reviewers of a task never see each other's reviews before the judge settles them.
        folder.push(&event(
            3,
            Some("task-002"),
            RunEventKind::Task(TaskEvent::Created(Box::new(spec(
                "task-002",
                &["src/limiter/**"],
                &[],
            )))),
        ));
        folder.push(&review(4, "task-002", "claude-code"));
        assert!(!mentions(
            &folder.summary(&reviewer("task-002")),
            "reviewed"
        ));
        assert!(mentions(
            &folder.summary(&reviewer("task-003")),
            "task-002 reviewed"
        ));
        assert!(mentions(
            &folder.summary(&Viewer::User),
            "task-002 reviewed"
        ));
        folder.push(&event(
            5,
            Some("task-002"),
            RunEventKind::Review(ReviewEvent::Settled {
                judge: "agy".parse().unwrap(),
                ruling: SettleOutput {
                    verdict: ReviewVerdict::Approve,
                    reasons: vec!["both fine".to_owned()],
                    required_changes: Vec::new(),
                },
                picked_by: None,
            }),
        ));
        assert!(mentions(&folder.summary(&reviewer("task-002")), "reviewed"));

        // Hidden tests are never shown to an agent, the narrator included.
        folder.push(&event(
            6,
            Some("test-009"),
            RunEventKind::Task(TaskEvent::Created(Box::new(spec(
                "test-009",
                &["tests/hidden/**"],
                &["tests/hidden/**"],
            )))),
        ));
        folder.push(&event(
            7,
            Some("task-002"),
            RunEventKind::Check(argonauts_core::events::task::CheckOutcome {
                name: "tests/hidden/limits.rs".to_owned(),
                kind: CheckKind::Command,
                passed: false,
                command: None,
                exit_code: Some(1),
                duration_ms: 10,
                summary: "1 failed".to_owned(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            }),
        ));
        let you = folder.summary(&Viewer::User);
        assert!(
            you.tasks
                .iter()
                .any(|task| task.task.as_str() == "test-009")
        );
        assert!(mentions(&you, "tests/hidden"));
        for agent in [
            Viewer::Narrator,
            judge(),
            reviewer("task-003"),
            drafting_agent(Some(1)),
        ] {
            let seen = folder.summary(&agent);
            assert!(
                !seen
                    .tasks
                    .iter()
                    .any(|task| task.task.as_str() == "test-009")
            );
            assert!(
                !mentions(&seen, "tests/hidden"),
                "{agent:?}: {:?}",
                seen.recent
            );
        }

        // A role switched off gets nothing but the run.
        let mut off = VisibilitySettings::default();
        off.status_summary.insert(RoleName::Judge, false);
        let judged = filter(folder.tagged(), &judge(), &off);
        assert!(judged.run.is_some() && judged.tasks.is_empty() && judged.recent.is_empty());
    }
}
