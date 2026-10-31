//! A task's agent that ends its turn with a question instead of its report.
//! Nobody reads the end of a turn, so the task would fail or stall on "Should I proceed?".
//! While the run decides without you the agent is told to decide and carry on, in its own
//! session, `approvals.ask_limit` times, and then once to take the option it recommended.
//!
//! What an agent decided in your place, here or on a question you left with Esc, comes back in
//! its report's concerns on lines that start with `Assumed:`. The run says each one once, so
//! an answer nobody gave is recorded as an assumption you can review.

use argonauts_core::base::names::{ModelRef, SessionKey};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::session::{EndReason, InputPart};
use argonauts_core::workflows::steps::asking;
use argonauts_core::workflows::steps::task::TaskReport;
use argonauts_core::{Note, NoteLevel, TaskId};

use super::session::Ended;
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::engine::shared::lock;

/// What the agent reads in place of an answer, when its session ended with a question and can
/// take a reply. `None` leaves the end to be what it was: a report that doesn't match.
pub(crate) fn reply(
    host: &Host,
    task: &TaskId,
    worker: &ModelRef,
    ended: &Ended,
) -> EngineResult<Option<InputPart>> {
    let resumes = ended.end.cli_session.is_some()
        && host
            .services()
            .workers
            .get(worker)
            .is_ok_and(|model| model.capabilities().resume);
    if ended.end.reason != EndReason::Completed || !resumes {
        return Ok(None);
    }
    let Some(limit) = host.ask_limit().map(u32::from) else {
        return Ok(None);
    };
    // Counted for the task, since a resumed session may go by a new key.
    let asked_before = lock(&host.shared().asked_anyway)
        .get(task)
        .copied()
        .unwrap_or(0);
    let Some(mut reply) = asking::reply(&ended.end, asked_before, Some(limit)) else {
        return Ok(None);
    };
    // A task's report has a place for it, which the run reads back for you to review.
    reply.text.push_str(
        " List each thing you assumed in its `concerns`, on a line starting with \"Assumed:\".",
    );
    lock(&host.shared().asked_anyway).insert(task.clone(), asked_before + 1);
    let told = if asked_before >= limit {
        "to take the option it recommended"
    } else {
        "to decide and carry on"
    };
    let label = host.read(|state| crate::sessions::labels::task_label(state, task));
    let note = Note {
        level: NoteLevel::Info,
        text: format!("{label} ended its turn with a question, so it was told {told}."),
    };
    host.record(Some(task), Some(&ended.key), RunEventKind::Note(note))?;
    Ok(Some(reply))
}

/// The task reported: its next turn may ask afresh, and what its agent says it assumed is said
/// once, in one line. Its concerns carry the same to the closing answer.
pub(crate) fn on_report(
    host: &Host,
    task: &TaskId,
    session: &SessionKey,
    report: &TaskReport,
) -> EngineResult<()> {
    lock(&host.shared().asked_anyway).remove(task);
    let assumed = report.assumptions();
    if assumed.is_empty() {
        return Ok(());
    }
    let label = host.read(|state| crate::sessions::labels::task_label(state, task));
    let note = Note {
        level: NoteLevel::Info,
        text: format!("{label} assumed: {}", assumed.join("; ")),
    };
    host.record(Some(task), Some(session), RunEventKind::Note(note))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::workflows::steps::task::ReportStatus;

    use super::*;

    fn report(concerns: &[&str]) -> TaskReport {
        TaskReport {
            status: ReportStatus::Done,
            summary: "Built it.".to_owned(),
            files_changed: Vec::new(),
            tests_run: Vec::new(),
            concerns: concerns
                .iter()
                .map(|concern| (*concern).to_owned())
                .collect(),
            questions: Vec::new(),
            change_requests: Vec::new(),
            memory_proposals: Vec::new(),
            answer: String::new(),
        }
    }

    #[test]
    fn assumptions_are_the_concerns_marked_as_assumed() {
        let reported = report(&[
            "Assumed: dates without a year mean this year.",
            "The store has no pool yet.",
            "- assumed: ISO dates only\nAssumed:",
        ]);
        assert_eq!(
            reported.assumptions(),
            ["dates without a year mean this year", "ISO dates only"]
        );
        assert!(
            report(&["Nothing was assumed here."])
                .assumptions()
                .is_empty()
        );
    }
}
