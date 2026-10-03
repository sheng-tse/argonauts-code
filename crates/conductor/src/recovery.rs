//! The limits service: each session's limits from the settings, and what happens after a session
//! stops for anything but a clean finish, following the recovery table (usage limits, outages,
//! logins, billing, broken updates, retired models, context, stuck agents, budgets, crashes).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use argonauts_core::events::card::{CardEvent, CardSpec, CardState};
use argonauts_core::events::run::ParkReason;
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::vendor::{HandoffEvent, HandoffPlan};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::future::BoxFuture;
use argonauts_core::handoff::CheckpointTag;
use argonauts_core::limits::{
    Candidate, CandidateQuery, CostEstimate, HandoffPacket, HandoffPlanner, HealthBoard,
    HealthState, LimitsService, QuotaBoard, RecoveryAction, Shortlist, StopContext,
};
use argonauts_core::names::{RoleName, VendorId, WorkerRef};
use argonauts_core::registry::{ModelEntry, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::settings::budget::{BudgetAmount, BudgetSettings, BudgetUnit};
use argonauts_core::settings::handoff::{AppliedHandoff, HandoffMode, HandoffStrategy, OnReturn};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::worker::SessionLimits;
use argonauts_core::{AgentEvent, CardId, DEFAULT_SOFT_RATIO, ErrorCategory, RunId};
use time::OffsetDateTime;

use crate::handoff::history::{CardLog, History, Placements, card_log};
use crate::handoff::shortlist::{auto_check, balance, out_of_quota};
use crate::handoff::why::{one_line, stop_line};
use crate::handoff::{applied, entry_rules};
use crate::health::cooldown;

/// The native cap sits this far above the meter's dollar limit, so the meter stops first.
const NATIVE_CAP_MARGIN: f64 = 1.1;

/// Successors followed before a retired model gives up and asks you.
const MAX_SUCCESSOR_HOPS: usize = 5;

const RESUME_MESSAGE: &str =
    "The run was paused and is resuming. Continue the card from where you stopped.";

/// A card `spread` handed to a worker, and the newest log event read when it did.
type Picks = HashMap<CardId, (WorkerRef, u64)>;

pub struct Limits {
    settings: Arc<Settings>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    handoff: Arc<dyn HandoffPlanner>,
    registry: Option<Arc<dyn ModelRegistry>>,
    /// Each run's `spread` picks the log may not show yet, since a vendor's cards stop at once.
    picks: Mutex<HashMap<RunId, Picks>>,
}

impl Limits {
    pub fn new(
        settings: Arc<Settings>,
        quota: Arc<dyn QuotaBoard>,
        health: Arc<dyn HealthBoard>,
        handoff: Arc<dyn HandoffPlanner>,
    ) -> Self {
        Self {
            settings,
            quota,
            health,
            handoff,
            registry: None,
            picks: Mutex::default(),
        }
    }

    /// Lets a retired model hand off to its successor and the retry ladder tell families apart.
    /// Without it, a retired model asks you.
    pub fn with_registry(mut self, registry: Arc<dyn ModelRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// How long a handoff checkpoint for this role and vendor waits for you before it falls
    /// back to auto. `None` waits for you.
    pub fn ask_timeout(
        &self,
        role: RoleName,
        vendor: &VendorId,
    ) -> Result<Option<Duration>, String> {
        applied(&self.settings, role, vendor).ask_timeout.parse()
    }

    /// When a handoff checkpoint's `ask_timeout` passes: the stop decided as in `auto` mode, so
    /// the card goes to the next valid candidate within the cost cap, or `Ask` again when none is
    /// left.
    pub fn after_ask_timeout<'a>(
        &'a self,
        context: &'a StopContext,
    ) -> BoxFuture<'a, RecoveryAction> {
        Box::pin(self.recover(context, true))
    }

    async fn recover(&self, context: &StopContext, timed_out: bool) -> RecoveryAction {
        let (log, unread) = match card_log(&context.events, &context.card.id).await {
            Ok(log) => (log, None),
            Err(error) => {
                tracing::warn!(
                    path = %context.events.display(),
                    %error,
                    "the handoff packet has only the card, since the event log couldn't be read"
                );
                (CardLog::default(), Some(error.to_string()))
            }
        };
        let stop = self.stop(context, log, unread, timed_out);
        self.decide(&stop)
    }

    fn stop<'a>(
        &self,
        context: &'a StopContext,
        log: CardLog,
        unread: Option<String>,
        timed_out: bool,
    ) -> Stop<'a> {
        let events = &log.events;
        let model = self.stopped_model(events, &context.worker);
        let mut packet = self.handoff.packet(&context.card, events);
        packet.why_stopped = match &context.end.reason {
            EndReason::ModelUnavailable { model: named } if named.is_empty() => {
                stop_line(&EndReason::ModelUnavailable {
                    model: model
                        .clone()
                        .unwrap_or_else(|| format!("{}'s default", context.worker)),
                })
            }
            reason => stop_line(reason),
        };
        if packet.work.branch.is_empty() {
            packet.work.branch = format!("argonauts/{}/{}", context.run, context.card.id);
        }
        if let Some(error) = unread {
            packet.notes.push(format!(
                "The run's event log couldn't be read ({}), so this packet has only the card.",
                one_line(&error, 160)
            ));
        }
        let history = History::of(events, &context.card.id);
        Stop {
            context,
            rules: applied(&self.settings, context.role, &context.worker.vendor()),
            handed_off: history.handoffs,
            gates_ran: history.gates_ran,
            family: self.family(events, &context.worker),
            model,
            packet,
            timed_out,
            placements: log.placements,
        }
    }

    /// The family of the worker that stopped: the registry's, else what its session recorded,
    /// else the worker's own name.
    fn family(&self, events: &[RunEvent], worker: &WorkerRef) -> String {
        if let Some(registry) = &self.registry {
            return registry.family(worker);
        }
        events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                RunEventKind::Session(SessionEvent::Started(started))
                    if &started.worker == worker =>
                {
                    Some(started.family.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| worker.worker.clone())
    }

    /// The model the stopped worker ran: its own, else `agents.<id>.model`, else the one its
    /// session's CLI reported. The vendor's error text is prose, so it never names the model.
    fn stopped_model(&self, events: &[RunEvent], worker: &WorkerRef) -> Option<String> {
        worker
            .model
            .clone()
            .or_else(|| {
                let agent = self.settings.agents.get(&worker.worker);
                agent.and_then(|agent| agent.model.clone())
            })
            .or_else(|| reported_model(events, worker))
    }

    fn decide(&self, stop: &Stop<'_>) -> RecoveryAction {
        match &stop.context.end.reason {
            EndReason::Completed => RecoveryAction::Proceed,
            EndReason::Paused => RecoveryAction::ResumeSame {
                message: RESUME_MESSAGE.to_owned(),
            },
            EndReason::Stopped => RecoveryAction::Escalate {
                reason: "you stopped its session".to_owned(),
            },
            EndReason::UsageLimit { resets_at } => self.vendor_out(stop, Out::Quota(*resets_at)),
            EndReason::RateLimited => self.vendor_out(stop, Out::Quota(None)),
            EndReason::VendorError { category, .. } => self.vendor_error(stop, *category),
            EndReason::Auth { message } => self.logged_out(stop, message),
            EndReason::Billing { message } => self.billing(stop, message),
            EndReason::ContextFull => RecoveryAction::Rotate {
                packet: Box::new(stop.packet.clone()),
            },
            EndReason::ModelUnavailable { .. } => self.successor(stop),
            EndReason::AdapterBroken { .. } => self.adapter_broken(stop),
            EndReason::Stuck { .. } | EndReason::StartFailed { .. } | EndReason::Crashed { .. } => {
                self.ladder(stop)
            }
            EndReason::BudgetHit { .. } => RecoveryAction::BudgetHit(self.settings.budget.on_hit),
        }
    }

    fn vendor_error(&self, stop: &Stop<'_>, category: ErrorCategory) -> RecoveryAction {
        match category {
            ErrorCategory::Overloaded
            | ErrorCategory::Server
            | ErrorCategory::Network
            | ErrorCategory::Timeout => self.vendor_out(stop, Out::Outage),
            ErrorCategory::UsageLimit | ErrorCategory::RateLimit => {
                self.vendor_out(stop, Out::Quota(None))
            }
            ErrorCategory::Auth => self.logged_out(stop, ""),
            ErrorCategory::Billing => self.billing(stop, ""),
            ErrorCategory::ContextFull => RecoveryAction::Rotate {
                packet: Box::new(stop.packet.clone()),
            },
            ErrorCategory::ModelUnavailable => self.successor(stop),
            ErrorCategory::Budget => RecoveryAction::BudgetHit(self.settings.budget.on_hit),
            ErrorCategory::InvalidRequest
            | ErrorCategory::Permission
            | ErrorCategory::Interrupted
            | ErrorCategory::Cli
            | ErrorCategory::Stalled
            | ErrorCategory::Unknown => self.ladder(stop),
        }
    }

    /// A usage limit or an outage: the vendor is out for a while, so the handoff mode decides.
    fn vendor_out(&self, stop: &Stop<'_>, out: Out) -> RecoveryAction {
        let vendor = stop.vendor();
        let until = match out {
            Out::Quota(resets_at) => resets_at.or_else(|| {
                self.quota
                    .get(&vendor)
                    .and_then(|reading| reading.resets_at)
            }),
            Out::Outage => self.health.state(&vendor).next_test_at,
        };
        let mode = stop.mode();
        if mode != HandoffMode::Wait && stop.handed_off >= stop.rules.max_handoffs {
            return too_many_handoffs(stop);
        }
        let reason = stop.reason();
        match mode {
            HandoffMode::Ask => self.ask(stop, reason, until, self.others(stop)),
            // With no known return, as for an error before the board marks the vendor Down or a
            // limit with no reset, the wait is a retry after the breaker's cool-down, so the
            // parked card always has something to wake it.
            HandoffMode::Wait => RecoveryAction::Wait {
                vendor,
                until: until.or_else(|| Some(self.retry_at())),
            },
            HandoffMode::Auto => {
                let shortlist = self.others(stop);
                let none_to_take_it = !shortlist
                    .candidates
                    .iter()
                    .any(|candidate| auto_check(candidate).is_ok());
                // Every agent of the crew is out, so the run parks until the first one is back
                // and the card goes on then.
                if none_to_take_it && let Some(out) = self.all_out(&self.crew(stop)) {
                    return RecoveryAction::Park(out);
                }
                self.hand_off(stop, reason, until, shortlist)
            }
        }
    }

    /// The crew the run draws on: your `crew`, the default agent, and the worker that stopped.
    fn crew(&self, stop: &Stop<'_>) -> Vec<WorkerRef> {
        let mut workers = self.settings.crew.clone();
        for worker in [&self.settings.default_agent, &stop.context.worker] {
            if !workers.contains(worker) {
                workers.push(worker.clone());
            }
        }
        workers
    }

    fn retry_at(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc() + cooldown(&self.settings.health)
    }

    /// Always asks, with the retry the engine lists first. Candidates are still offered.
    fn logged_out(&self, stop: &Stop<'_>, message: &str) -> RecoveryAction {
        let vendor = stop.vendor();
        let reason = with_detail(
            format!("logged out of {vendor}: log in to {vendor}, then retry"),
            message,
        );
        self.ask_until_timeout(stop, reason, self.others(stop))
    }

    /// Always asks, and never moves on to another paid API by itself, even after the timeout.
    fn billing(&self, stop: &Stop<'_>, message: &str) -> RecoveryAction {
        let vendor = stop.vendor();
        let reason = with_detail(
            format!("{vendor} hit its billing cap: top up, then retry"),
            message,
        );
        let mut shortlist = self.others(stop);
        if stop.timed_out {
            shortlist
                .candidates
                .retain(|candidate| !matches!(candidate.cost, CostEstimate::Usd(_)));
        }
        self.ask_until_timeout(stop, reason, shortlist)
    }

    /// The registry's successor in the same family, on the same worker; else you decide.
    fn successor(&self, stop: &Stop<'_>) -> RecoveryAction {
        let next = self.registry.as_deref().and_then(|registry| {
            successor_of(registry, &stop.context.worker, stop.model.as_deref())
        });
        match next {
            Some(to) => RecoveryAction::HandOff {
                to,
                packet: Box::new(stop.packet.clone()),
                checkpoint: stop.checkpoint(),
            },
            None => {
                let reason = format!("{}, with no successor in its family", stop.reason());
                self.ask_until_timeout(stop, reason, self.shortlist(stop, false, Vec::new()))
            }
        }
    }

    /// The vendor stays disabled until you pin a working version, so waiting doesn't help.
    fn adapter_broken(&self, stop: &Stop<'_>) -> RecoveryAction {
        if stop.handed_off >= stop.rules.max_handoffs {
            return too_many_handoffs(stop);
        }
        let reason = stop.reason();
        match stop.mode() {
            HandoffMode::Auto => self.hand_off(stop, reason, None, self.others(stop)),
            HandoffMode::Ask | HandoffMode::Wait => self.ask(stop, reason, None, self.others(stop)),
        }
    }

    /// Stuck, crashed, or failed to start: one fresh session on another family, then you.
    fn ladder(&self, stop: &Stop<'_>) -> RecoveryAction {
        let why = stop.reason();
        if stop.context.reassigned_once {
            return RecoveryAction::Escalate {
                reason: format!("{why}, and it was already retried on another agent"),
            };
        }
        let shortlist = self.shortlist(stop, true, vec![stop.family.clone()]);
        match shortlist
            .candidates
            .iter()
            .find(|candidate| auto_check(candidate).is_ok())
        {
            Some(candidate) => RecoveryAction::HandOff {
                to: candidate.worker.clone(),
                packet: Box::new(self.packet_for(stop, std::slice::from_ref(&candidate.worker))),
                checkpoint: stop.checkpoint(),
            },
            None => RecoveryAction::Escalate {
                reason: format!("{why}, and no agent of another family can retry it"),
            },
        }
    }

    /// Candidates on any vendor but the stopped one, which is out as a whole.
    fn others(&self, stop: &Stop<'_>) -> Shortlist {
        let mut shortlist = self.shortlist(stop, false, Vec::new());
        let vendor = stop.vendor();
        shortlist
            .candidates
            .retain(|candidate| candidate.worker.vendor() != vendor);
        shortlist
    }

    fn shortlist(
        &self,
        stop: &Stop<'_>,
        all_valid: bool,
        exclude_families: Vec<String>,
    ) -> Shortlist {
        let context = stop.context;
        self.handoff.shortlist(&CandidateQuery {
            card: context.card.clone(),
            role: context.role,
            stopped: context.worker.clone(),
            exclude_families,
            fallback: Vec::new(),
            all_valid,
        })
    }

    /// Auto mode: by the strategy, to the candidates auto may take. A race needs two of them.
    fn hand_off(
        &self,
        stop: &Stop<'_>,
        reason: String,
        until: Option<OffsetDateTime>,
        mut shortlist: Shortlist,
    ) -> RecoveryAction {
        let eligible: Vec<&Candidate> = shortlist
            .candidates
            .iter()
            .filter(|candidate| auto_check(candidate).is_ok())
            .collect();
        let racers = usize::try_from(shortlist.race_count).unwrap_or(usize::MAX);
        if shortlist.strategy == HandoffStrategy::Race && racers >= 2 && eligible.len() >= 2 {
            let to: Vec<WorkerRef> = eligible
                .iter()
                .take(racers)
                .map(|candidate| candidate.worker.clone())
                .collect();
            return RecoveryAction::Race {
                packet: Box::new(self.packet_for(stop, &to)),
                to,
                checkpoint: stop.checkpoint(),
            };
        }
        let to = if shortlist.strategy == HandoffStrategy::Spread {
            self.spread_pick(stop, &mut shortlist.candidates)
        } else {
            eligible.first().map(|candidate| candidate.worker.clone())
        };
        match to {
            Some(to) => RecoveryAction::HandOff {
                packet: Box::new(self.packet_for(stop, std::slice::from_ref(&to))),
                to,
                checkpoint: stop.checkpoint(),
            },
            None => self.ask(stop, reason, until, shortlist),
        }
    }

    /// The valid candidate `spread` gives this card, noted until the log shows it, so the
    /// vendor's next stop goes to someone else.
    fn spread_pick(&self, stop: &Stop<'_>, candidates: &mut [Candidate]) -> Option<WorkerRef> {
        let mut picks = self.picks();
        let run = picks.entry(stop.context.run.clone()).or_default();
        let placements = overlay(&stop.placements, run);
        balance(candidates, |worker| placements.handed_to(worker));
        let to = candidates
            .iter()
            .find(|candidate| auto_check(candidate).is_ok())
            .map(|candidate| candidate.worker.clone());
        match &to {
            Some(to) => {
                run.insert(
                    stop.context.card.id.clone(),
                    (to.clone(), stop.placements.last_seq),
                );
            }
            None if run.is_empty() => {
                picks.remove(&stop.context.run);
            }
            None => {}
        }
        to
    }

    /// The run's placements with the `spread` picks the log doesn't show yet.
    fn placements(&self, stop: &Stop<'_>) -> Placements {
        let mut picks = self.picks();
        let Some(run) = picks.get_mut(&stop.context.run) else {
            return stop.placements.clone();
        };
        let placements = overlay(&stop.placements, run);
        if run.is_empty() {
            picks.remove(&stop.context.run);
        }
        placements
    }

    /// A panic elsewhere can't leave the picks half written, so a poisoned lock is still good.
    fn picks(&self) -> MutexGuard<'_, HashMap<RunId, Picks>> {
        self.picks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks, or after the ask timeout decides as auto would.
    fn ask_until_timeout(
        &self,
        stop: &Stop<'_>,
        reason: String,
        shortlist: Shortlist,
    ) -> RecoveryAction {
        if stop.timed_out {
            self.hand_off(stop, reason, None, shortlist)
        } else {
            self.ask(stop, reason, None, shortlist)
        }
    }

    fn ask(
        &self,
        stop: &Stop<'_>,
        reason: String,
        resets_at: Option<OffsetDateTime>,
        mut shortlist: Shortlist,
    ) -> RecoveryAction {
        let context = stop.context;
        let mut also_moving = Vec::new();
        if shortlist.strategy == HandoffStrategy::Spread {
            let placements = self.placements(stop);
            balance(&mut shortlist.candidates, |worker| {
                placements.handed_to(worker)
            });
            if stop.vendor_wide() {
                also_moving = placements.on_vendor(&stop.vendor(), &context.card.id);
            }
        }
        let reason = if shortlist.candidates.is_empty() {
            format!("{reason} · no valid replacement")
        } else {
            reason
        };
        RecoveryAction::Ask(Box::new(HandoffPlan {
            card: context.card.id.clone(),
            title: context.card.title.clone(),
            stopped: context.worker.clone(),
            reason,
            resets_at,
            work: stop.packet.work.stat,
            gates_ran: stop.gates_ran,
            candidates: shortlist.candidates,
            mode: stop.rules.mode,
            strategy: shortlist.strategy,
            race_count: shortlist.race_count,
            also_moving,
        }))
    }

    /// The packet with any instructions the replacements' fallback entries carry.
    fn packet_for(&self, stop: &Stop<'_>, to: &[WorkerRef]) -> HandoffPacket {
        let mut packet = stop.packet.clone();
        for worker in to {
            let Some(rules) = entry_rules(&stop.rules.candidates, worker) else {
                continue;
            };
            let whom = if to.len() > 1 {
                format!(" for {worker}")
            } else {
                String::new()
            };
            if let Some(instructions) = &rules.instructions {
                packet
                    .notes
                    .push(format!("Instructions{whom}: {instructions}"));
            }
            if rules.read_only {
                packet
                    .notes
                    .push(format!("Read-only{whom}: don't edit files on this card."));
            }
        }
        packet
    }
}

