//! One worker session on a task: started fresh or resumed, its work committed when it ends
//! whatever the reason, and its report read.

use argonauts_core::TaskId;
use argonauts_core::events::session::{EndReason, InputPart, SessionEnd};
use argonauts_core::events::task::TryCause;
use argonauts_core::limits::FallbackNote;
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::Access;
use argonauts_core::steps::task::{Build, TaskReport};
use argonauts_core::steps::{StepName, last_json_object, output_schema};

use crate::engine::host::{Fallbacks, Host, Ran, SessionPlan};
use crate::error::EngineResult;
use crate::task::worktree::{self, TaskTree};
use crate::task::{at_start, prompt};
use crate::workflow::StepAt;

/// What the next worker session is for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Next {
    /// A fresh session: the first, or after a failure, a fallback, a reassignment, or a
    /// rotation. All but the first get the fallback note, built from the log unless `note`
    /// brings one.
    Fresh {
        cause: TryCause,
        why: String,
        note: Option<Box<FallbackNote>>,
        /// Such as your note on a retry.
        extra: Vec<InputPart>,
    },
    /// The task's own session again, given these parts.
    Resume {
        cause: TryCause,
        parts: Vec<InputPart>,
    },
}

impl Next {
    pub(crate) fn fresh(cause: TryCause, why: impl Into<String>) -> Self {
        Self::Fresh {
            cause,
            why: why.into(),
            note: None,
            extra: Vec::new(),
        }
    }

    pub(crate) fn resume(cause: TryCause, parts: Vec<InputPart>) -> Self {
        Self::Resume { cause, parts }
    }

    pub(crate) fn cause(&self) -> TryCause {
        match self {
            Self::Fresh { cause, .. } | Self::Resume { cause, .. } => *cause,
        }
    }
}

/// Causes that continue the same session instead of starting a fresh one.
pub(crate) fn same_session(cause: TryCause) -> bool {
    matches!(
        cause,
        TryCause::CheckRetry
            | TryCause::ReviewChanges
            | TryCause::GoalRetry
            | TryCause::MergeFailed
            | TryCause::Resumed
    )
}

/// A finished worker session.
#[derive(Debug, Clone)]
pub(crate) struct Ended {
    pub key: SessionKey,
    pub end: SessionEnd,
    pub report: Option<TaskReport>,
}

pub(crate) struct SessionCall<'a> {
    pub task: &'a TaskId,
    pub worker: &'a ModelRef,
    pub tree: &'a TaskTree,
    pub at: &'a StepAt,
    pub next: Next,
    /// The session a `Resume` continues.
    pub resume: Option<SessionKey>,
    /// Tells this session apart from the task's other worker sessions, for replay. `None`
    /// takes the task's own next slot, which stays the same while its session can resume.
    pub slot: Option<u32>,
}

/// Starts (or, after a crash, resumes) the session, waits for its end, and commits its work.
pub(crate) async fn run(host: &Host, call: SessionCall<'_>) -> EngineResult<Ended> {
    let SessionCall {
        task,
        worker,
        tree,
        at,
        next,
        resume,
        slot,
    } = call;
    // A task started over gets a session of its own, whatever its role keeps for the task.
    let starts_over = next.cause() == TryCause::Restarted;
    let (parts, resume) = match next {
        Next::Fresh {
            cause,
            why,
            note,
            extra,
        } => {
            // A run with no planning step reaches its first task before anything checked the
            // code it started from, and a worker is told what already fails there as a
            // planner is.
            at_start::ensure(host).await?;
            let mut parts = prompt::worker_parts(host, task)?;
            if cause != TryCause::First {
                let note = note.map(|p| *p);
                let told = prompt::note_part(host, task, worker, cause, &why, note).await?;
                parts.extend(told);
            }
            parts.extend(extra);
            (parts, None)
        }
        // A worker without a status tool gets the summary from the engine at every start,
        // resumes included.
        Next::Resume { parts, .. } => (parts, resume),
    };
    let mut plan = SessionPlan {
        at: Some(at.clone()),
        role: RoleName::Worker,
        worker: worker.clone(),
        task: Some(task.clone()),
        access: Access::Build,
        step: Some(StepName::Build),
        workdir: tree.path.clone(),
        parts,
        json_schema: Some(output_schema::<Build>()),
        resume,
        slot: 0,
        label: None,
        reviews: false,
        fresh: starts_over,
    };
    plan.slot = slot.unwrap_or_else(|| host.slot_for(&plan));
    // A CLI error has the worker go again once on its own session; when it fails again, the
    // task's recovery falls back.
    let Ran {
        key,
        worker: model,
        end,
    } = host.run_session(plan, Fallbacks::None).await?;
    worktree::commit_in(host, task, tree, Some(&key), Some(model)).await?;
    let report = report_of(&end);
    Ok(Ended { key, end, report })
}

