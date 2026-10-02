//! A run a front end started gets `mcp.run_budget` of time from when it was started. Once
//! that's spent it pauses at the next safe point and waits for you, since only you can let it
//! carry on. A run you started, or carried on yourself, has no such limit.

use std::sync::Arc;
use std::time::Duration;

use argonauts_core::NoteLevel;
use argonauts_core::control::ControlRequest;
use argonauts_core::settings::Settings;
use time::OffsetDateTime;
use tokio::task::JoinHandle;

use super::relay::Entry;
use super::{Host, note};

/// What's left of a front-end run's time budget. `None` for a run you started, or a budget of
/// `"none"`.
pub(crate) fn time_left(
    settings: &Settings,
    front_end: Option<&str>,
    started: OffsetDateTime,
    now: OffsetDateTime,
) -> Option<Duration> {
    front_end?;
    let budget = settings.mcp.run_budget.parse().ok().flatten()?;
    let spent = Duration::try_from(now - started).unwrap_or(Duration::ZERO);
    Some(budget.saturating_sub(spent))
}

impl Host {
    /// In the supervisor that started a front-end run, asks the engine to pause once the time
    /// budget is spent, as `/pause` would. A run already parked by then has stopped, so it's
    /// left alone. Aborted when the engine is done.
    pub(super) fn pause_when_time_is_spent(&self) -> Option<JoinHandle<()>> {
        if !self.fresh {
            return None;
        }
        let left = time_left(
            &self.settings,
            self.start.request.front_end.as_deref(),
            self.start.created,
            OffsetDateTime::now_utc(),
        )?;
        let budget = self.settings.mcp.run_budget.0.clone();
        let state = Arc::clone(&self.state);
        let sink = Arc::clone(&self.sink);
        Some(tokio::spawn(async move {
            tokio::time::sleep(left).await;
            let (entry, _reply) = Entry::new(ControlRequest::Pause {
                now: false,
                front_end: None,
            });
            if state.door.submit(entry, false).is_ok() {
                note(
                    &sink,
                    NoteLevel::Warn,
                    format!(
                        "This run's front-end time budget of {budget} is spent, so it pauses at \
                         the next safe point. Resume it in the app to let it carry on."
                    ),
                );
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::Span;

    use super::*;

    fn budget(span: &str) -> Settings {
        let mut settings = Settings::default();
        settings.mcp.run_budget = Span::new(span);
        settings
    }

    #[test]
    fn only_a_front_end_run_with_a_budget_has_time_left() {
        let started = OffsetDateTime::now_utc();
        let later = started + time::Duration::minutes(10);
        assert_eq!(time_left(&budget("60m"), None, started, later), None);
        assert_eq!(
            time_left(&budget("none"), Some("other-tool"), started, later),
            None
        );
        assert_eq!(
            time_left(&budget("60m"), Some("other-tool"), started, later),
            Some(Duration::from_secs(50 * 60))
        );
        let spent = started + time::Duration::hours(2);
        assert_eq!(
            time_left(&budget("60m"), Some("other-tool"), started, spent),
            Some(Duration::ZERO)
        );
    }
}