/// What a card handed away from a vendor does once that vendor is healthy again.
#[derive(Debug, Clone, PartialEq)]
pub enum OriginalBack {
    /// No quality debt and no `hand_back`, so the card stays with its holder, and the notice
    /// says how to move it back.
    Stays { card: CardId, notice: String },
    /// `on_return = "hand_back"`, or `return_to_original`: the original takes the card at its
    /// next attempt.
    HandBack { card: CardId, to: WorkerRef },
    /// The card carries quality debt, so `on_return` decides, and `ask` opens
    /// `OriginalReturned`.
    Debt {
        card: CardId,
        original: WorkerRef,
        holder: WorkerRef,
        on_return: OnReturn,
    },
}

impl Limits {
    /// For each open card handed away from `vendor` that still sits elsewhere, what its return
    /// means, read from the run's events. `name` is the vendor's display name. A running step
    /// finishes first, so the engine applies each outcome at the card's next attempt.
    pub fn original_returned(
        &self,
        vendor: &VendorId,
        name: &str,
        events: &[RunEvent],
    ) -> Vec<OriginalBack> {
        let mut cards: BTreeMap<CardId, AwayCard> = BTreeMap::new();
        for event in events {
            let Some(card) = card_of(event) else {
                continue;
            };
            cards.entry(card.clone()).or_default().follow(&event.kind);
        }
        // Handoffs move a card's owner, so the owner's rules decide.
        let on_return = applied(&self.settings, RoleName::Owner, vendor).on_return;
        cards
            .into_iter()
            .filter(|(_, away)| !away.done)
            .filter_map(|(card, away)| {
                let holder = away.holder?;
                let original = away
                    .taken_from
                    .into_iter()
                    .find(|worker| &worker.vendor() == vendor)?;
                Some(if away.debt {
                    OriginalBack::Debt {
                        card,
                        original,
                        holder,
                        on_return,
                    }
                } else if on_return == OnReturn::HandBack {
                    OriginalBack::HandBack { card, to: original }
                } else {
                    let notice = format!(
                        "{name} is back · {card} stays with {holder} · /reassign {card} {original} moves it back"
                    );
                    OriginalBack::Stays { card, notice }
                })
            })
            .collect()
    }
}

