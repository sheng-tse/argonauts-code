//! The health board: a circuit breaker per vendor, in `vendors.json` beside the quota. Errors or
//! retries within `health.window` make a vendor Degraded; `down_after` overloads or server errors
//! across `down_sessions` sessions make it Down; after the cool-down (or its reported reset) it
//! goes to Testing. There the first session to report takes the one test card, and its success
//! or outage decides, within [`TEST_CARD_LIMIT`]. A broken adapter disables the vendor, and a
//! billing cap disables its API-key models. Every change is broadcast.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::Paths;
use argonauts_core::clock::{Clock, SystemClock};
use argonauts_core::events::VendorEvent;
use argonauts_core::limits::{HealthBoard, HealthSignal, HealthState, QuotaStatus, VendorHealth};
use argonauts_core::names::{SessionKey, VendorId};
use argonauts_core::settings::Span;
use argonauts_core::settings::sessions::HealthSettings;
use argonauts_core::{ErrorCategory, RetryReason};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::broadcast;

use crate::quota::registered_prober;
use crate::vendors_file::{Owner, VendorEntry, VendorsFile, VendorsStore};

/// Marks kept when `health.window` is `none`, so the file stays small.
const MAX_MARKS: usize = 256;

/// How long Testing waits for the test card's result before it counts as failed, so a test card
/// that stopped without one can't hold the vendor in Testing for good.
pub const TEST_CARD_LIMIT: Duration = Duration::from_secs(60 * 60);

/// Starts every billing disable. `VendorHealth` carries a disable as text alone, so this is how
/// readers tell its scope.
const BILLING_CAP: &str = "the billing cap was reached";

/// Which of a vendor's models a disable keeps from new cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisableScope {
    Every,
    /// Only those billed to an API key, after a billing cap.
    ApiKey,
}

pub fn disable_scope(reason: &str) -> DisableScope {
    if reason.starts_with(BILLING_CAP) {
        DisableScope::ApiKey
    } else {
        DisableScope::Every
    }
}

/// The disable a billing cap sets. It lasts until a session on the vendor succeeds, such as the
/// retry after you top up, or until you enable the vendor.
pub fn billing_disable() -> String {
    format!("{BILLING_CAP}. Tasks billed to its API key go to other agents until a retry works")
}

/// One error or retry the breaker counts, persisted so every process counts the same ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthMark {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The session, prefixed by its process, since two runs each have an `s-001`.
    pub session: String,
    /// Overload or a server error, which counts toward Down as well as Degraded.
    #[serde(default)]
    pub outage: bool,
}

/// One vendor's breaker as plain state, so its rules are tested without files or clocks.
#[derive(Debug, Clone)]
pub struct Breaker {
    settings: HealthSettings,
    state: HealthState,
    since: OffsetDateTime,
    reason: Option<String>,
    next_test_at: Option<OffsetDateTime>,
    reported_reset: Option<OffsetDateTime>,
    left_down_at: Option<OffsetDateTime>,
    marks: Vec<HealthMark>,
    /// A test card is running, while Testing.
    testing_card: bool,
}

impl Breaker {
    pub fn new(settings: &HealthSettings) -> Self {
        Self {
            settings: settings.clone(),
            state: HealthState::Healthy,
            since: OffsetDateTime::UNIX_EPOCH,
            reason: None,
            next_test_at: None,
            reported_reset: None,
            left_down_at: None,
            marks: Vec::new(),
            testing_card: false,
        }
    }

    /// A breaker as `vendors.json` last recorded it.
    pub fn restore(settings: &HealthSettings, entry: &VendorEntry) -> Self {
        let mut breaker = Self::new(settings);
        if let Some(health) = &entry.health {
            breaker.state = health.state;
            breaker.since = health.since;
            breaker.reason = health.reason.clone();
            breaker.next_test_at = health.next_test_at;
        }
        breaker.left_down_at = entry.left_down_at;
        breaker.marks = entry.marks.clone();
        breaker.testing_card = breaker.state == HealthState::Testing && entry.test_card.is_some();
        breaker
    }

    pub fn state(&self) -> HealthState {
        self.state
    }

