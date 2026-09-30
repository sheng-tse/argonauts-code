//! The run engine: triage, planning, card pipelines, integration, and the status summary.

use argonauts_core::engine::{EngineContext, EngineExit, RunEngine};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome};
use argonauts_core::future::BoxFuture;
use argonauts_core::settings::Settings;

pub const ENGINE_NOT_BUILT: &str = "crew engine not built yet";

pub fn engine_for(settings: &Settings, mode: RunMode) -> Box<dyn RunEngine> {
    let _ = settings;
    Box::new(PlaceholderEngine { mode })
}

/// Records a failed finish so a run ends visibly instead of hanging.
struct PlaceholderEngine {
    mode: RunMode,
}

impl RunEngine for PlaceholderEngine {
    fn run(self: Box<Self>, context: EngineContext) -> BoxFuture<'static, EngineExit> {
        Box::pin(async move {
            let detail = format!("{ENGINE_NOT_BUILT} ({:?} run)", self.mode).to_lowercase();
            let finished = RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::Failed,
                detail: Some(detail),
            });
            match context.sink.record(None, None, "conductor", finished) {
                Ok(_) => EngineExit::Finished(RunOutcome::Failed),
                Err(err) => EngineExit::Crashed(err.to_string()),
            }
        })
    }
}

pub mod status;
pub mod triage;

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::steps::intake::ChangeEstimate;

    #[test]
    fn the_placeholder_triage_says_it_is_not_real() {
        let estimate = ChangeEstimate {
            task_type: "docs".into(),
            summary: "fix the README".into(),
            files_likely: vec!["README.md".into()],
            file_count: 1,
            risk_labels: Vec::new(),
            tests_exist: true,
            unknowns: Vec::new(),
        };
        let decision = triage::decide(&estimate, &Settings::default());
        assert_eq!(decision.rule, triage::NOT_IMPLEMENTED);
        assert!(decision.reason.contains("not implemented"));
    }

    #[test]
    fn the_placeholder_status_summary_says_so() {
        let summary = status::summary(&[], &status::Viewer::User);
        assert_eq!(summary.recent, [status::NOT_IMPLEMENTED]);
        assert!(status::render(&summary).contains("not implemented"));
    }
}