/// One card's owners as the events tell them.
#[derive(Default)]
struct AwayCard {
    holder: Option<WorkerRef>,
    /// Workers the card was handed away from, oldest first, while it sits on another vendor.
    taken_from: Vec<WorkerRef>,
    /// The worker whose stop asked for the pending handoff.
    stopped: Option<WorkerRef>,
    debt: bool,
    done: bool,
}

impl AwayCard {
    fn follow(&mut self, kind: &RunEventKind) {
        match kind {
            RunEventKind::Handoff(HandoffEvent::Needed(plan)) => {
                self.stopped = Some(plan.stopped.clone());
            }
            RunEventKind::Handoff(HandoffEvent::HandedOff { to, .. }) => {
                let from = self.stopped.take().or_else(|| self.holder.clone());
                if let Some(from) = from
                    && !self.taken_from.contains(&from)
                {
                    self.taken_from.push(from);
                }
                self.moved_to(to);
            }
            RunEventKind::Card(
                CardEvent::Attempt { worker: to, .. } | CardEvent::Reassigned { to, .. },
            ) => self.moved_to(to),
            RunEventKind::Card(CardEvent::QualityDebt(_)) => self.debt = true,
            RunEventKind::Card(CardEvent::DebtCleared { .. }) => self.debt = false,
            RunEventKind::Card(CardEvent::State { to, .. }) => {
                self.done = matches!(to, CardState::Integrated | CardState::Cancelled);
            }
            _ => {}
        }
    }