    pub fn since(&self) -> OffsetDateTime {
        self.since
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn next_test_at(&self) -> Option<OffsetDateTime> {
        self.next_test_at
    }

    pub fn left_down_at(&self) -> Option<OffsetDateTime> {
        self.left_down_at
    }

    pub fn marks(&self) -> &[HealthMark] {
        &self.marks
    }

    pub fn testing_card(&self) -> bool {
        self.testing_card
    }

    pub fn health(&self, vendor: &VendorId, disabled: Option<String>) -> VendorHealth {
        VendorHealth {
            vendor: vendor.clone(),
            state: self.state,
            since: self.since,
            reason: self.reason.clone(),
            next_test_at: self.next_test_at,
            disabled,
        }
    }

    /// Writes the breaker into its `vendors.json` entry, keeping the entry's disable.
    pub fn save(&self, vendor: &VendorId, entry: &mut VendorEntry) {
        let disabled = entry.health.as_ref().and_then(|h| h.disabled.clone());
        entry.health = Some(self.health(vendor, disabled));
        entry.left_down_at = self.left_down_at;
        entry.marks = self.marks.clone();
    }

    /// The reset the vendor reported, such as when its quota comes back. While it's down, the
    /// test waits for this instead of the cool-down.
    pub fn set_reported_reset(&mut self, reset: Option<OffsetDateTime>) {
        self.reported_reset = reset;
        if self.state == HealthState::Down {
            self.next_test_at = self.test_time(self.since);
        }
    }

    /// The new state, if the signal changed it.
    pub fn observe(&mut self, signal: &HealthSignal, now: OffsetDateTime) -> Option<HealthState> {
        self.observe_from(signal, "", now)
    }

    /// Like [`Breaker::observe`], with sessions counted as `<scope>/<session>`, so sessions
    /// of different processes stay apart.
    pub fn observe_from(
        &mut self,
        signal: &HealthSignal,
        scope: &str,
        now: OffsetDateTime,
    ) -> Option<HealthState> {
        let (session, outage) = match signal {
            HealthSignal::Retry {
                session, reason, ..
            } => (
                session,
                matches!(reason, RetryReason::Overloaded | RetryReason::ServerError),
            ),
            HealthSignal::Error {
                session, category, ..
            } if is_vendor_trouble(*category) => (
                session,
                matches!(category, ErrorCategory::Overloaded | ErrorCategory::Server),
            ),
            HealthSignal::Success { .. } => {
                return if self.state == HealthState::Degraded {
                    self.recover_if_quiet(now)
                } else {
                    None
                };
            }
            HealthSignal::Error { .. } | HealthSignal::AdapterBroken { .. } => return None,
        };
        // Down and Testing wait for the test card, so what their paused cards say is not counted.
        if matches!(self.state, HealthState::Down | HealthState::Testing) {
            return None;
        }
        self.marks.push(HealthMark {
            at: now,
            session: if scope.is_empty() {
                session.to_string()
            } else {
                format!("{scope}/{session}")
            },
            outage,
        });
        self.prune(now);

        let outages: Vec<&HealthMark> = self.marks.iter().filter(|m| m.outage).collect();
        let sessions: BTreeSet<&str> = outages.iter().map(|m| m.session.as_str()).collect();
        if outages.len() >= at_least_one(self.settings.down_after)
            && sessions.len() >= at_least_one(self.settings.down_sessions)
        {
            let reason = format!(
                "overload or server errors on {} sessions in {}",
                sessions.len(),
                self.settings.window.0
            );
            return self.go_down(now, reason);
        }
        if self.state == HealthState::Healthy
            && self.marks.len() >= at_least_one(self.settings.degraded_after)
        {
            let reason = format!(
                "{} errors or retries in {}",
                self.marks.len(),
                self.settings.window.0
            );
            return self.set(HealthState::Degraded, now, Some(reason));
        }
        None
    }

    /// Moves Down to Testing once the cool-down or the reported reset has passed, and Testing
    /// back to Down when no test card finished within [`TEST_CARD_LIMIT`].
    pub fn tick(&mut self, now: OffsetDateTime) -> Option<HealthState> {
        match self.state {
            HealthState::Down => {
                let due = self.next_test_at.is_none_or(|at| at <= now);
                if !due {
                    return None;
                }
                let reason = match self.reported_reset {
                    Some(reset) if reset > self.since && reset <= now => {
                        "the reported reset has passed"
                    }
                    _ => "the cool-down is over",
                };
                self.left_down_at = Some(now);
                self.next_test_at = None;
                self.set(HealthState::Testing, now, Some(reason.to_owned()))
            }
            HealthState::Testing if self.since + TEST_CARD_LIMIT <= now => {
                let minutes = TEST_CARD_LIMIT.as_secs() / 60;
                self.go_down(
                    now,
                    format!("no test task finished within {minutes} minutes"),
                )
            }
            HealthState::Degraded => self.recover_if_quiet(now),
            HealthState::Healthy | HealthState::Testing => None,
        }
    }

    /// Starts the one test card a vendor in Testing takes, and its time limit with it. `false`
    /// when it isn't testing or a test card already runs.
    pub fn start_test(&mut self, now: OffsetDateTime) -> bool {
        if self.state != HealthState::Testing || self.testing_card {
            return false;
        }
        self.testing_card = true;
        self.since = now;
        self.reason = Some("a test task is running".to_owned());
        true
    }

    /// The test card's result: Healthy, or Down again.
    pub fn test_result(&mut self, ok: bool, now: OffsetDateTime) -> HealthState {
        if self.state != HealthState::Testing {
            return self.state;
        }
        if ok {
            self.marks.clear();
            self.set(
                HealthState::Healthy,
                now,
                Some("the test task succeeded".to_owned()),
            );
        } else {
            self.go_down(now, "the test task failed".to_owned());
        }
        self.state
    }

    fn go_down(&mut self, now: OffsetDateTime, reason: String) -> Option<HealthState> {
        self.marks.clear();
        self.next_test_at = self.test_time(now);
        self.set(HealthState::Down, now, Some(reason))
    }

    /// When a vendor that went down at `down_at` gets its test card: the reported reset if it
    /// came after, else the cool-down. A cool-down of `none` tests at once.
    fn test_time(&self, down_at: OffsetDateTime) -> Option<OffsetDateTime> {
        match self.reported_reset {
            Some(reset) if reset > down_at => Some(reset),
            _ => Some(down_at + cooldown(&self.settings)),
        }
    }

    fn recover_if_quiet(&mut self, now: OffsetDateTime) -> Option<HealthState> {
        self.prune(now);
        if self.marks.len() < at_least_one(self.settings.degraded_after) {
            self.set(
                HealthState::Healthy,
                now,
                Some("the errors stopped".to_owned()),
            )
        } else {
            None
        }
    }

    /// Forgets marks older than the window. A window of `none` keeps the latest few.
    fn prune(&mut self, now: OffsetDateTime) {
        if let Some(window) = span(&self.settings.window, "health.window") {
            self.marks.retain(|mark| mark.at > now - window);
        }
        let extra = self.marks.len().saturating_sub(MAX_MARKS);
        self.marks.drain(..extra);
    }

    fn set(
        &mut self,
        state: HealthState,
        now: OffsetDateTime,
        reason: Option<String>,
    ) -> Option<HealthState> {
        let changed = self.state != state;
        self.state = state;
        self.since = now;
        self.reason = reason;
        if state != HealthState::Down {
            self.next_test_at = None;
        }
        if state != HealthState::Testing {
            self.testing_card = false;
        }
        changed.then_some(state)
    }
}

/// Errors that say something about the vendor's service. Account problems (login, usage limit,
/// billing) go to the handoff rules, and request or local problems say nothing about health.
pub(crate) fn is_vendor_trouble(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::Overloaded
            | ErrorCategory::Server
            | ErrorCategory::Network
            | ErrorCategory::Timeout
            | ErrorCategory::RateLimit
            | ErrorCategory::ModelUnavailable
            | ErrorCategory::Unknown
    )
}

