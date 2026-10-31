//! One model's breaker on its CLI. An overload or a server error counts against the model it
//! came from: [`OUT_AFTER`] in a row take that model out, and the CLI's other models stay usable.
//! After the cool-down the model is under test, where the next session on it decides. Trouble that
//! comes and goes (retries, reconnects) only marks it degraded until it has been quiet for the
//! window. The rules are plain state, so they're tested without files or clocks.

use std::time::Duration;

use argonauts_core::base::names::{AgentId, ModelRef};
use argonauts_core::models::limits::{HealthSignal, ModelHealth, ModelState, Trouble};
use argonauts_core::settings::sessions::HealthSettings;
use argonauts_core::{ErrorCategory, RetryReason};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::boards::health::{TEST_TASK_LIMIT, cooldown, is_agent_trouble, window};

/// Overload or server errors in a row that take a model out: the failure and its retry.
pub const OUT_AFTER: usize = 2;

/// Models out at once that take their whole CLI down.
pub const MODELS_OUT_TO_DOWN: usize = 2;

/// Marks kept when `health.window` is `none`, so the file stays small.
const MAX_MARKS: usize = 64;

/// What a session said of its model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seen {
    /// A session ended on an overload or a server error.
    Outage(Trouble),
    /// The CLI lost its connection and connected again.
    Reconnected,
    /// Another retry or passing error.
    Trouble,
}

/// One thing counted against a model, kept so every process counts the same ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMark {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub seen: Seen,
}

/// One model's breaker as `agents.json` keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
    pub state: ModelState,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    pub reason: Option<String>,
    pub trouble: Option<Trouble>,
    /// When it's looked at again: its test task while out, the end of the quiet window while
    /// degraded.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub next_check_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<ModelMark>,
}

impl Default for ModelRecord {
    fn default() -> Self {
        Self {
            state: ModelState::Healthy,
            since: OffsetDateTime::UNIX_EPOCH,
            reason: None,
            trouble: None,
            next_check_at: None,
            marks: Vec::new(),
        }
    }
}

/// What a signal says of the model it came from. `None` for one that says nothing of it, such as
/// an account error.
pub fn seen(signal: &HealthSignal) -> Option<Result<(), Seen>> {
    match signal {
        HealthSignal::Success { .. } => Some(Ok(())),
        // A sign-in retry counts against the account, as sign-in, never as the model's trouble.
        HealthSignal::Retry { reason, .. } if reason.is_sign_in() => None,
        HealthSignal::Retry { reason, .. } => Some(Err(match reason {
            RetryReason::Network => Seen::Reconnected,
            _ => Seen::Trouble,
        })),
        HealthSignal::Error { category, .. } => match Trouble::of(*category) {
            Some(trouble) => Some(Err(Seen::Outage(trouble))),
            None if is_agent_trouble(*category) => Some(Err(match category {
                ErrorCategory::Network | ErrorCategory::Timeout => Seen::Reconnected,
                _ => Seen::Trouble,
            })),
            None => None,
        },
        HealthSignal::AdapterBroken { .. } => None,
    }
}

impl ModelRecord {
    pub fn is_out(&self) -> bool {
        self.state == ModelState::Out
    }

    /// Nothing is held against it, so its entry needn't be kept.
    pub fn is_clean(&self) -> bool {
        self.state == ModelState::Healthy && self.marks.is_empty()
    }

    pub fn health(&self, agent_id: &AgentId, model: &str) -> ModelHealth {
        let model = (!model.is_empty()).then(|| model.to_owned());
        ModelHealth {
            model: ModelRef::new(agent_id.0.clone(), model),
            state: self.state,
            since: self.since,
            reason: self.reason.clone(),
            trouble: self.trouble,
            next_test_at: self.next_check_at,
        }
    }