    /// A card back on a vendor it was taken from is no longer away from it.
    fn moved_to(&mut self, to: &WorkerRef) {
        let vendor = to.vendor();
        self.taken_from.retain(|from| from.vendor() != vendor);
        self.holder = Some(to.clone());
    }
}

fn card_of(event: &RunEvent) -> Option<&CardId> {
    match &event.kind {
        RunEventKind::Handoff(HandoffEvent::Needed(plan)) => Some(&plan.card),
        RunEventKind::Handoff(HandoffEvent::HandedOff { card, .. }) => Some(card),
        _ => event.card_id.as_ref(),
    }
}

impl LimitsService for Limits {
    fn session_limits(&self, card: Option<&CardSpec>, role: RoleName) -> SessionLimits {
        let settings = &self.settings;
        let lifetime = settings
            .roles
            .get(&role)
            .and_then(|role| role.lifetime)
            .unwrap_or(settings.sessions.lifetime);
        // A session that lives past one card answers to the run's budget lines, not a card's.
        let per_card = card.is_some() || lifetime == SessionLifetime::Card;
        let own = card.map(|card| &card.budget);
        let budget = if per_card {
            own.and_then(|budget| budget.limit)
                .or_else(|| per_card_budget(&settings.budget))
                .filter(|_| settings.budget.enabled)
        } else {
            None
        };
        let watchdogs = &settings.watchdogs;
        SessionLimits {
            budget,
            soft_stop: soft_stop(settings.budget.soft_stop),
            max_minutes: own
                .and_then(|budget| budget.max_minutes)
                .or(per_card.then_some(watchdogs.wall_clock_min))
                .filter(|minutes| *minutes > 0.0),
            max_tool_calls: own
                .and_then(|budget| budget.max_tool_calls)
                .or(per_card.then_some(watchdogs.max_tool_calls))
                .filter(|calls| *calls > 0),
            inactivity_min: own
                .and_then(|budget| budget.inactivity_min)
                .or(Some(watchdogs.inactivity_min))
                .filter(|minutes| *minutes > 0.0),
            repeat_limit: Some(watchdogs.repeat_limit).filter(|limit| *limit > 0),
            native_cap_usd: budget
                .filter(|budget| budget.unit == BudgetUnit::Usd)
                .map(|budget| budget.amount * NATIVE_CAP_MARGIN),
        }
    }

