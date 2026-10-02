//! Hard limits no mode or model overrides: at each decision point, on each card's diff (the
//! built-in gates, run before the command gates), and on each session's sandbox. When a check
//! can't tell, it reports a violation.

pub mod deps;
pub mod diff;
pub mod paths;
pub mod point;
pub mod secrets;
pub mod session;
pub mod tamper;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use argonauts_core::events::card::CardSpec;
use argonauts_core::policy::{DecisionPoint, EvidencePacket, HardLimit, HardLimits, Violation};
use argonauts_core::worker::{SessionSpec, WorkerDirectory};

use crate::approvals::settings::SettingsSource;

pub struct BuiltinLimits {
    source: SettingsSource,
    /// Tells a harness session from an endpoint one.
    workers: Arc<dyn WorkerDirectory>,
}

impl BuiltinLimits {
    pub fn new(source: SettingsSource, workers: Arc<dyn WorkerDirectory>) -> Self {
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

    fn check_card_diff(&self, card: &CardSpec, unified_diff: &str) -> Vec<Violation> {
        let settings = self.source.current();
        let changes = match diff::parse(unified_diff) {
            Ok(changes) => changes,
            Err(err) => {
                return vec![violation(
                    HardLimit::GatesFailed,
                    format!("the card's diff can't be read ({err}), so no gate can pass"),
                    Vec::new(),
                )];
            }
        };
        let mut violations = paths::check(card, &changes, &settings);
        violations.extend(secrets::check(&changes));
        violations.extend(tamper::check(&changes));
        violations.extend(deps::check(&changes, &settings));
        violations
    }

    fn check_session(&self, spec: &SessionSpec) -> Vec<Violation> {
        session::check(spec, &self.source.current(), self.workers.as_ref())
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