/// What a signal says about the vendor under test: a success passes it and an overload or
/// server error fails it. `None` when it says neither.
fn test_verdict(signal: &HealthSignal) -> Option<bool> {
    match signal {
        HealthSignal::Success { .. } => Some(true),
        HealthSignal::Error {
            category: ErrorCategory::Overloaded | ErrorCategory::Server,
            ..
        } => Some(false),
        HealthSignal::Retry { .. }
        | HealthSignal::Error { .. }
        | HealthSignal::AdapterBroken { .. } => None,
    }
}

/// The session a signal about the vendor's service came from. Account and request errors come
/// from sessions that won't finish, so they don't take the test card.
fn reporting_session(signal: &HealthSignal) -> Option<&SessionKey> {
    match signal {
        HealthSignal::Retry { session, .. } | HealthSignal::Success { session, .. } => {
            Some(session)
        }
        HealthSignal::Error {
            session, category, ..
        } if is_vendor_trouble(*category) => Some(session),
        HealthSignal::Error { .. } | HealthSignal::AdapterBroken { .. } => None,
    }
}

/// How long a down vendor rests before its test card. `none` rests not at all.
pub(crate) fn cooldown(settings: &HealthSettings) -> Duration {
    span(&settings.cooldown, "health.cooldown").unwrap_or_default()
}

/// A threshold of 0 would trip on nothing at all, so it reads as 1.
fn at_least_one(n: u32) -> usize {
    n.max(1) as usize
}

/// `None` for `none`. A span settings validation missed falls back to its default.
fn span(value: &Span, key: &str) -> Option<Duration> {
    match value.parse() {
        Ok(duration) => duration,
        Err(err) => {
            let defaults = HealthSettings::default();
            let fallback = if key.ends_with("window") {
                defaults.window
            } else {
                defaults.cooldown
            };
            tracing::warn!("{key}: {err}; using {}", fallback.0);
            fallback.parse().ok().flatten()
        }
    }
}

pub struct FileHealthBoard {
    store: VendorsStore,
    settings: HealthSettings,
    events: broadcast::Sender<VendorEvent>,
    clock: Arc<dyn Clock>,
    /// What this board last told subscribers, so changes made by other processes are
    /// broadcast too once it reads them.
    seen: Mutex<BTreeMap<String, VendorHealth>>,
    /// Prefixes session keys with this process, like `<pid>.<start time>`.
    scope: String,
}

impl FileHealthBoard {
    pub fn new(paths: &Paths, settings: &HealthSettings) -> Self {
        let (events, _) = broadcast::channel(64);
        let store = VendorsStore::new(paths);
        let seen = store
            .read()
            .map(|file| {
                file.vendors
                    .iter()
                    .filter_map(|(id, entry)| Some((id.clone(), entry.health.clone()?)))
                    .collect()
            })
            .unwrap_or_default();
        let scope = crate::liveness::this_process()
            .map(|(pid, start)| format!("{pid}.{start}"))
            .unwrap_or_else(|| std::process::id().to_string());
        Self {
            store,
            settings: settings.clone(),
            events,
            clock: Arc::new(SystemClock),
            seen: Mutex::new(seen),
            scope,
        }
    }