    fn after_stop<'a>(&'a self, context: &'a StopContext) -> BoxFuture<'a, RecoveryAction> {
        Box::pin(self.recover(context, false))
    }

    /// Every vendor among `workers` is down or out of quota. The run parks until the earliest
    /// time one is known to come back.
    fn all_out(&self, workers: &[WorkerRef]) -> Option<ParkReason> {
        if workers.is_empty() {
            return None;
        }
        let now = OffsetDateTime::now_utc();
        let mut vendors: Vec<VendorId> = workers.iter().map(WorkerRef::vendor).collect();
        vendors.sort();
        vendors.dedup();
        let mut until: Option<OffsetDateTime> = None;
        for vendor in &vendors {
            let health = self.health.state(vendor);
            let rejected = self
                .quota
                .get(vendor)
                .filter(|reading| out_of_quota(reading, now));
            let down = health.state == HealthState::Down;
            if !down && rejected.is_none() {
                return None;
            }
            // Out for both reasons, it's back only when both clear.
            let times = [
                down.then_some(health.next_test_at),
                rejected.map(|reading| reading.resets_at),
            ];
            let known: Option<Vec<OffsetDateTime>> = times.into_iter().flatten().collect();
            if let Some(back) = known.and_then(|times| times.into_iter().max()) {
                until = Some(until.map_or(back, |until| until.min(back)));
            }
        }
        Some(ParkReason::AllVendorsOut { until })
    }
}