/// The report a completed session ended with, when it matches the schema: its structured
/// output, or else the last JSON object in its final text, for agents with no schema flag.
pub(crate) fn report_of(end: &SessionEnd) -> Option<TaskReport> {
    if end.reason != EndReason::Completed {
        return None;
    }
    let mut value = end
        .structured
        .clone()
        .filter(serde_json::Value::is_object)
        .or_else(|| {
            [&end.result_text, &end.last_message]
                .into_iter()
                .flatten()
                .find_map(|text| last_json_object(text))
        })?;
    // A report in the 0.0.1 shape leaves out the lists added since, and an empty list says
    // the same thing.
    if let Some(report) = value.as_object_mut() {
        for list in ["questions", "change_requests", "memory_proposals"] {
            report
                .entry(list)
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        }
    }
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::Spend;
    use serde_json::json;

    fn end(reason: EndReason, structured: Option<serde_json::Value>) -> SessionEnd {
        SessionEnd {
            cli_session: Some("v-1".into()),
            exit_code: Some(0),
            signal: None,
            duration_ms: 10,
            reason,
            spend: Spend::default(),
            tool_calls: 3,
            result_text: None,
            structured,
            last_message: None,
            raw_log: None,
        }
    }

    fn report() -> serde_json::Value {
        json!({
            "status": "done", "summary": "built it", "files_changed": ["src/a.rs"],
            "tests_run": [], "concerns": [], "questions": [], "change_requests": [],
            "memory_proposals": []
        })
    }

    #[test]
    fn only_a_completed_session_with_a_valid_report_has_one() {
        let done = report_of(&end(EndReason::Completed, Some(report()))).unwrap();
        assert_eq!(done.summary, "built it");
        assert!(report_of(&end(EndReason::Completed, None)).is_none());
        assert!(report_of(&end(EndReason::Completed, Some(json!({"status": "done"})))).is_none());
        assert!(report_of(&end(EndReason::Stopped, Some(report()))).is_none());
        let first_shape = json!({
            "status": "done", "summary": "built it", "files_changed": [], "tests_run": [],
            "concerns": []
        });
        let done = report_of(&end(EndReason::Completed, Some(first_shape.clone()))).unwrap();
        assert!(done.questions.is_empty() && done.memory_proposals.is_empty());
        let mut in_text = end(EndReason::Completed, None);
        in_text.result_text = Some(format!("Built it.\n\n```json\n{first_shape}\n```"));
        assert_eq!(report_of(&in_text).unwrap().summary, "built it");
    }

    #[test]
    fn retries_and_resumes_keep_the_session() {
        for cause in [
            TryCause::CheckRetry,
            TryCause::ReviewChanges,
            TryCause::MergeFailed,
            TryCause::Resumed,
        ] {
            assert!(same_session(cause), "{cause:?}");
        }
        for cause in [
            TryCause::First,
            TryCause::AfterFailure,
            TryCause::Fallback,
            TryCause::Reassigned,
            TryCause::Restarted,
            TryCause::Rotation,
        ] {
            assert!(!same_session(cause), "{cause:?}");
        }
    }
}