    /// Uses `clock` instead of the system time, as tests do.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn store(&self) -> &VendorsStore {
        &self.store
    }

    /// Moves every vendor whose time has come (a finished cool-down, errors that stopped) and
    /// broadcasts what changed, here or in another process. The app calls this on a timer.
    pub fn refresh(&self) {
        let file = match self.store.read() {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("the health board could not read vendors.json: {err}");
                return;
            }
        };
        let now = self.clock.now();
        let due: Vec<VendorId> = file
            .vendors
            .iter()
            .filter(|(_, entry)| self.is_due(entry, now))
            .map(|(id, _)| VendorId(id.clone()))
            .collect();
        if due.is_empty() {
            self.publish(&file);
        }
        for vendor in due {
            self.change(&vendor, |_, _, _| ());
        }
    }

    /// Claims the one test card a vendor in Testing takes. `false` when it isn't testing or
    /// another live session has the card.
    pub fn claim_test_card(&self, vendor: &VendorId, session: &str) -> bool {
        let owner = match Owner::this_process(Some(session.to_owned())) {
            Ok(owner) => owner,
            Err(err) => {
                tracing::warn!("could not claim {vendor}'s test task: {err}");
                return false;
            }
        };
        self.change(vendor, |breaker, entry, now| {
            let claimed = breaker.start_test(now);
            if claimed {
                entry.test_card = Some(owner);
            }
            claimed
        })
        .unwrap_or(false)
    }

    /// Lifts a disable, such as after the CLI was fixed.
    pub fn enable(&self, vendor: &VendorId) {
        self.change(vendor, |_, entry, _| {
            entry.disabled_by = None;
            if let Some(health) = &mut entry.health {
                health.disabled = None;
            }
        });
    }

    fn disable_adapter(&self, vendor: &VendorId, detail: &str) {
        let reason = format!(
            "{}. {}",
            detail.trim_end_matches('.'),
            pin_suggestion(vendor)
        );
        self.disable(vendor, reason);
    }

    /// Records a disable owned by this process, so it lapses with it. A billing cap never
    /// narrows a disable that covers every model.
    fn disable(&self, vendor: &VendorId, reason: String) {
        let owner = Owner::this_process(None);
        self.change(vendor, |breaker, entry, _| {
            let health = entry
                .health
                .get_or_insert_with(|| breaker.health(vendor, None));
            let narrower = disable_scope(&reason) == DisableScope::ApiKey
                && health
                    .disabled
                    .as_deref()
                    .is_some_and(|current| disable_scope(current) == DisableScope::Every);
            if narrower {
                return;
            }
            health.disabled = Some(reason);
            entry.disabled_by = owner.ok();
        });
    }

    /// While Testing, the first session to report takes the test card, and only what that
    /// session says decides the test.
    fn observe_test(
        breaker: &mut Breaker,
        entry: &mut VendorEntry,
        signal: &HealthSignal,
        now: OffsetDateTime,
    ) {
        let Some(session) = reporting_session(signal) else {
            return;
        };
        let owner = Owner::this_process(Some(session.to_string()))
            .map_err(|err| tracing::warn!("could not claim a test task: {err}"))
            .ok();
        let holds = match (&entry.test_card, &owner) {
            (Some(card), Some(owner)) => card == owner,
            (Some(_), None) => false,
            (None, _) => {
                breaker.start_test(now);
                entry.test_card = owner;
                true
            }
        };
        if holds && let Some(ok) = test_verdict(signal) {
            breaker.test_result(ok, now);
        }
    }

    fn is_due(&self, entry: &VendorEntry, now: OffsetDateTime) -> bool {
        let mut breaker = self.breaker(entry);
        breaker.tick(now).is_some()
    }

    fn breaker(&self, entry: &VendorEntry) -> Breaker {
        let mut breaker = Breaker::restore(&self.settings, entry);
        let reset = entry
            .quota
            .as_ref()
            .filter(|quota| quota.status == QuotaStatus::Rejected)
            .and_then(|quota| quota.resets_at);
        breaker.set_reported_reset(reset);
        breaker
    }

    /// Changes one vendor's entry under the file lock, after moving it along if its time came,
    /// then broadcasts what changed.
    fn change<T>(
        &self,
        vendor: &VendorId,
        apply: impl FnOnce(&mut Breaker, &mut VendorEntry, OffsetDateTime) -> T,
    ) -> Option<T> {
        let now = self.clock.now();
        let updated = self.store.update(|file| {
            let entry = file.vendors.entry(vendor.0.clone()).or_default();
            let mut breaker = self.breaker(entry);
            breaker.tick(now);
            let result = apply(&mut breaker, entry, now);
            breaker.save(vendor, entry);
            if !breaker.testing_card() {
                entry.test_card = None;
            }
            (result, file.clone())
        });
        match updated {
            Ok((result, file)) => {
                self.publish(&file);
                Some(result)
            }
            Err(err) => {
                tracing::warn!("the health board could not write vendors.json: {err}");
                None
            }
        }
    }

    fn publish(&self, file: &VendorsFile) {
        let Ok(mut seen) = self.seen.lock() else {
            tracing::warn!("the health board's memory is poisoned");
            return;
        };
        for (id, entry) in &file.vendors {
            let vendor = VendorId(id.clone());
            let current = health_of(&vendor, Some(entry));
            let before = seen
                .get(id)
                .cloned()
                .unwrap_or_else(|| health_of(&vendor, None));
            if current == before {
                continue;
            }
            if let Some(reason) = &current.disabled
                && before.disabled.as_ref() != Some(reason)
            {
                let _ = self.events.send(VendorEvent::Disabled {
                    vendor: vendor.clone(),
                    reason: reason.clone(),
                });
            }
            let moved = (
                current.state,
                current.since,
                &current.reason,
                current.next_test_at,
            ) != (
                before.state,
                before.since,
                &before.reason,
                before.next_test_at,
            );
            if moved || (current.disabled.is_none() && before.disabled.is_some()) {
                let _ = self.events.send(VendorEvent::Health(current.clone()));
            }
            seen.insert(id.clone(), current);
        }
    }
}