/// The model the CLI reported at the start of the worker's latest session on the card.
fn reported_model(events: &[RunEvent], worker: &WorkerRef) -> Option<String> {
    let session = events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started)) if &started.worker == worker => {
            Some(event.session.as_ref())
        }
        _ => None,
    })??;
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Agent(AgentEvent::Init {
            model: Some(model), ..
        }) if event.session.as_ref() == Some(session) => Some(model.clone()),
        _ => None,
    })
}

/// The first model along the registry's successor chain that isn't retired, in the stopped
/// model's family. It stays on the same worker, since another one could bill you differently.
fn successor_of(
    registry: &dyn ModelRegistry,
    worker: &WorkerRef,
    model: Option<&str>,
) -> Option<WorkerRef> {
    let stopped = WorkerRef::new(worker.worker.clone(), model.map(str::to_owned));
    let entry = registry.get(&stopped).or_else(|| registry.get(worker))?;
    let mut current = entry.clone();
    for _ in 0..MAX_SUCCESSOR_HOPS {
        let to = successor_ref(&current)?;
        if to.worker != worker.worker || to == stopped || registry.family(&to) != entry.family {
            return None;
        }
        match registry.get(&to) {
            Some(later) if later.retired => current = later,
            _ => return Some(to),
        }
    }
    None
}