    /// Counts what a session said. `true` when the state changed.
    pub fn observe(
        &mut self,
        said: Result<(), Seen>,
        settings: &HealthSettings,
        now: OffsetDateTime,
    ) -> bool {
        match (self.state, said) {
            // Out waits for its test, so what sessions that were already running say isn't
            // counted.
            (ModelState::Out, _) => false,
            (ModelState::Testing, Ok(())) => self.pass(now),
            (ModelState::Testing, Err(Seen::Outage(trouble))) => {
                self.go_out(trouble, "the test task failed".to_owned(), settings, now)
            }
            (ModelState::Testing, Err(_)) => false,
            (ModelState::Healthy | ModelState::Degraded, Ok(())) => {
                // A success ends the row of failures.
                self.marks
                    .retain(|mark| !matches!(mark.seen, Seen::Outage(_)));
                self.recover_if_quiet(settings, now)
            }
            (ModelState::Healthy | ModelState::Degraded, Err(seen)) => {
                self.marks.push(ModelMark { at: now, seen });
                self.prune(settings, now);
                self.count(settings, now)
            }
        }
    }

    /// Moves a model whose time has come: out to under test after the cool-down, a test nobody
    /// finished back to out, and degraded to healthy once it has been quiet. `true` when the
    /// state changed.
    pub fn tick(&mut self, settings: &HealthSettings, now: OffsetDateTime) -> bool {
        match self.state {
            ModelState::Out if self.next_check_at.is_none_or(|at| at <= now) => {
                self.set(
                    ModelState::Testing,
                    now,
                    Some("the cool-down is over".to_owned()),
                );
                true
            }
            ModelState::Testing if self.since + TEST_TASK_LIMIT <= now => {
                let minutes = TEST_TASK_LIMIT.as_secs() / 60;
                let trouble = self.trouble.unwrap_or(Trouble::Overload);
                let reason = format!("no test task finished within {minutes} minutes");
                self.go_out(trouble, reason, settings, now)
            }
            ModelState::Degraded => self.recover_if_quiet(settings, now),
            ModelState::Healthy | ModelState::Out | ModelState::Testing => false,
        }
    }

    /// A session on the model worked while its CLI was under test, so the model is fine too.
    pub fn pass(&mut self, now: OffsetDateTime) -> bool {
        let changed = self.state != ModelState::Healthy;
        self.marks.clear();
        self.trouble = None;
        self.set(
            ModelState::Healthy,
            now,
            Some(argonauts_core::models::limits::TEST_TASK_SUCCEEDED.to_owned()),
        );
        changed
    }

    fn count(&mut self, settings: &HealthSettings, now: OffsetDateTime) -> bool {
        let outages: Vec<Trouble> = self
            .marks
            .iter()
            .filter_map(|mark| match mark.seen {
                Seen::Outage(trouble) => Some(trouble),
                Seen::Reconnected | Seen::Trouble => None,
            })
            .collect();
        if let Some(latest) = outages.last().copied()
            && outages.len() >= OUT_AFTER
        {
            let kind = match latest {
                Trouble::Overload => "overload",
                Trouble::ServerErrors => "server",
            };
            let reason = format!("{} {kind} errors in a row", outages.len());
            return self.go_out(latest, reason, settings, now);
        }
        let threshold = settings.degraded_after.max(1) as usize;
        if self.marks.len() < threshold {
            return false;
        }
        // Each new mark moves the quiet window on, whether or not the state changes.
        self.next_check_at = window(settings).map(|window| now + window);
        if self.state == ModelState::Degraded {
            return false;
        }
        let reason = self.degraded_reason(settings);
        self.state = ModelState::Degraded;
        self.since = now;
        self.reason = Some(reason);
        true
    }

    /// `reconnected twice in 2 minutes`, or `3 errors or retries in 2 minutes`.
    fn degraded_reason(&self, settings: &HealthSettings) -> String {
        let count = self.marks.len();
        let span = match window(settings) {
            Some(window) => format!(" in {}", minutes_in_words(window)),
            None => String::new(),
        };
        if self.marks.iter().all(|mark| mark.seen == Seen::Reconnected) {
            let times = match count {
                1 => "once".to_owned(),
                2 => "twice".to_owned(),
                n => format!("{n} times"),
            };
            format!("reconnected {times}{span}")
        } else {
            format!("{count} errors or retries{span}")
        }
    }

    fn go_out(
        &mut self,
        trouble: Trouble,
        reason: String,
        settings: &HealthSettings,
        now: OffsetDateTime,
    ) -> bool {
        let changed = self.state != ModelState::Out;
        self.marks.clear();
        self.trouble = Some(trouble);
        self.set(ModelState::Out, now, Some(reason));
        self.next_check_at = Some(now + cooldown(settings));
        changed
    }