/// A session on the vendor worked, so its billing is fine again.
fn lift_billing_disable(entry: &mut VendorEntry) {
    if let Some(health) = &mut entry.health
        && health
            .disabled
            .as_deref()
            .is_some_and(|reason| disable_scope(reason) == DisableScope::ApiKey)
    {
        health.disabled = None;
        entry.disabled_by = None;
    }
}

fn health_of(vendor: &VendorId, entry: Option<&VendorEntry>) -> VendorHealth {
    entry
        .and_then(|entry| entry.health.clone())
        .unwrap_or_else(|| Breaker::new(&HealthSettings::default()).health(vendor, None))
}

/// How to get back to a CLI version the adapter was tested with.
fn pin_suggestion(vendor: &VendorId) -> String {
    let tested = registered_prober(vendor)
        .and_then(|prober| prober.adapter.tested_versions().last().copied());
    match tested {
        Some(version) => format!(
            "New tasks go to other agents. To pin a tested version, install {version} and set \
             agents.{vendor}.version_pin = \"{version}\""
        ),
        None => format!(
            "New tasks go to other agents. To pin a tested version, install one and set \
             agents.{vendor}.version_pin"
        ),
    }
}

impl HealthBoard for FileHealthBoard {
    fn observe(&self, signal: HealthSignal) {
        let (HealthSignal::Retry { vendor, .. }
        | HealthSignal::Error { vendor, .. }
        | HealthSignal::Success { vendor, .. }
        | HealthSignal::AdapterBroken { vendor, .. }) = &signal;
        match &signal {
            HealthSignal::AdapterBroken { detail, .. } => {
                self.disable_adapter(vendor, detail);
                return;
            }
            HealthSignal::Error {
                category: ErrorCategory::Billing,
                ..
            } => {
                self.disable(vendor, billing_disable());
                return;
            }
            HealthSignal::Retry { .. }
            | HealthSignal::Error { .. }
            | HealthSignal::Success { .. } => {}
        }
        self.change(vendor, |breaker, entry, now| {
            if matches!(signal, HealthSignal::Success { .. }) {
                lift_billing_disable(entry);
            }
            if breaker.state() == HealthState::Testing {
                Self::observe_test(breaker, entry, &signal, now);
            } else {
                breaker.observe_from(&signal, &self.scope, now);
            }
        });
    }