/// A registry `successor` is a full `worker/model` reference, or a model on the entry's own
/// worker.
fn successor_ref(entry: &ModelEntry) -> Option<WorkerRef> {
    let successor = entry.successor.as_deref()?;
    if !successor.contains('/') {
        return Some(WorkerRef::new(
            entry.id.worker.clone(),
            Some(successor.to_owned()),
        ));
    }
    match successor.parse() {
        Ok(to) => Some(to),
        Err(error) => {
            tracing::warn!(model = %entry.id, %error, "the registry's successor can't be read");
            None
        }
    }
}

/// A card's budget from the `per_card_*` key that's set, when budgets are on.
pub fn per_card_budget(budget: &BudgetSettings) -> Option<BudgetAmount> {
    if !budget.enabled {
        return None;
    }
    let amount = |unit, amount| Some(BudgetAmount { unit, amount });
    if let Some(usd) = budget.per_card_usd {
        return amount(BudgetUnit::Usd, usd);
    }
    if let Some(tokens) = budget.per_card_tokens {
        return amount(BudgetUnit::Tokens, tokens as f64);
    }
    if let Some(share) = budget.per_card_quota {
        return amount(BudgetUnit::Quota, share);
    }
    budget
        .per_card_minutes
        .and_then(|minutes| amount(BudgetUnit::Minutes, minutes))
}