    fn recover_if_quiet(&mut self, settings: &HealthSettings, now: OffsetDateTime) -> bool {
        if self.state != ModelState::Degraded {
            return false;
        }
        self.prune(settings, now);
        if self.marks.len() >= settings.degraded_after.max(1) as usize {
            return false;
        }
        self.set(
            ModelState::Healthy,
            now,
            Some("the errors stopped".to_owned()),
        );
        true
    }

    /// Forgets marks older than the window. A window of `none` keeps the latest few.
    fn prune(&mut self, settings: &HealthSettings, now: OffsetDateTime) {
        if let Some(window) = window(settings) {
            self.marks.retain(|mark| mark.at > now - window);
        }
        let extra = self.marks.len().saturating_sub(MAX_MARKS);
        self.marks.drain(..extra);
    }

    fn set(&mut self, state: ModelState, now: OffsetDateTime, reason: Option<String>) {
        self.state = state;
        self.since = now;
        self.reason = reason;
        self.next_check_at = None;
    }
}

/// `2 minutes`, `a minute`, `90 seconds`.
fn minutes_in_words(span: Duration) -> String {
    let seconds = span.as_secs();
    match (seconds / 60, seconds % 60) {
        (1, 0) => "a minute".to_owned(),
        (minutes, 0) if minutes > 0 => format!("{minutes} minutes"),
        _ => format!("{seconds} seconds"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration as Delta;

    fn t0() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    fn settings() -> HealthSettings {
        HealthSettings::default()
    }

    const OVERLOAD: Result<(), Seen> = Err(Seen::Outage(Trouble::Overload));
    const SERVER: Result<(), Seen> = Err(Seen::Outage(Trouble::ServerErrors));

    /// Claude Code retrying a sign-in that failed says nothing of the model.
    #[test]
    fn a_sign_in_retry_is_no_trouble_of_the_model() {
        let retry = |reason: RetryReason| HealthSignal::Retry {
            agent_id: AgentId("claude-code".to_owned()),
            model: None,
            session: argonauts_core::base::names::SessionKey::numbered(1),
            reason,
        };
        let auth = retry(RetryReason::Other("authentication_failed".into()));
        assert_eq!(seen(&auth), None);
        let other = retry(RetryReason::Other("something else".into()));
        assert_eq!(seen(&other), Some(Err(Seen::Trouble)));
    }

    #[test]
    fn two_overloads_in_a_row_take_the_model_out_until_its_test() {
        let mut model = ModelRecord::default();
        let now = t0();
        assert!(!model.observe(OVERLOAD, &settings(), now));
        assert_eq!(model.state, ModelState::Healthy);

        let again = now + Delta::seconds(7);
        assert!(model.observe(OVERLOAD, &settings(), again));
        assert_eq!(model.state, ModelState::Out);
        assert_eq!(model.trouble, Some(Trouble::Overload));
        assert_eq!(model.reason.as_deref(), Some("2 overload errors in a row"));
        assert_eq!(model.next_check_at, Some(again + Delta::minutes(5)));

        // While it's out, nothing a session says is counted.
        assert!(!model.observe(Ok(()), &settings(), again + Delta::seconds(1)));
        assert!(!model.tick(&settings(), again + Delta::minutes(4)));

        assert!(model.tick(&settings(), again + Delta::minutes(5)));
        assert_eq!(model.state, ModelState::Testing);
        assert_eq!(model.trouble, Some(Trouble::Overload));
    }

    #[test]
    fn a_success_between_two_failures_starts_the_row_over() {
        let mut model = ModelRecord::default();
        let now = t0();
        model.observe(SERVER, &settings(), now);
        model.observe(Ok(()), &settings(), now + Delta::seconds(5));
        assert!(!model.observe(SERVER, &settings(), now + Delta::seconds(10)));
        assert_eq!(model.state, ModelState::Healthy);

        assert!(model.observe(SERVER, &settings(), now + Delta::seconds(15)));
        assert_eq!(model.reason.as_deref(), Some("2 server errors in a row"));
        assert_eq!(model.trouble, Some(Trouble::ServerErrors));
    }

    #[test]
    fn failures_the_window_has_forgotten_are_no_row() {
        let mut model = ModelRecord::default();
        let now = t0();
        model.observe(OVERLOAD, &settings(), now);
        assert!(!model.observe(OVERLOAD, &settings(), now + Delta::minutes(3)));
        assert_eq!(model.state, ModelState::Healthy);
    }

    #[test]
    fn the_test_task_decides() {
        let out = |model: &mut ModelRecord, now| {
            model.observe(OVERLOAD, &settings(), now);
            model.observe(OVERLOAD, &settings(), now);
            model.tick(&settings(), now + Delta::minutes(5));
            assert_eq!(model.state, ModelState::Testing);
        };
        let now = t0();

        let mut passed = ModelRecord::default();
        out(&mut passed, now);
        // A retry notice from the test session says neither.
        assert!(!passed.observe(Err(Seen::Trouble), &settings(), now + Delta::minutes(6)));
        assert!(passed.observe(Ok(()), &settings(), now + Delta::minutes(6)));
        assert_eq!(passed.state, ModelState::Healthy);
        assert_eq!(passed.trouble, None);
        assert!(passed.is_clean());

        let mut failed = ModelRecord::default();
        out(&mut failed, now);
        let at = now + Delta::minutes(6);
        assert!(failed.observe(SERVER, &settings(), at));
        assert_eq!(failed.state, ModelState::Out);
        assert_eq!(failed.reason.as_deref(), Some("the test task failed"));
        assert_eq!(failed.next_check_at, Some(at + Delta::minutes(5)));

        let mut silent = ModelRecord::default();
        out(&mut silent, now);
        assert!(!silent.tick(&settings(), now + Delta::minutes(64)));
        assert!(silent.tick(&settings(), now + Delta::minutes(65)));
        assert_eq!(silent.state, ModelState::Out);
        assert_eq!(
            silent.reason.as_deref(),
            Some("no test task finished within 60 minutes")
        );
    }

    #[test]
    fn reconnects_only_mark_it_degraded_until_it_is_quiet() {
        let mut model = ModelRecord::default();
        let now = t0();
        assert!(!model.observe(Err(Seen::Reconnected), &settings(), now));
        let second = now + Delta::seconds(30);
        assert!(model.observe(Err(Seen::Reconnected), &settings(), second));
        assert_eq!(model.state, ModelState::Degraded);
        assert_eq!(
            model.reason.as_deref(),
            Some("reconnected twice in 2 minutes")
        );
        assert_eq!(model.next_check_at, Some(second + Delta::minutes(2)));

        // Any number of them never takes it out.
        for n in 0..6 {
            model.observe(
                Err(Seen::Reconnected),
                &settings(),
                second + Delta::seconds(n),
            );
        }
        assert_eq!(model.state, ModelState::Degraded);

        assert!(!model.tick(&settings(), second + Delta::minutes(1)));
        assert!(model.tick(&settings(), second + Delta::minutes(3)));
        assert_eq!(model.state, ModelState::Healthy);
        assert_eq!(model.next_check_at, None);
    }

    #[test]
    fn mixed_trouble_is_counted_in_plain_words() {
        let mut model = ModelRecord::default();
        let now = t0();
        model.observe(Err(Seen::Trouble), &settings(), now);
        model.observe(Err(Seen::Reconnected), &settings(), now);
        assert_eq!(
            model.reason.as_deref(),
            Some("2 errors or retries in 2 minutes")
        );
    }

    #[test]
    fn a_signal_is_read_for_what_it_says_of_the_model() {
        let agent_id = AgentId("codex".into());
        let session = argonauts_core::base::names::SessionKey::numbered(1);
        let error = |category| HealthSignal::Error {
            agent_id: agent_id.clone(),
            model: None,
            session: session.clone(),
            category,
        };
        assert_eq!(
            seen(&error(ErrorCategory::Overloaded)),
            Some(Err(Seen::Outage(Trouble::Overload)))
        );
        assert_eq!(
            seen(&error(ErrorCategory::Server)),
            Some(Err(Seen::Outage(Trouble::ServerErrors)))
        );
        assert_eq!(
            seen(&error(ErrorCategory::Network)),
            Some(Err(Seen::Reconnected))
        );
        assert_eq!(
            seen(&error(ErrorCategory::RateLimit)),
            Some(Err(Seen::Trouble))
        );
        // The account's problems say nothing of the model.
        assert_eq!(seen(&error(ErrorCategory::Auth)), None);
        assert_eq!(seen(&error(ErrorCategory::Billing)), None);
        assert_eq!(seen(&error(ErrorCategory::UsageLimit)), None);
    }
}
