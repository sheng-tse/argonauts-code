//! Hard limits no mode or model overrides: at each decision point, on each task's diff (the
//! built-in checks, run before the command checks), and on each session's sandbox. When a check
//! can't tell, it reports a violation.

pub mod daily;
pub mod deps;
pub mod diff;
pub mod paths;
pub mod point;
pub mod secrets;
pub mod session;
pub mod tamper;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;

use argonauts_core::agents::session_runner::{RunnerDirectory, SessionSpec};
use argonauts_core::runs::events::task::TaskSpec;
use argonauts_core::safety::policy::{
    DecisionPoint, EvidencePacket, HardLimit, HardLimits, Violation,
};

use crate::approvals::settings::SettingsSource;

pub struct BuiltinLimits {
    source: SettingsSource,
    /// Tells a CLI session from an endpoint one.
    workers: Arc<dyn RunnerDirectory>,
}

impl BuiltinLimits {
    pub fn new(source: SettingsSource, workers: Arc<dyn RunnerDirectory>) -> Self {
        Self { source, workers }
    }
}

impl HardLimits for BuiltinLimits {
    fn check_point(&self, point: &DecisionPoint, evidence: &EvidencePacket) -> Vec<Violation> {
        point::check(
            point,
            evidence,
            &self.source.current(),
            self.source.run_start(),
        )
    }

    fn check_task_diff(&self, task: &TaskSpec, unified_diff: &str) -> Vec<Violation> {
        self.task_diff(task, unified_diff, None)
    }

    fn check_task_tree_diff(
        &self,
        task: &TaskSpec,
        unified_diff: &str,
        tree: &Path,
    ) -> Vec<Violation> {
        self.task_diff(task, unified_diff, Some(tree))
    }

    fn check_session(&self, spec: &SessionSpec) -> Vec<Violation> {
        session::check(spec, &self.source.current(), self.workers.as_ref())
    }
}

impl BuiltinLimits {
    fn task_diff(
        &self,
        task: &TaskSpec,
        unified_diff: &str,
        tree: Option<&Path>,
    ) -> Vec<Violation> {
        let settings = self.source.current();
        let changes = match diff::parse(unified_diff) {
            Ok(changes) => changes,
            Err(err) => {
                return vec![violation(
                    HardLimit::ChecksFailed,
                    format!("the task's diff can't be read ({err}), so no check can pass"),
                    Vec::new(),
                )];
            }
        };
        let mut violations = paths::check(task, &changes, &settings, tree);
        violations.extend(secrets::check(&changes));
        violations.extend(tamper::check(&changes));
        violations.extend(deps::check(&changes, &settings));
        violations
    }
}

pub(crate) fn violation(
    limit: HardLimit,
    detail: impl Into<String>,
    paths: Vec<String>,
) -> Violation {
    Violation {
        limit,
        detail: detail.into(),
        paths,
    }
}
