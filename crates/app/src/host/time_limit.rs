//! A run an outside agent started gets `mcp.run_limit` of time from when it was started. Once
//! that's spent it pauses at the next safe point and waits for you, since only you can let it
//! carry on. A run you started, or carried on yourself, has no such limit.

use std::sync::Arc;
use std::time::Duration;

use argonauts_core::NoteLevel;
use argonauts_core::base::clock;
use argonauts_core::settings::Settings;
use time::OffsetDateTime;
use tokio::task::JoinHandle;

use super::relay::Entry;
use super::{Host, note};
use crate::control::ControlRequest;

/// What's left of an outside-agent run's time limit. `None` for a run you started, or a limit of
/// `"none"`.
pub(crate) fn time_left(
    settings: &Settings,
    outside_agent: Option<&str>,
    started: OffsetDateTime,
    now: OffsetDateTime,
) -> Option<Duration> {
    outside_agent?;
    let limit = settings.mcp.run_limit.parse().ok().flatten()?;
    let spent = Duration::try_from(now - started).unwrap_or(Duration::ZERO);
    Some(limit.saturating_sub(spent))
}

impl Host {
    /// In the run host that started an outside-agent run, asks the engine to pause once the time
    /// limit is spent, as `/pause` would. A run already waiting by then has stopped, so it's
    /// left alone. Aborted when the engine is done.
    pub(super) fn pause_when_time_is_spent(&self) -> Option<JoinHandle<()>> {
        if !self.fresh {
            return None;
        }
        let left = time_left(
            &self.settings,
            self.start.request.outside_agent.as_deref(),
            self.start.created,
            clock::now_utc(),
        )?;
        let limit = self.settings.mcp.run_limit.0.clone();
        let state = Arc::clone(&self.state);
        let sink = Arc::clone(&self.sink);
        Some(tokio::spawn(async move {
            tokio::time::sleep(left).await;
            let (entry, _reply) = Entry::new(ControlRequest::Pause {
                now: false,
                outside_agent: None,
            });
            if state.door.submit(entry, false).is_ok() {
                note(
                    &sink,
                    NoteLevel::Warn,
                    format!(
                        "This run's outside-agent time limit of {limit} is spent, so it pauses at \
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

    fn limit(span: &str) -> Settings {
        let mut settings = Settings::default();
        settings.mcp.run_limit = Span::new(span);
        settings
    }

    #[test]
    fn only_a_outside_agent_run_with_a_limit_has_time_left() {
        let started = clock::now_utc();
        let later = started + time::Duration::minutes(10);
        assert_eq!(time_left(&limit("60m"), None, started, later), None);
        assert_eq!(
            time_left(&limit("none"), Some("other-tool"), started, later),
            None
        );
        assert_eq!(
            time_left(&limit("60m"), Some("other-tool"), started, later),
            Some(Duration::from_secs(50 * 60))
        );
        let spent = started + time::Duration::hours(2);
        assert_eq!(
            time_left(&limit("60m"), Some("other-tool"), started, spent),
            Some(Duration::ZERO)
        );
    }
}
