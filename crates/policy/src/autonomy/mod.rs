//! Earning and losing autonomy: shadow records scored against your answers, spot checks,
//! `auto-off`, and the daily caps. Everything fails closed: records that can't be read or
//! written leave every checkpoint supervised, and auto asks.

pub(crate) mod file_lock;
pub mod records;
pub mod scoring;
pub mod spot_check;
pub mod view;

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use argonauts_core::clock::Clock;
use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::layout::user_files;
use argonauts_core::paths::write_private_file;
use argonauts_core::policy::{
    ApprovalsView, DecisionPoint, EvidencePacket, HardLimit, ShadowRecord, UserDecision, Verdict,
    Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use time::OffsetDateTime;

use crate::daily::{DailyError, DailyLedger};
use crate::parts::{Autonomy, Refusal, Reservation};
use records::{Event, Record, Records};
use scoring::Scope;

pub struct StateAutonomy {
    /// `~/.argonauts/state/<repo>`.
    state_dir: PathBuf,
    records: Records,
    clock: Arc<dyn Clock>,
    /// Fixed in tests; otherwise read from, or created in, the install's state folder.
    salt: Option<[u8; 32]>,
    /// Why a record or `auto-off` couldn't be written. A lost miss or demotion would raise
    /// autonomy, so this process stays supervised once one is lost.
    broken: Mutex<Option<String>>,
}

impl StateAutonomy {
    /// Nothing is read or created until the first call, so a missing folder is an error there,
    /// and auto asks instead.
    pub fn open(state_dir: &Path, clock: Arc<dyn Clock>) -> Self {
        Self {
            state_dir: state_dir.to_owned(),
            records: Records::in_state_dir(state_dir),
            clock,
            salt: None,
            broken: Mutex::new(None),
        }
    }

    /// A fixed spot-check salt, so sampling is stable in tests.
    pub fn with_salt(mut self, salt: [u8; 32]) -> Self {
        self.salt = Some(salt);
        self
    }

    fn records(&self) -> &Records {
        &self.records
    }

    /// `~/.argonauts/state`, which holds every repo's state folder and the install's salt.
    fn install_state_dir(&self) -> io::Result<&Path> {
        self.state_dir
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .ok_or_else(|| self.not_in_home())
    }

    /// `~/.argonauts`.
    fn home(&self) -> io::Result<&Path> {
        self.install_state_dir()?
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .ok_or_else(|| self.not_in_home())
    }

    fn repo(&self) -> io::Result<&str> {
        self.state_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| self.not_in_home())
    }

    fn not_in_home(&self) -> io::Error {
        io::Error::other(format!(
            "{} isn't a repo's folder under ~/.argonauts/state",
            self.state_dir.display()
        ))
    }

    fn salt(&self) -> io::Result<[u8; 32]> {
        match self.salt {
            Some(salt) => Ok(salt),
            None => spot_check::load_or_create_salt(self.install_state_dir()?),
        }
    }

    fn ledger(&self) -> io::Result<DailyLedger> {
        Ok(DailyLedger::open(self.home()?))
    }

    fn broken(&self) -> Option<String> {
        self.broken
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn break_with(&self, reason: String) {
        let mut broken = self.broken.lock().unwrap_or_else(PoisonError::into_inner);
        broken.get_or_insert(reason);
    }

    /// `auto-off` is there, or can't be checked.
    fn auto_is_off(&self) -> bool {
        let Ok(home) = self.home() else {
            return true;
        };
        match fs::symlink_metadata(home.join(user_files::AUTO_OFF)) {
            Ok(_) => true,
            Err(err) => err.kind() != io::ErrorKind::NotFound,
        }
    }

    fn supervised_everywhere(&self) -> bool {
        self.broken().is_some() || self.auto_is_off()
    }

    fn write(&self, what: &str, change: impl FnOnce(&[Record]) -> Vec<Record>) {
        let written = self.records().lock().and_then(|mut locked| {
            let new = change(locked.records());
            locked.append(&new)
        });
        if let Err(err) = written {
            self.break_with(format!("the approval records couldn't take {what}: {err}"));
        }
    }

    fn record_decision(&self, decision: &UserDecision, settings: &Settings) -> io::Result<()> {
        let point = &decision.point;
        let now = self.clock.now();
        let answer = scoring::answer(&decision.action, decision.edited);
        let current = scoring::point_settings(&settings.approvals, point.kind);

        let mut locked = self.records().lock()?;
        let log = records::checkpoint_log(locked.records(), &point.run, &point.checkpoint);
        if log.answer.is_some() {
            return Ok(());
        }
        // The first shadow verdict to arrive is the one scored. A reservation means auto
        // approved, but your answer came first. A shadow verdict carried only by your answer
        // leaves the rules document unknown.
        let shadow = if log.scored {
            None
        } else if let Some((basis, would)) = log.shadow {
            Some((basis.fingerprint, basis.rules, would))
        } else if let (Some(shadow), Some(current)) = (&decision.shadow, &current) {
            Some((current.fingerprint.as_str(), None, shadow.would))
        } else {
            log.reservation.and_then(|reserved| {
                let fingerprint = reserved.fingerprint?;
                Some((fingerprint, reserved.rules, Verdict::Approve))
            })
        };
        let released_merge = log
            .reservation
            .map(|reserved| reserved.merge_key.map(str::to_owned));

        let mut new = vec![Record::new(point, now, Event::Answered { answer })];
        if let Some((fingerprint, rules, would)) = shadow
            && let Some(outcome) = scoring::score(would, answer)
        {
            new.push(Record::new(
                point,
                now,
                Event::Scored {
                    fingerprint: fingerprint.to_owned(),
                    rules: rules.map(str::to_owned),
                    outcome,
                },
            ));
        }
        if released_merge.is_some() {
            new.push(Record::new(point, now, Event::Released));
        }
        locked.append(&new)?;

        if let Some(Some(key)) = released_merge {
            // Still under the records lock, so a reservation racing this answer sees it. A
            // failed release keeps the auto-merge taken, the safe side.
            let _ = self
                .ledger()
                .map(|ledger| ledger.release_auto_merge(&key, now));
        }
        Ok(())
    }

    /// Lifts the kind's demotion after a wrong spot check or a revert, so a kind set to auto
    /// decides on its own again. Only you may do this, from the interactive app, since it
    /// raises autonomy.
    pub fn reinstate(&self, kind: CheckpointKind) -> io::Result<()> {
        let now = self.clock.now();
        let mut locked = self.records().lock()?;
        if !scoring::tally(locked.records(), kind, Scope::AllRecipes, None, now).demoted {
            return Ok(());
        }
        locked.append(&[Record::for_kind(kind, now, Event::Reinstated)])
    }
}

/// Too many spot checks waiting pause new auto-approvals until you catch up.
fn spot_check_pause(records: &[Record], settings: &Settings) -> Option<String> {
    let limit = settings.approvals.pause_auto_when_spot_checks_waiting;
    let waiting = records::spot_checks_waiting(records);
    (limit > 0 && waiting >= limit).then(|| {
        let noun = if waiting == 1 {
            "spot check is"
        } else {
            "spot checks are"
        };
        format!("{waiting} {noun} waiting, so auto asks until you catch up")
    })
}

fn daily_refusal(error: DailyError, limit: HardLimit) -> Refusal {
    match error {
        DailyError::Capped(detail) => Refusal::Capped(vec![Violation {
            limit,
            detail,
            paths: Vec::new(),
        }]),
        DailyError::Io { .. } => Refusal::Unavailable(error.to_string()),
    }
}

const AUTO_IS_OFF: &str = "auto is off, so every approval asks";

fn unavailable(err: io::Error) -> Refusal {
    Refusal::Unavailable(records_error(&err))
}

fn records_error(err: &io::Error) -> String {
    format!("the approval records: {err}")
}

impl Autonomy for StateAutonomy {
    fn effective_mode(&self, point: &DecisionPoint, configured: ApprovalMode) -> ApprovalMode {
        if configured == ApprovalMode::Supervised || self.supervised_everywhere() {
            return ApprovalMode::Supervised;
        }
        let Ok(records) = self.records().read_all() else {
            return ApprovalMode::Supervised;
        };
        let scope = Scope::Recipe(point.recipe.as_deref());
        let tally = scoring::tally(&records, point.kind, scope, None, self.clock.now());
        // Full auto stays itself: nobody answers spot checks there to win autonomy back.
        if configured == ApprovalMode::Auto && tally.demoted {
            ApprovalMode::AutoPreview
        } else {
            configured
        }
    }

    fn auto_paused(&self, settings: &Settings) -> Option<String> {
        if let Some(reason) = self.broken() {
            return Some(reason);
        }
        if self.auto_is_off() {
            return Some(AUTO_IS_OFF.to_owned());
        }
        match self.records().read_all() {
            Ok(records) => spot_check_pause(&records, settings),
            Err(err) => Some(format!("the approval records can't be read: {err}")),
        }
    }

    fn reserve(
        &self,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
    ) -> Result<Reservation, Refusal> {
        if let Some(reason) = self.broken() {
            return Err(Refusal::Unavailable(reason));
        }
        // Checked again here, so `auto off` stops an auto-approval already being decided.
        if self.auto_is_off() {
            return Err(Refusal::Unavailable(AUTO_IS_OFF.to_owned()));
        }
        let now = self.clock.now();
        let mut locked = self.records().lock().map_err(unavailable)?;
        let log = records::checkpoint_log(locked.records(), &point.run, &point.checkpoint);
        if log.answer.is_some() {
            return Err(Refusal::AlreadyDecided);
        }
        if let Some(reserved) = log.reservation {
            return Ok(Reservation {
                spot_check: reserved.spot_check,
            });
        }
        // Checked again under the lock, since a wrong spot check, or a spot check drawn by
        // another run, can land while the approver model is deciding. Full auto goes on past
        // both, since nobody answers spot checks there; its daily caps below still hold.
        let scope = Scope::Recipe(point.recipe.as_deref());
        let autopilot = crate::approvals::in_autopilot(point);
        if !autopilot && scoring::tally(locked.records(), point.kind, scope, None, now).demoted {
            return Err(Refusal::Paused(
                "a wrong spot check or a revert put this kind of approval back in auto preview \
                 until you reinstate auto for it"
                    .to_owned(),
            ));
        }
        if let Some(reason) = spot_check_pause(locked.records(), settings).filter(|_| !autopilot) {
            return Err(Refusal::Paused(reason));
        }
        let limits = &settings.limits;
        let spent = &evidence.facts.spent;
        if limits.spend_per_day.is_some() && spent.cost_partial {
            return Err(Refusal::Unavailable(format!(
                "some of this run's usage has no cost, so its spend of at least ${:.2} can't be \
                 checked against the daily ceiling",
                spent.cost_usd
            )));
        }

        let repo = self.repo().map_err(unavailable)?;
        let ledger = self.ledger().map_err(unavailable)?;
        let spend_key = format!("{repo}/{}", point.run);
        ledger
            .record_spend(&spend_key, spent.cost_usd, limits.spend_per_day, now)
            .map_err(|err| daily_refusal(err, HardLimit::Ceiling))?;
        let merge_key = if point.kind == CheckpointKind::Merge {
            let key = format!("{repo}/{}/{}", point.run, point.checkpoint);
            ledger
                .take_auto_merge(&key, limits.auto_merges_per_day, now)
                .map_err(|err| daily_refusal(err, HardLimit::AutoMergeCap))?;
            Some(key)
        } else {
            None
        };

        let current = scoring::point_settings(&settings.approvals, point.kind);
        let rules = current
            .as_ref()
            .and_then(|current| scoring::rules_document(&settings.approvals, current, evidence));
        let fingerprint = current.map(|current| current.fingerprint);
        let reserved = self.salt().and_then(|salt| {
            let sample = settings.approvals.spot_check_sample;
            let spot_check = spot_check::sampled(&salt, &point.run, &point.checkpoint, sample);
            let event = Event::Reserved {
                fingerprint,
                rules,
                merge_key: merge_key.clone(),
                spot_check,
            };
            locked.append(&[Record::new(point, now, event)])?;
            Ok(spot_check)
        });
        match reserved {
            Ok(spot_check) => Ok(Reservation { spot_check }),
            Err(err) => {
                let mut why = records_error(&err);
                if let Some(key) = &merge_key
                    && let Err(release) = ledger.release_auto_merge(key, now)
                {
                    // Kept taken, the safe side, but today's cap is one short until tomorrow.
                    why.push_str(&format!(
                        "; the auto-merge it took stays counted for today: {release}"
                    ));
                }
                Err(Refusal::Unavailable(why))
            }
        }
    }

    fn note_shadow(
        &self,
        point: &DecisionPoint,
        shadow: &ShadowRecord,
        evidence: &EvidencePacket,
        settings: &Settings,
    ) {
        let Some(current) = scoring::point_settings(&settings.approvals, point.kind) else {
            return;
        };
        let rules = scoring::rules_document(&settings.approvals, &current, evidence);
        let now = self.clock.now();
        self.write("an auto preview verdict", |records| {
            let log = records::checkpoint_log(records, &point.run, &point.checkpoint);
            if log.scored || log.shadow.is_some() {
                return Vec::new();
            }
            let fingerprint = current.fingerprint;
            let event = match log.answer {
                Some(answer) => match scoring::score(shadow.would, answer) {
                    Some(outcome) => Event::Scored {
                        fingerprint,
                        rules,
                        outcome,
                    },
                    None => return Vec::new(),
                },
                None => Event::Shadow {
                    fingerprint,
                    rules,
                    would: shadow.would,
                },
            };
            vec![Record::new(point, now, event)]
        });
    }

    fn user_decided(&self, decision: &UserDecision, settings: &Settings) {
        if let Err(err) = self.record_decision(decision, settings) {
            self.break_with(format!(
                "the approval records couldn't take your answer: {err}"
            ));
        }
    }

    /// Gives back the auto-merge that a refused landing took, and records that the
    /// reservation is gone.
    fn landing_refused(&self, point: &DecisionPoint) {
        let now = self.clock.now();
        let mut released: Option<String> = None;
        self.write("a refused merge", |records| {
            let log = records::checkpoint_log(records, &point.run, &point.checkpoint);
            match log.reservation.and_then(|reserved| reserved.merge_key) {
                Some(key) => {
                    released = Some(key.to_owned());
                    vec![Record::new(point, now, Event::Released)]
                }
                None => Vec::new(),
            }
        });
        if let Some(key) = released {
            // A failed release keeps the auto-merge taken, the safe side.
            let _ = self
                .ledger()
                .map(|ledger| ledger.release_auto_merge(&key, now));
        }
    }

    fn spot_checked(&self, point: &DecisionPoint, correct: bool, settings: &Settings) {
        let now = self.clock.now();
        let fingerprint = scoring::point_settings(&settings.approvals, point.kind)
            .map(|current| current.fingerprint);
        self.write("a spot check", |records| {
            let log = records::checkpoint_log(records, &point.run, &point.checkpoint);
            // The host reports a revert again at every start, and it costs autonomy once.
            if !correct && log.wrong {
                return Vec::new();
            }
            let event = if correct || log.reservation.is_some() {
                Event::SpotChecked {
                    fingerprint,
                    correct,
                }
            } else {
                Event::Reverted
            };
            vec![Record::new(point, now, event)]
        });
    }

    fn auto_off(&self) {
        let now = self.clock.now();
        let written = self.home().and_then(|home| {
            let note = format!("auto off since {}\n", rfc3339(now));
            write_synced(&home.join(user_files::AUTO_OFF), note.as_bytes())
        });
        if let Err(err) = written {
            self.break_with(format!("auto-off couldn't be written: {err}"));
        }
    }

    fn view(&self, settings: &Settings) -> ApprovalsView {
        let now = self.clock.now();
        match self.records().read_all() {
            Ok(records) => view::build(&records, settings, now, self.supervised_everywhere()),
            Err(_) => view::build(&[], settings, now, true),
        }
    }
}

/// Synced with its folder, so `auto off` holds after a crash.
fn write_synced(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_private_file(path, contents)?;
    File::open(path)?.sync_all()?;
    match path.parent() {
        Some(dir) => File::open(dir)?.sync_all(),
        None => Ok(()),
    }
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| at.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::RunId;
    use argonauts_core::events::checkpoint::AnswerAction;
    use argonauts_core::names::CheckpointId;
    use argonauts_core::policy::{ApprovalRow, EvidenceDocument, EvidenceFacts};
    use argonauts_core::settings::approvals::DecideWith;
    use argonauts_testkit::FakeClock;
    use tempfile::TempDir;

    const N: u32 = 3;

    fn days(n: u64) -> std::time::Duration {
        std::time::Duration::from_secs(n * 24 * 60 * 60)
    }

    struct Fixture {
        home: TempDir,
        clock: Arc<FakeClock>,
        autonomy: StateAutonomy,
        settings: Settings,
    }

    fn fixture() -> Fixture {
        let home = tempfile::tempdir().expect("temp dir");
        let state_dir = home.path().join("state").join("repo-1234");
        fs::create_dir_all(&state_dir).expect("state dir");
        let clock = Arc::new(FakeClock::default());
        let autonomy = StateAutonomy::open(&state_dir, clock.clone()).with_salt([7; 32]);
        let mut settings = Settings::default();
        settings.approvals.suggest_auto_after_matches = N;
        Fixture {
            home,
            clock,
            autonomy,
            settings,
        }
    }

    fn point(run: &str, n: u32, kind: CheckpointKind) -> DecisionPoint {
        DecisionPoint {
            run: RunId::new(run).expect("run id"),
            checkpoint: CheckpointId::numbered(n),
            kind,
            recipe: None,
            authors: Vec::new(),
            mode_override: None,
            lead: None,
        }
    }

    fn would(verdict: Verdict) -> ShadowRecord {
        ShadowRecord {
            would: verdict,
            by: DecideWith::Rules,
            reasons: Vec::new(),
        }
    }

    fn decided(point: &DecisionPoint, action: AnswerAction, shadow: bool) -> UserDecision {
        UserDecision {
            point: point.clone(),
            action,
            edited: false,
            shadow: shadow.then(|| would(Verdict::Approve)),
        }
    }

    fn evidence(kind: CheckpointKind) -> EvidencePacket {
        EvidencePacket {
            kind,
            goal: None,
            facts: EvidenceFacts::default(),
            agent_outputs: Vec::new(),
            documents: Vec::new(),
        }
    }

    fn row(f: &Fixture, kind: CheckpointKind) -> ApprovalRow {
        let view = f.autonomy.view(&f.settings);
        view.points
            .into_iter()
            .find(|row| row.kind == kind)
            .expect("a row for the kind")
    }

    /// Auto would approve, and you approve too.
    fn agree(f: &Fixture, point: &DecisionPoint) {
        agree_on(f, point, &evidence(point.kind));
    }

    fn agree_on(f: &Fixture, point: &DecisionPoint, evidence: &EvidencePacket) {
        let action = if point.kind == CheckpointKind::Merge {
            AnswerAction::Merge
        } else {
            AnswerAction::Approve
        };
        f.autonomy
            .note_shadow(point, &would(Verdict::Approve), evidence, &f.settings);
        f.autonomy
            .user_decided(&decided(point, action, true), &f.settings);
    }

    /// Auto approved `point` on its own.
    fn auto_approved(f: &Fixture, point: &DecisionPoint) {
        let reserved = f
            .autonomy
            .reserve(point, &evidence(point.kind), &f.settings);
        assert!(reserved.is_ok(), "{reserved:?}");
    }

    #[test]
    fn earning_and_losing_autonomy() {
        let mut f = fixture();
        let goal = CheckpointKind::Goal;
        for n in 1..=N {
            agree(&f, &point("run-a", n, goal));
        }
        let earned = row(&f, goal);
        assert_eq!((earned.matches_in_a_row, earned.misses), (N, 0));
        assert!(earned.suggest_auto);
        assert!(!row(&f, CheckpointKind::Plan).suggest_auto);

        // Auto would have asked, so this one doesn't count either way.
        let asked = point("run-a", 10, goal);
        f.autonomy.note_shadow(
            &asked,
            &would(Verdict::Escalate),
            &evidence(asked.kind),
            &f.settings,
        );
        f.autonomy
            .user_decided(&decided(&asked, AnswerAction::SendBack, false), &f.settings);
        assert_eq!(row(&f, goal).matches_in_a_row, N);

        // Sent back what auto would have approved: a miss.
        let sent_back = point("run-a", 11, goal);
        f.autonomy.user_decided(
            &decided(&sent_back, AnswerAction::SendBack, true),
            &f.settings,
        );
        let missed = row(&f, goal);
        assert_eq!((missed.matches_in_a_row, missed.misses), (0, 1));
        assert!(!missed.suggest_auto);

        // An edit is a miss too, and so is any other answer that declines the approval.
        let edited = point("run-a", 12, goal);
        let mut with_edits = decided(&edited, AnswerAction::Approve, true);
        with_edits.edited = true;
        f.autonomy.user_decided(&with_edits, &f.settings);
        assert_eq!(row(&f, goal).misses, 2);
        let declines = [
            AnswerAction::PullRequest,
            AnswerAction::KeepBranch,
            AnswerAction::SwitchToSolo,
            AnswerAction::Wait,
            AnswerAction::UpgradeReview,
        ];
        for (n, action) in (13..).zip(declines) {
            let declined = point("run-a", n, goal);
            f.autonomy
                .user_decided(&decided(&declined, action, true), &f.settings);
        }
        assert_eq!(row(&f, goal).misses, 7);
        let looked = point("run-a", 19, goal);
        f.autonomy
            .user_decided(&decided(&looked, AnswerAction::ShowDiff, true), &f.settings);
        assert_eq!(row(&f, goal).misses, 7);

        for n in 20..20 + N {
            agree(&f, &point("run-b", n, goal));
        }
        assert_eq!(row(&f, goal).matches_in_a_row, N);

        // Switching the mode and `otherwise` keeps the count; `decide_with` restarts it.
        f.settings.approvals.goal.mode = ApprovalMode::Auto;
        f.settings.approvals.goal.otherwise =
            argonauts_core::settings::approvals::Otherwise::SendBack;
        assert_eq!(row(&f, goal).matches_in_a_row, N);
        f.settings.approvals.goal.decide_with = DecideWith::RulesOrModel;
        let restarted = row(&f, goal);
        assert_eq!((restarted.matches_in_a_row, restarted.misses), (0, 0));
        assert!(!restarted.suggest_auto);

        for n in 30..30 + N {
            agree(&f, &point("run-c", n, goal));
        }
        let goal_point = point("run-d", 1, goal);
        assert_eq!(
            f.autonomy.effective_mode(&goal_point, ApprovalMode::Auto),
            ApprovalMode::Auto
        );

        // A spot check you mark wrong drops the kind to shadow and restarts its count.
        auto_approved(&f, &goal_point);
        f.autonomy.spot_checked(&goal_point, true, &f.settings);
        assert_eq!(
            f.autonomy.effective_mode(&goal_point, ApprovalMode::Auto),
            ApprovalMode::Auto
        );
        f.autonomy.spot_checked(&goal_point, false, &f.settings);
        assert_eq!(
            f.autonomy.effective_mode(&goal_point, ApprovalMode::Auto),
            ApprovalMode::AutoPreview
        );
        let demoted = row(&f, goal);
        assert_eq!(demoted.mode, ApprovalMode::AutoPreview);
        assert_eq!((demoted.matches_in_a_row, demoted.misses), (0, 1));
        // Never more autonomous than configured, and other kinds keep their mode.
        assert_eq!(
            f.autonomy
                .effective_mode(&goal_point, ApprovalMode::Supervised),
            ApprovalMode::Supervised
        );
        let plan_point = point("run-d", 2, CheckpointKind::Plan);
        assert_eq!(
            f.autonomy.effective_mode(&plan_point, ApprovalMode::Auto),
            ApprovalMode::Auto
        );

        // Earning the matches again only suggests auto. You reinstate it yourself.
        for n in 40..40 + N {
            agree(&f, &point("run-e", n, goal));
        }
        assert_eq!(
            f.autonomy.effective_mode(&goal_point, ApprovalMode::Auto),
            ApprovalMode::AutoPreview
        );
        let earned_back = row(&f, goal);
        assert_eq!(earned_back.mode, ApprovalMode::AutoPreview);
        assert_eq!(earned_back.matches_in_a_row, N);
        assert!(earned_back.suggest_auto);
        f.autonomy.reinstate(goal).expect("reinstate");
        assert_eq!(
            f.autonomy.effective_mode(&goal_point, ApprovalMode::Auto),
            ApprovalMode::Auto
        );
        assert_eq!(row(&f, goal).mode, ApprovalMode::Auto);

        // `auto_off` is supervised everywhere until the file is removed.
        f.autonomy.auto_off();
        let auto_off = f.home.path().join(user_files::AUTO_OFF);
        assert!(auto_off.exists());
        assert_eq!(
            f.autonomy.auto_paused(&f.settings).as_deref(),
            Some(AUTO_IS_OFF)
        );
        for configured in [ApprovalMode::Auto, ApprovalMode::AutoPreview] {
            assert_eq!(
                f.autonomy.effective_mode(&plan_point, configured),
                ApprovalMode::Supervised
            );
        }
        assert!(
            f.autonomy
                .view(&f.settings)
                .points
                .iter()
                .all(|row| row.mode == ApprovalMode::Supervised)
        );
        let refused = f
            .autonomy
            .reserve(&plan_point, &evidence(CheckpointKind::Plan), &f.settings);
        assert!(matches!(refused, Err(Refusal::Unavailable(_))));
        fs::remove_file(&auto_off).expect("remove auto-off");
        assert_eq!(
            f.autonomy.effective_mode(&plan_point, ApprovalMode::Auto),
            ApprovalMode::Auto
        );
    }

    #[test]
    fn revert_of_auto_merge_demotes_to_shadow() {
        let mut f = fixture();
        let merge = CheckpointKind::Merge;
        for n in 1..=N {
            agree(&f, &point(&format!("run-{n}"), 5, merge));
        }
        let fresh = row(&f, merge);
        assert_eq!(fresh.matches_in_a_row, 0, "merge matches wait 7 days");
        assert!(!fresh.suggest_auto);

        f.clock.advance(days(6));
        assert_eq!(row(&f, merge).matches_in_a_row, 0);
        f.clock.advance(days(1));
        let aged = row(&f, merge);
        assert_eq!(aged.matches_in_a_row, N);
        assert!(aged.suggest_auto);

        f.settings.approvals.merge.mode = ApprovalMode::Auto;
        f.settings.approvals.spot_check_sample = 0.0;
        let auto_merge = point("run-auto", 5, merge);
        assert_eq!(
            f.autonomy.effective_mode(&auto_merge, ApprovalMode::Auto),
            ApprovalMode::Auto
        );
        let reserved = f
            .autonomy
            .reserve(&auto_merge, &evidence(merge), &f.settings);
        assert_eq!(reserved, Ok(Reservation { spot_check: false }));

        // The host found a revert of that merge on the target branch.
        f.clock.advance(days(2));
        f.autonomy.spot_checked(&auto_merge, false, &f.settings);
        assert_eq!(
            f.autonomy.effective_mode(&auto_merge, ApprovalMode::Auto),
            ApprovalMode::AutoPreview
        );
        let reverted = row(&f, merge);
        assert_eq!(reverted.mode, ApprovalMode::AutoPreview);
        assert_eq!((reverted.matches_in_a_row, reverted.misses), (0, 1));
        let goal = point("run-auto", 1, CheckpointKind::Goal);
        assert_eq!(
            f.autonomy.effective_mode(&goal, ApprovalMode::Auto),
            ApprovalMode::Auto
        );

        // The host reports the revert again at every start, and it costs autonomy only once.
        for n in 1..N {
            agree(&f, &point(&format!("run-again-{n}"), 5, merge));
        }
        f.clock.advance(days(7));
        f.autonomy.spot_checked(&auto_merge, false, &f.settings);
        f.autonomy.spot_checked(&auto_merge, false, &f.settings);
        let again = row(&f, merge);
        assert_eq!((again.matches_in_a_row, again.misses), (N - 1, 1));
    }

    #[test]
    fn a_reverted_shadow_merge_never_counts() {
        let f = fixture();
        let merge = CheckpointKind::Merge;
        let merges: Vec<_> = (1..=N)
            .map(|n| point(&format!("run-{n}"), 5, merge))
            .collect();
        for merged in &merges {
            agree(&f, merged);
        }

        // You merged it yourself in shadow, and the host found a revert within 7 days.
        f.clock.advance(days(3));
        f.autonomy.spot_checked(&merges[1], false, &f.settings);
        f.autonomy.spot_checked(&merges[1], false, &f.settings);
        f.clock.advance(days(4));
        let aged = row(&f, merge);
        assert_eq!((aged.matches_in_a_row, aged.misses), (1, 1));
        assert!(!aged.suggest_auto);
        // Auto didn't act, so the kind isn't demoted.
        assert_eq!(
            f.autonomy.effective_mode(&merges[0], ApprovalMode::Auto),
            ApprovalMode::Auto
        );
    }

    #[test]
    fn a_demotion_or_pause_during_a_decision_stops_it() {
        let mut f = fixture();
        let goal = CheckpointKind::Goal;
        let deciding = point("run-a", 1, goal);
        assert_eq!(
            f.autonomy.effective_mode(&deciding, ApprovalMode::Auto),
            ApprovalMode::Auto
        );
        // While the approver model decides, you mark another goal approval wrong.
        let other = point("run-b", 1, goal);
        auto_approved(&f, &other);
        f.autonomy.spot_checked(&other, false, &f.settings);
        let refused = f.autonomy.reserve(&deciding, &evidence(goal), &f.settings);
        assert!(matches!(refused, Err(Refusal::Paused(_))), "{refused:?}");

        // Another run's approval fills the spot-check queue while this one decides.
        f.settings.approvals.spot_check_sample = 1.0;
        f.settings.approvals.pause_auto_when_spot_checks_waiting = 1;
        let plan = CheckpointKind::Plan;
        let first = point("run-c", 1, plan);
        let second = point("run-d", 1, plan);
        assert_eq!(f.autonomy.auto_paused(&f.settings), None);
        auto_approved(&f, &first);
        let refused = f.autonomy.reserve(&second, &evidence(plan), &f.settings);
        assert!(matches!(refused, Err(Refusal::Paused(_))), "{refused:?}");
    }

    #[test]
    fn a_new_rules_document_restarts_the_count() {
        let mut f = fixture();
        let plan = CheckpointKind::Plan;
        f.settings.approvals.plan.decide_with = DecideWith::Model;
        let with_rules = |text: &str| {
            let mut packet = evidence(plan);
            packet.documents.push(EvidenceDocument {
                label: crate::approver::render::rules_label(
                    &f.settings.approvals.approver.model_rules,
                ),
                text: text.to_owned(),
                path: None,
                sha256: None,
            });
            packet
        };
        let strict = with_rules("R1: approve only small plans.");
        let loose = with_rules("R1: approve every plan.");
        for n in 1..=N {
            agree_on(&f, &point("run-a", n, plan), &strict);
        }
        assert_eq!(row(&f, plan).matches_in_a_row, N);
        agree_on(&f, &point("run-b", 1, plan), &loose);
        let restarted = row(&f, plan);
        assert_eq!((restarted.matches_in_a_row, restarted.misses), (1, 0));
    }

    #[test]
    fn a_partly_known_spend_refuses_under_a_daily_ceiling() {
        let mut f = fixture();
        let plan = CheckpointKind::Plan;
        let mut partial = evidence(plan);
        partial.facts.spent.cost_usd = 1.0;
        partial.facts.spent.cost_partial = true;
        f.settings.limits.spend_per_day = None;
        let first = point("run-a", 1, plan);
        assert!(f.autonomy.reserve(&first, &partial, &f.settings).is_ok());

        f.settings.limits.spend_per_day = Some(100.0);
        let second = point("run-a", 2, plan);
        let refused = f.autonomy.reserve(&second, &partial, &f.settings);
        assert!(
            matches!(refused, Err(Refusal::Unavailable(_))),
            "{refused:?}"
        );
    }

    /// The review's probe: each refused landing took one of the day's auto-merges, so a
    /// dirty checkout used up the cap for every other run. Now it gives it back.
    #[test]
    fn a_refused_landing_gives_back_its_auto_merge() {
        let mut f = fixture();
        let merge = CheckpointKind::Merge;
        f.settings.approvals.merge.mode = ApprovalMode::Auto;
        f.settings.limits.auto_merges_per_day = Some(1);
        let first = point("run-a", 1, merge);
        assert!(
            f.autonomy
                .reserve(&first, &evidence(merge), &f.settings)
                .is_ok()
        );
        let second = point("run-b", 1, merge);
        assert!(matches!(
            f.autonomy.reserve(&second, &evidence(merge), &f.settings),
            Err(Refusal::Capped(_))
        ));

        f.autonomy.landing_refused(&first);
        assert!(
            f.autonomy
                .reserve(&second, &evidence(merge), &f.settings)
                .is_ok(),
            "the refused landing's auto-merge went back"
        );
        // Giving it back twice, or for a point that took none, changes nothing.
        f.autonomy.landing_refused(&first);
        f.autonomy.landing_refused(&point("run-c", 1, merge));
        let third = point("run-d", 1, merge);
        assert!(matches!(
            f.autonomy.reserve(&third, &evidence(merge), &f.settings),
            Err(Refusal::Capped(_))
        ));
    }

    #[test]
    fn user_answer_first_releases_the_reservation() {
        let mut f = fixture();
        let merge = CheckpointKind::Merge;
        f.settings.approvals.merge.mode = ApprovalMode::Auto;
        f.settings.approvals.spot_check_sample = 1.0;
        f.settings.limits.auto_merges_per_day = Some(1);

        let first = point("run-a", 7, merge);
        let reserved = f.autonomy.reserve(&first, &evidence(merge), &f.settings);
        assert_eq!(reserved, Ok(Reservation { spot_check: true }));
        assert_eq!(f.autonomy.view(&f.settings).spot_checks_waiting, 1);
        let second = point("run-b", 7, merge);
        let capped = f.autonomy.reserve(&second, &evidence(merge), &f.settings);
        let Err(Refusal::Capped(violations)) = capped else {
            panic!("the last auto-merge is taken: {capped:?}");
        };
        assert_eq!(violations[0].limit, HardLimit::AutoMergeCap);

        // Your merge beat auto's: the auto-merge and the spot check go back.
        f.autonomy
            .user_decided(&decided(&first, AnswerAction::Merge, false), &f.settings);
        assert_eq!(f.autonomy.view(&f.settings).spot_checks_waiting, 0);
        assert_eq!(
            f.autonomy.reserve(&first, &evidence(merge), &f.settings),
            Err(Refusal::AlreadyDecided)
        );
        assert_eq!(
            f.autonomy.reserve(&second, &evidence(merge), &f.settings),
            Ok(Reservation { spot_check: true })
        );

        // Auto's approval was scored as a shadow record against your merge.
        f.clock.advance(days(7));
        assert_eq!(row(&f, merge).matches_in_a_row, 1);

        // The day's spend ceiling refuses any auto-approval.
        f.settings.limits.spend_per_day = Some(5.0);
        let mut costly = evidence(CheckpointKind::Plan);
        costly.facts.spent.cost_usd = 6.0;
        let plan = point("run-c", 1, CheckpointKind::Plan);
        let ceiling = f.autonomy.reserve(&plan, &costly, &f.settings);
        let Err(Refusal::Capped(violations)) = ceiling else {
            panic!("the spend ceiling is reached: {ceiling:?}");
        };
        assert_eq!(violations[0].limit, HardLimit::Ceiling);
    }

    #[test]
    fn score_once_in_either_order() {
        let f = fixture();
        let plan = CheckpointKind::Plan;
        let approve = would(Verdict::Approve);

        // The shadow from evaluate first, then your answer carrying it too.
        let a = point("run-a", 1, plan);
        f.autonomy
            .note_shadow(&a, &approve, &evidence(a.kind), &f.settings);
        f.autonomy
            .note_shadow(&a, &approve, &evidence(a.kind), &f.settings);
        f.autonomy
            .user_decided(&decided(&a, AnswerAction::Approve, true), &f.settings);
        f.autonomy
            .user_decided(&decided(&a, AnswerAction::Approve, true), &f.settings);
        assert_eq!(row(&f, plan).matches_in_a_row, 1);

        // Your answer first, with no shadow yet; evaluate's arrives after.
        let b = point("run-b", 1, plan);
        f.autonomy
            .user_decided(&decided(&b, AnswerAction::Approve, false), &f.settings);
        assert_eq!(row(&f, plan).matches_in_a_row, 1);
        f.autonomy
            .note_shadow(&b, &approve, &evidence(b.kind), &f.settings);
        f.autonomy
            .note_shadow(&b, &approve, &evidence(b.kind), &f.settings);
        assert_eq!(row(&f, plan).matches_in_a_row, 2);

        // Your answer first, carrying the shadow; evaluate's is then ignored.
        let c = point("run-c", 1, plan);
        f.autonomy
            .user_decided(&decided(&c, AnswerAction::Approve, true), &f.settings);
        f.autonomy
            .note_shadow(&c, &approve, &evidence(c.kind), &f.settings);
        assert_eq!(row(&f, plan).matches_in_a_row, 3);
    }

    #[test]
    fn waiting_spot_checks_pause_auto() {
        let mut f = fixture();
        f.settings.approvals.spot_check_sample = 1.0;
        f.settings.approvals.pause_auto_when_spot_checks_waiting = 2;
        let goal = CheckpointKind::Goal;
        let first = point("run-a", 1, goal);
        let second = point("run-a", 2, goal);
        for waiting in [&first, &second] {
            assert_eq!(f.autonomy.auto_paused(&f.settings), None);
            let reserved = f.autonomy.reserve(waiting, &evidence(goal), &f.settings);
            assert_eq!(reserved, Ok(Reservation { spot_check: true }));
        }
        assert!(f.autonomy.auto_paused(&f.settings).is_some());
        f.settings.approvals.pause_auto_when_spot_checks_waiting = 0;
        assert_eq!(f.autonomy.auto_paused(&f.settings), None);

        f.settings.approvals.pause_auto_when_spot_checks_waiting = 2;
        f.autonomy.spot_checked(&first, true, &f.settings);
        assert_eq!(f.autonomy.auto_paused(&f.settings), None);
        assert_eq!(f.autonomy.view(&f.settings).spot_checks_waiting, 1);
    }

    #[test]
    fn unreadable_records_fail_closed() {
        let f = fixture();
        let records = f.autonomy.records();
        fs::write(records.path(), "not a record\n").expect("write");
        let goal = point("run-a", 1, CheckpointKind::Goal);
        assert_eq!(
            f.autonomy.effective_mode(&goal, ApprovalMode::Auto),
            ApprovalMode::Supervised
        );
        assert!(f.autonomy.auto_paused(&f.settings).is_some());
        let refused = f
            .autonomy
            .reserve(&goal, &evidence(CheckpointKind::Goal), &f.settings);
        assert!(matches!(refused, Err(Refusal::Unavailable(_))));

        // A record that can't be written leaves this process supervised.
        f.autonomy.spot_checked(&goal, false, &f.settings);
        fs::remove_file(records.path()).expect("remove");
        assert_eq!(
            f.autonomy.effective_mode(&goal, ApprovalMode::Auto),
            ApprovalMode::Supervised
        );
        assert!(f.autonomy.auto_paused(&f.settings).is_some());
    }

    #[test]
    fn the_install_salt_is_created_beside_the_repos() {
        let home = tempfile::tempdir().expect("temp dir");
        let state_dir = home.path().join("state").join("repo-1234");
        fs::create_dir_all(&state_dir).expect("state dir");
        let autonomy = StateAutonomy::open(&state_dir, Arc::new(FakeClock::default()));
        let merge = point("run-a", 1, CheckpointKind::Merge);
        let settings = Settings::default();
        let reserved = autonomy.reserve(&merge, &evidence(CheckpointKind::Merge), &settings);
        assert!(reserved.is_ok(), "{reserved:?}");
        let salt = home.path().join("state").join(spot_check::SALT_FILE);
        assert_eq!(fs::read(salt).expect("salt").len(), 32);
        assert!(home.path().join(user_files::DAILY).exists());
    }
}