/// A ratio at or below zero would send the wrap-up at the start, so it falls back too.
fn soft_stop(ratio: f64) -> f64 {
    if ratio.is_nan() || ratio <= 0.0 {
        DEFAULT_SOFT_RATIO
    } else {
        ratio.min(1.0)
    }
}

enum Out {
    Quota(Option<OffsetDateTime>),
    Outage,
}

/// One stop, with what the decision reads.
struct Stop<'a> {
    context: &'a StopContext,
    rules: AppliedHandoff,
    /// Earlier handoffs of this card.
    handed_off: u32,
    /// Gates ran on the card's latest commit.
    gates_ran: bool,
    family: String,
    /// The model it ran, when known.
    model: Option<String>,
    packet: HandoffPacket,
    timed_out: bool,
    placements: Placements,
}

impl Stop<'_> {
    fn vendor(&self) -> VendorId {
        self.context.worker.vendor()
    }

    fn mode(&self) -> HandoffMode {
        if self.timed_out {
            HandoffMode::Auto
        } else {
            self.rules.mode
        }
    }

    fn reason(&self) -> String {
        self.packet.why_stopped.clone()
    }

    /// The whole vendor stopped, not only its model, so its other cards stop too.
    fn vendor_wide(&self) -> bool {
        !matches!(
            self.context.end.reason,
            EndReason::ModelUnavailable { .. }
                | EndReason::VendorError {
                    category: ErrorCategory::ModelUnavailable,
                    ..
                }
        )
    }

    fn checkpoint(&self) -> Option<CheckpointTag> {
        self.rules.checkpoint.then(|| CheckpointTag {
            run: self.context.run.clone(),
            card: self.context.card.id.clone(),
            n: self.handed_off + 1,
        })
    }
}

/// The log's placements with `picks` laid over them. A pick the log has since caught up with is
/// dropped.
fn overlay(log: &Placements, picks: &mut Picks) -> Placements {
    let mut placements = log.clone();
    picks.retain(|card, (worker, seen)| placements.pick(card, worker, *seen));
    placements
}

fn too_many_handoffs(stop: &Stop<'_>) -> RecoveryAction {
    RecoveryAction::Escalate {
        reason: format!(
            "{} stopped after {} handoffs, the most handoff.max_handoffs allows",
            stop.context.card.id, stop.handed_off
        ),
    }
}

fn with_detail(reason: String, detail: &str) -> String {
    let detail = one_line(detail, 120);
    if detail.is_empty() {
        reason
    } else {
        format!("{reason} ({detail})")
    }
}

#[cfg(test)]
#[path = "handoff/recovery_tests.rs"]
mod tests;