    /// Fails closed: when `vendors.json` can't be read, the vendor reads as disabled, since
    /// whether you or its adapter disabled it is unknown.
    fn state(&self, vendor: &VendorId) -> VendorHealth {
        let file = match self.store.read() {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("the health board could not read vendors.json: {err}");
                return VendorHealth {
                    disabled: Some(format!(
                        "could not read {}, so whether {vendor} is disabled is unknown: {err}. \
                         Fix or remove the file to start new work on it",
                        self.store.path().display()
                    )),
                    ..health_of(vendor, None)
                };
            }
        };
        match file.entry(vendor) {
            Some(entry) if self.is_due(entry, self.clock.now()) => self
                .change(vendor, |breaker, entry, _| {
                    let disabled = entry.health.as_ref().and_then(|h| h.disabled.clone());
                    breaker.health(vendor, disabled)
                })
                .unwrap_or_else(|| health_of(vendor, Some(entry))),
            entry => {
                self.publish(&file);
                health_of(vendor, entry)
            }
        }
    }

    fn all(&self) -> Vec<VendorHealth> {
        self.refresh();
        match self.store.read() {
            Ok(file) => file
                .vendors
                .iter()
                .map(|(id, entry)| health_of(&VendorId(id.clone()), Some(entry)))
                .collect(),
            Err(err) => {
                tracing::warn!("the health board could not read vendors.json: {err}");
                Vec::new()
            }
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<VendorEvent> {
        self.events.subscribe()
    }

    fn test_result(&self, vendor: &VendorId, ok: bool) {
        self.change(vendor, |breaker, _, now| {
            if breaker.state() == HealthState::Testing {
                breaker.test_result(ok, now);
            } else {
                tracing::debug!("a test result for {vendor} came while it wasn't testing");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::{QuotaReading, QuotaSource};
    use argonauts_core::names::SessionKey;
    use time::Duration as Delta;

    fn settings() -> HealthSettings {
        HealthSettings {
            degraded_after: 2,
            down_after: 3,
            down_sessions: 2,
            window: Span::new("2m"),
            cooldown: Span::new("5m"),
        }
    }

    fn codex() -> VendorId {
        VendorId("codex".into())
    }

    fn retry(session: u32, reason: RetryReason) -> HealthSignal {
        HealthSignal::Retry {
            vendor: codex(),
            session: SessionKey::numbered(session),
            reason,
        }
    }

    fn error(session: u32, category: ErrorCategory) -> HealthSignal {
        HealthSignal::Error {
            vendor: codex(),
            session: SessionKey::numbered(session),
            category,
        }
    }

    fn success(session: u32) -> HealthSignal {
        HealthSignal::Success {
            vendor: codex(),
            session: SessionKey::numbered(session),
        }
    }

    fn t0() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    #[derive(Default)]
    struct ManualClock(Mutex<Option<OffsetDateTime>>);

    impl ManualClock {
        fn at(time: OffsetDateTime) -> Arc<Self> {
            Arc::new(Self(Mutex::new(Some(time))))
        }
        fn advance(&self, by: Delta) {
            let mut now = self.0.lock().unwrap();
            *now = Some(now.unwrap() + by);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> OffsetDateTime {
            self.0.lock().unwrap().unwrap()
        }
    }

    #[test]
    fn circuit_breaker_states() {
        let mut breaker = Breaker::new(&settings());
        let now = t0();
        assert_eq!(breaker.state(), HealthState::Healthy);

        // Account problems say nothing about the service.
        assert_eq!(breaker.observe(&error(1, ErrorCategory::Auth), now), None);
        assert_eq!(
            breaker.observe(&error(1, ErrorCategory::UsageLimit), now),
            None
        );
        assert_eq!(breaker.observe(&retry(1, RetryReason::Network), now), None);
        assert_eq!(
            breaker.observe(&retry(1, RetryReason::RateLimit), now),
            Some(HealthState::Degraded)
        );
        assert_eq!(breaker.reason(), Some("2 errors or retries in 2m"));

        // The errors stop: the window passes and a success brings it back.
        let later = now + Delta::minutes(3);
        assert_eq!(
            breaker.observe(&success(1), later),
            Some(HealthState::Healthy)
        );

        // Overloads on one session don't take it down, however many.
        for _ in 0..4 {
            breaker.observe(&error(1, ErrorCategory::Overloaded), later);
        }
        assert_eq!(breaker.state(), HealthState::Degraded);
        // A second session makes it an outage.
        assert_eq!(
            breaker.observe(&retry(2, RetryReason::ServerError), later),
            Some(HealthState::Down)
        );
        assert_eq!(
            breaker.reason(),
            Some("overload or server errors on 2 sessions in 2m")
        );
        let test_at = later + Delta::minutes(5);
        assert_eq!(breaker.next_test_at(), Some(test_at));
        // Paused cards' errors don't count while it's down.
        assert_eq!(
            breaker.observe(&error(3, ErrorCategory::Server), later),
            None
        );

        assert_eq!(breaker.tick(test_at - Delta::seconds(1)), None);
        assert_eq!(breaker.tick(test_at), Some(HealthState::Testing));
        assert_eq!(breaker.left_down_at(), Some(test_at));
        assert_eq!(breaker.next_test_at(), None);
        assert_eq!(breaker.test_result(true, test_at), HealthState::Healthy);
        assert!(breaker.marks().is_empty());

        // Down again, and this time the test card fails.
        for session in 1..=3 {
            breaker.observe(&error(session, ErrorCategory::Overloaded), test_at);
        }
        assert_eq!(breaker.state(), HealthState::Down);
        let test_at = test_at + Delta::minutes(5);
        assert_eq!(breaker.tick(test_at), Some(HealthState::Testing));
        assert_eq!(breaker.test_result(false, test_at), HealthState::Down);
        assert_eq!(breaker.reason(), Some("the test task failed"));
        assert_eq!(breaker.next_test_at(), Some(test_at + Delta::minutes(5)));

        // A reported reset replaces the cool-down.
        let reset = test_at + Delta::minutes(30);
        breaker.set_reported_reset(Some(reset));
        assert_eq!(breaker.next_test_at(), Some(reset));
        assert_eq!(breaker.tick(test_at + Delta::minutes(10)), None);
        assert_eq!(breaker.tick(reset), Some(HealthState::Testing));
        assert_eq!(breaker.reason(), Some("the reported reset has passed"));
        // A result outside Testing changes nothing.
        assert_eq!(breaker.test_result(true, reset), HealthState::Healthy);
        assert_eq!(breaker.test_result(false, reset), HealthState::Healthy);
    }

    #[test]
    fn odd_settings_still_work() {
        let mut breaker = Breaker::new(&HealthSettings {
            degraded_after: 0,
            down_after: 0,
            down_sessions: 0,
            window: Span::new("none"),
            cooldown: Span::new("soon"),
        });
        let now = t0();
        assert_eq!(
            breaker.observe(&error(1, ErrorCategory::Server), now),
            Some(HealthState::Down)
        );
        // An unreadable cool-down uses the default one.
        assert_eq!(breaker.next_test_at(), Some(now + Delta::minutes(5)));

        let mut breaker = Breaker::new(&HealthSettings {
            cooldown: Span::new("none"),
            window: Span::new("none"),
            ..settings()
        });
        for session in 1..=3 {
            breaker.observe(&error(session, ErrorCategory::Overloaded), now);
        }
        assert_eq!(breaker.tick(now), Some(HealthState::Testing));
    }

    fn board(home: &std::path::Path, clock: &Arc<ManualClock>) -> FileHealthBoard {
        FileHealthBoard::new(&Paths::new(home), &settings()).with_clock(clock.clone())
    }

    fn drain(events: &mut broadcast::Receiver<VendorEvent>) -> Vec<VendorEvent> {
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    fn states(events: &[VendorEvent]) -> Vec<HealthState> {
        events
            .iter()
            .filter_map(|event| match event {
                VendorEvent::Health(health) => Some(health.state),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn health_changes_are_broadcast() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let app = board(home.path(), &clock);
        let supervisor = board(home.path(), &clock);
        let mut events = app.subscribe();

        assert_eq!(app.state(&codex()).state, HealthState::Healthy);
        assert!(app.all().is_empty());
        app.observe(retry(1, RetryReason::Overloaded));
        assert!(drain(&mut events).is_empty());
        app.observe(retry(1, RetryReason::Overloaded));
        assert_eq!(states(&drain(&mut events)), vec![HealthState::Degraded]);

        // Another process's signals count with ours, and its change reaches our subscribers.
        supervisor.observe(error(2, ErrorCategory::Server));
        assert_eq!(supervisor.state(&codex()).state, HealthState::Down);
        assert!(drain(&mut events).is_empty());
        app.refresh();
        assert_eq!(states(&drain(&mut events)), vec![HealthState::Down]);

        clock.advance(Delta::minutes(5));
        assert_eq!(app.state(&codex()).state, HealthState::Testing);
        assert_eq!(states(&drain(&mut events)), vec![HealthState::Testing]);
        assert!(app.claim_test_card(&codex(), "s-004"));
        assert!(!supervisor.claim_test_card(&codex(), "s-005"));
        assert_eq!(
            app.state(&codex()).reason.as_deref(),
            Some("a test task is running")
        );
        app.test_result(&codex(), true);
        assert_eq!(
            states(&drain(&mut events)),
            vec![HealthState::Testing, HealthState::Healthy]
        );
        assert_eq!(app.store().read().unwrap().vendors["codex"].test_card, None);

        // A broken adapter disables the vendor for new cards, with a pin suggestion.
        supervisor.observe(HealthSignal::AdapterBroken {
            vendor: codex(),
            detail: "unknown events piled up".into(),
        });
        app.refresh();
        let events = drain(&mut events);
        let Some(VendorEvent::Disabled { vendor, reason }) = events.first() else {
            panic!("expected a disable, got {events:?}");
        };
        assert_eq!(vendor, &codex());
        assert!(reason.starts_with("unknown events piled up. New tasks go to other agents."));
        assert!(reason.contains("agents.codex.version_pin"));
        let health = app.state(&codex());
        assert_eq!(health.state, HealthState::Healthy);
        assert!(health.disabled.is_some());

        app.enable(&codex());
        assert_eq!(app.state(&codex()).disabled, None);
        assert_eq!(app.all().len(), 1);
    }

    #[test]
    fn wake_fields_recorded() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        let parked_at = t0();
        for session in 1..=3 {
            board.observe(error(session, ErrorCategory::Overloaded));
        }
        let store = board.store();
        let file = store.read().unwrap();
        let entry = &file.vendors["codex"];
        let health = entry.health.as_ref().unwrap();
        assert_eq!(health.state, HealthState::Down);
        let test_at = t0() + Delta::minutes(5);
        assert_eq!(health.next_test_at, Some(test_at));
        assert_eq!(entry.left_down_at, None);
        assert_eq!(file.earliest_reset(), Some(test_at));
        // With no supervisor running, the app still sees the vendor is due back.
        assert!(!file.vendor_back(&codex(), parked_at, t0()));
        assert!(file.vendor_back(&codex(), parked_at, test_at));

        // A reported quota reset later than the cool-down moves the test.
        let reset = t0() + Delta::hours(1);
        store
            .update(|file| {
                file.vendors.get_mut("codex").unwrap().quota = Some(QuotaReading {
                    vendor: codex(),
                    status: QuotaStatus::Rejected,
                    used: Some(1.0),
                    resets_at: Some(reset),
                    window: Some("five_hour".into()),
                    source: QuotaSource::AppServer,
                    observed_at: t0(),
                    detail: None,
                    wrap_up: false,
                })
            })
            .unwrap();
        clock.advance(Delta::minutes(5));
        assert_eq!(board.state(&codex()).state, HealthState::Down);
        clock.advance(Delta::minutes(55));
        assert_eq!(board.state(&codex()).state, HealthState::Testing);

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        let codex_raw = &raw["vendors"]["codex"];
        assert_eq!(codex_raw["health"]["state"], "testing");
        assert_eq!(codex_raw["left_down_at"], "2027-01-15T09:00:00Z");
        assert_eq!(codex_raw["quota"]["resets_at"], "2027-01-15T09:00:00Z");
        let file = store.read().unwrap();
        assert!(file.vendor_back(&codex(), parked_at, parked_at));
        assert_eq!(file.earliest_reset(), Some(reset));
    }

    #[test]
    fn an_unreadable_board_disables_new_work() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        assert!(board.state(&codex()).disabled.is_none());
        std::fs::write(board.store().path(), "{ not json").unwrap();
        let health = board.state(&codex());
        assert!(health.disabled.unwrap().contains("vendors.json"));
    }

    fn take_down(board: &FileHealthBoard) {
        for session in 1..=3 {
            board.observe(error(session, ErrorCategory::Overloaded));
        }
        assert_eq!(board.state(&codex()).state, HealthState::Down);
    }

    fn test_card(board: &FileHealthBoard) -> Option<String> {
        let file = board.store().read().unwrap();
        file.vendors["codex"].test_card.clone()?.session
    }

    #[test]
    fn one_test_card_decides() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        take_down(&board);
        clock.advance(Delta::minutes(5));
        assert_eq!(board.state(&codex()).state, HealthState::Testing);

        // The first session to report takes the test card, and no one else can.
        board.observe(retry(4, RetryReason::Overloaded));
        assert_eq!(test_card(&board).as_deref(), Some("s-004"));
        assert_eq!(
            board.state(&codex()).reason.as_deref(),
            Some("a test task is running")
        );
        assert!(!board.claim_test_card(&codex(), "s-005"));
        board.observe(error(5, ErrorCategory::Server));
        board.observe(success(5));
        assert_eq!(board.state(&codex()).state, HealthState::Testing);
        board.observe(success(4));
        assert_eq!(board.state(&codex()).state, HealthState::Healthy);
        assert_eq!(test_card(&board), None);

        // Under test again, a new outage takes the card and fails it.
        take_down(&board);
        clock.advance(Delta::minutes(5));
        board.observe(error(6, ErrorCategory::Overloaded));
        let health = board.state(&codex());
        assert_eq!(health.state, HealthState::Down);
        assert_eq!(health.reason.as_deref(), Some("the test task failed"));
        assert_eq!(test_card(&board), None);
    }

    #[test]
    fn testing_without_a_result_falls_back_to_down() {
        let mut breaker = Breaker::new(&settings());
        for session in 1..=3 {
            breaker.observe(&error(session, ErrorCategory::Overloaded), t0());
        }
        let test_at = t0() + Delta::minutes(5);
        assert_eq!(breaker.tick(test_at), Some(HealthState::Testing));
        let claimed_at = test_at + Delta::minutes(20);
        assert!(breaker.start_test(claimed_at));
        assert!(!breaker.start_test(claimed_at), "one test task at a time");
        // The limit runs from the claim.
        let limit = claimed_at + TEST_CARD_LIMIT;
        assert_eq!(breaker.tick(limit - Delta::seconds(1)), None);
        assert_eq!(breaker.tick(limit), Some(HealthState::Down));
        assert_eq!(
            breaker.reason(),
            Some("no test task finished within 60 minutes")
        );
        assert_eq!(breaker.next_test_at(), Some(limit + Delta::minutes(5)));
        assert!(!breaker.testing_card());
        assert!(!breaker.start_test(limit), "only Testing takes a test task");

        // On the board, a test card that never reports lets go of the vendor too.
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        take_down(&board);
        clock.advance(Delta::minutes(5));
        assert!(board.claim_test_card(&codex(), "s-004"));
        clock.advance(Delta::try_from(TEST_CARD_LIMIT).unwrap());
        board.refresh();
        assert_eq!(board.state(&codex()).state, HealthState::Down);
        assert_eq!(test_card(&board), None);
        clock.advance(Delta::minutes(5));
        board.observe(error(7, ErrorCategory::Server));
        assert_eq!(board.state(&codex()).state, HealthState::Down);
    }

    #[test]
    fn billing_cap_disables_api_key_models() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        let mut events = board.subscribe();
        let billing = || error(1, ErrorCategory::Billing);

        board.observe(billing());
        let health = board.state(&codex());
        assert_eq!(health.state, HealthState::Healthy);
        let reason = health.disabled.expect("a billing disable");
        assert_eq!(disable_scope(&reason), DisableScope::ApiKey);
        assert!(matches!(
            drain(&mut events).first(),
            Some(VendorEvent::Disabled { .. })
        ));
        let file = board.store().read().unwrap();
        assert!(
            file.vendors["codex"].disabled_by.is_some(),
            "owned by this process"
        );

        // A session that works lifts it, such as the retry after you top up.
        board.observe(success(1));
        assert_eq!(board.state(&codex()).disabled, None);
        assert_eq!(
            board.store().read().unwrap().vendors["codex"].disabled_by,
            None
        );

        // So does `enable`.
        board.observe(billing());
        board.enable(&codex());
        assert_eq!(board.state(&codex()).disabled, None);

        // A billing cap never narrows a broken adapter's disable, and a success keeps it.
        board.observe(HealthSignal::AdapterBroken {
            vendor: codex(),
            detail: "the startup check failed".into(),
        });
        board.observe(billing());
        board.observe(success(2));
        let reason = board.state(&codex()).disabled.expect("still disabled");
        assert_eq!(disable_scope(&reason), DisableScope::Every);
    }
}
