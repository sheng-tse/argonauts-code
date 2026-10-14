//! The header and status line's data, with what the bottom line draws from: each CLI's plan
//! windows, `mods/bottom-line.toml`, and the facts of the parts that file puts on the line.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use argonauts_core::RunId;
use argonauts_core::Spend;
use argonauts_core::app::{LineFacts, QuotaChip, StatusLine};
use argonauts_core::limits::{HealthBoard, QuotaBoard, QuotaReading};
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::plan_usage::{LiveUsage, UNNAMED_SHOWN_FOR, current_live};
use argonauts_core::registry::effort_to_run;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::bottom_line::{BRANCH, BottomLineSettings, SPEND};
use argonauts_core::settings::budget::BudgetAmount;
use time::OffsetDateTime;

use super::Ctx;
use super::names::cli_defaults;
use super::turn::chat_agent;
use crate::env::AppEnv;
use crate::usage_ledger::{UsageLedger, by_day, local_day};

/// What the attached run adds to the status line.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RunStatusParts {
    pub run: Option<RunId>,
    pub spent: Option<Spend>,
    pub limit: Option<BudgetAmount>,
    /// The attached run's mode, else Shift+Tab's for this app session. `None` leaves it to
    /// the settings, which every new run starts from.
    pub approval_mode: Option<ApprovalMode>,
}

pub(crate) fn status_line(ctx: &Ctx, title: &str, run: RunStatusParts) -> StatusLine {
    let settings = ctx.settings();
    static ASKED: Mutex<BTreeMap<VendorId, OffsetDateTime>> = Mutex::new(BTreeMap::new());
    read_stale(
        &ctx.services.quota,
        &settings.crew.members(),
        OffsetDateTime::now_utc(),
        &ASKED,
    );
    StatusLine {
        title: title.to_owned(),
        repo: ctx.env.display_repo(),
        workflow: settings.workflow.clone(),
        ultracode: settings.ultracode,
        run: run.run,
        main_agent: header_agent(ctx).0,
        quotas: quota_chips(
            &settings.crew.members(),
            ctx.services.quota.as_ref(),
            ctx.services.health.as_ref(),
        ),
        line: Box::new(line_facts(&ctx.env, OffsetDateTime::now_utc())),
        spent: run.spent,
        limit: run.limit,
        narrator: settings.ui.narrator,
        approval_mode: run
            .approval_mode
            .unwrap_or_else(|| settings.approvals.mode()),
        awake: ctx.awake.held(),
    }
}

/// The bottom line's settings, and the facts of the parts they put on it. A part that isn't
/// on the line costs nothing to leave out.
pub(crate) fn line_facts(env: &AppEnv, now: OffsetDateTime) -> LineFacts {
    let settings = line_settings(env);
    LineFacts {
        branch: settings
            .has(BRANCH)
            .then(|| branch(&env.repo_root))
            .flatten(),
        spend_days: if settings.has(SPEND) {
            spend_days(env, now)
        } else {
            Vec::new()
        },
        // The screen reads an artifact off the call that publishes it, so there's none to
        // tell it about here.
        artifact: None,
        settings,
    }
}

/// The two files a line's settings come from, and when each was last written.
type Stamp = Vec<(PathBuf, Option<SystemTime>)>;

/// `mods/bottom-line.toml`, yours under the repository's. The status is sent often, so the
/// files are read again only once one of them changed, which also picks up an edit made
/// outside the app.
fn line_settings(env: &AppEnv) -> BottomLineSettings {
    static READ: Mutex<Vec<(Stamp, BottomLineSettings)>> = Mutex::new(Vec::new());
    let home = env.paths.home();
    let files = [
        BottomLineSettings::global_file(home),
        BottomLineSettings::project_file(&env.repo_root),
    ];
    let stamp: Stamp = files
        .into_iter()
        .map(|file| {
            let written = fs::metadata(&file).and_then(|meta| meta.modified()).ok();
            (file, written)
        })
        .collect();
    let mut read = READ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, settings)) = read.iter().find(|(known, _)| *known == stamp) {
        return settings.clone();
    }
    let (settings, _) = BottomLineSettings::load(home, Some(&env.repo_root));
    // One entry per pair of files, the stale stamp replaced.
    read.retain(|(known, _)| {
        known
            .iter()
            .map(|(file, _)| file)
            .ne(stamp.iter().map(|(file, _)| file))
    });
    read.truncate(KEPT_READS - 1);
    read.insert(0, (stamp, settings.clone()));
    settings
}

/// How many repositories' settings stay read. An app has one; a test run has many.
const KEPT_READS: usize = 8;

/// The branch checked out in `repo`, read from `.git/HEAD` without starting git. A detached
/// HEAD reads as its commit's first seven characters.
fn branch(repo: &Path) -> Option<String> {
    let dot_git = repo.join(".git");
    // A linked worktree's `.git` is a file that names its own folder.
    let git_dir = match fs::read_to_string(&dot_git) {
        Ok(pointer) => repo.join(pointer.trim().strip_prefix("gitdir:")?.trim()),
        Err(_) => dot_git,
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: refs/heads/") {
        Some(branch) if !branch.is_empty() => Some(branch.to_owned()),
        _ => head.get(..7).map(str::to_owned),
    }
}

/// How many days the spend part's sparkline covers.
const SPEND_DAYS: i64 = 7;

/// Each ledger folder's last reading, and when it was taken.
type LedgerReads = HashMap<PathBuf, (Instant, Vec<f64>)>;

/// How long a reading of the ledger stands in for the next one.
const SPEND_READ_EVERY: Duration = Duration::from_secs(30);

/// What each of the last seven days cost, oldest first and today last, from the usage ledger.
/// Empty when none of them cost anything. The ledger is read at most every half minute.
fn spend_days(env: &AppEnv, now: OffsetDateTime) -> Vec<f64> {
    static READ: Mutex<Option<LedgerReads>> = Mutex::new(None);
    let ledger_dir = env.paths.usage_dir();
    let mut read = READ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let read = read.get_or_insert_with(HashMap::new);
    if let Some((at, days)) = read.get(&ledger_dir)
        && at.elapsed() < SPEND_READ_EVERY
    {
        return days.clone();
    }
    let today = local_day(now);
    let first = today.saturating_sub(time::Duration::days(SPEND_DAYS - 1));
    let spent = by_day(&UsageLedger::new(&env.paths).read_since(Some(first)));
    let mut days: Vec<f64> = (0..SPEND_DAYS)
        .map(|back| today.saturating_sub(time::Duration::days(SPEND_DAYS - 1 - back)))
        .map(|day| spent.get(&day).map_or(0.0, |spend| spend.cost_usd))
        .collect();
    if days.iter().all(|cost| *cost <= 0.0) {
        days.clear();
    }
    read.insert(ledger_dir, (Instant::now(), days.clone()));
    days
}

/// The chat's agent as the header names it. The status line carries the same, so it names an
/// agent only once you change it. With no model set the CLI runs its own default: the one its
/// model list marks, else the one the catalog's "Default (recommended)" runs. With no effort
/// set it runs the level that model starts at, as the CLI says, else the catalog. A model fixed
/// at one level, or taking none, runs as it does whatever is set.
pub(crate) fn header_agent(ctx: &Ctx) -> (WorkerRef, Option<String>) {
    let settings = ctx.settings();
    let (mut agent, effort) = chat_agent(settings, &settings.main_agent);
    let registry = &ctx.services.registry;
    let catalog = registry.catalog();
    if agent.model.is_none() {
        agent.model = cli_defaults(ctx, [agent.worker.as_str()]).remove(&agent.worker);
    }
    let effort = effort_to_run(registry.as_ref(), &agent, effort)
        .or_else(|| registry.get(&agent)?.default_effort)
        .or_else(|| catalog.entry(&agent)?.default_effort.clone());
    (agent, effort)
}

/// How old a plan usage reading may get before the line asks for a fresh one. It stays under
/// the hour after which the line shows a reading's age, so only a CLI that can't be read for
/// free ever shows one.
const REREAD_EVERY: time::Duration = time::Duration::minutes(30);

/// Starts a fresh reading for each CLI of the crew whose newest one is older than
/// [`REREAD_EVERY`], at most once in that time. A CLI that can't be read for free answers
/// nothing, and one never read yet is left until it works. The answer lands on the board for
/// the next status line.
fn read_stale(
    quota: &Arc<dyn QuotaBoard>,
    crew: &[WorkerRef],
    now: OffsetDateTime,
    asked: &Mutex<BTreeMap<VendorId, OffsetDateTime>>,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
    for worker in stale(quota.as_ref(), crew, now) {
        let vendor = worker.vendor();
        let every = again_after(quota.live(&vendor).as_ref());
        let fresh = |at: &OffsetDateTime| now - *at < every;
        if asked.get(&vendor).is_some_and(fresh) {
            continue;
        }
        asked.insert(vendor.clone(), now);
        let quota = Arc::clone(quota);
        runtime.spawn(async move {
            quota.probe(&vendor).await;
        });
    }
}

/// Reads again, once at start, each CLI of the crew whose reading is older than
/// [`REREAD_EVERY`] and can be read for free. A CLI's probe is registered when its worker is
/// built, which nothing has done this early, so the stale ones are built first.
pub(crate) fn read_stale_at_start(ctx: &Ctx) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let quota = Arc::clone(&ctx.services.quota);
    let crew = ctx.settings().crew.members();
    let vendors: Vec<VendorId> = stale(quota.as_ref(), &crew, OffsetDateTime::now_utc())
        .into_iter()
        .filter(|worker| ctx.services.workers.get(worker).is_ok())
        .map(|worker| worker.vendor())
        .collect();
    if vendors.is_empty() {
        return;
    }
    let home = ctx.env.paths.clone();
    runtime.spawn(async move {
        let every = REREAD_EVERY.unsigned_abs();
        argonauts_conductor::quota::probe_at_start(quota.as_ref(), &home, &vendors, every).await;
    });
}

/// The crew's CLIs, one per vendor, whose newest reading is older than [`REREAD_EVERY`]. One
/// never read isn't stale: it's left until its first session.
fn stale(quota: &dyn QuotaBoard, crew: &[WorkerRef], now: OffsetDateTime) -> Vec<WorkerRef> {
    let mut seen: Vec<VendorId> = Vec::new();
    let mut stale = Vec::new();
    for worker in crew {
        let vendor = worker.vendor();
        if seen.contains(&vendor) {
            continue;
        }
        seen.push(vendor.clone());
        // A CLI read only from a session's stream has no live reading yet, so it's due one.
        let live = quota.live(&vendor);
        let read_at_all = quota.get(&vendor).is_some() || !quota.windows(&vendor).is_empty();
        let due = match &live {
            Some(live) => now - live.read_at >= again_after(Some(live)),
            None => read_at_all,
        };
        if due {
            stale.push(worker.clone());
        }
    }
    stale
}

/// How soon a CLI's live reading is read again. One that names no account shows only while
/// fresh, so it's read a minute before it would go.
fn again_after(live: Option<&LiveUsage>) -> time::Duration {
    match live {
        Some(live) if live.account.is_none() => UNNAMED_SHOWN_FOR - time::Duration::minutes(1),
        _ => REREAD_EVERY,
    }
}

/// One chip per vendor, in `crew` order, so two models of one vendor share a chip.
pub(crate) fn quota_chips(
    crew: &[WorkerRef],
    quota: &dyn QuotaBoard,
    health: &dyn HealthBoard,
) -> Vec<QuotaChip> {
    let mut seen: Vec<VendorId> = Vec::new();
    let mut chips = Vec::new();
    for worker in crew {
        let vendor = worker.vendor();
        if seen.contains(&vendor) {
            continue;
        }
        // The CLI's own live reading of the account signed in now, as the Usage tab shows it.
        let now = OffsetDateTime::now_utc();
        let live = current_live(quota, &vendor, now);
        let windows: Vec<QuotaReading> = live
            .iter()
            .flat_map(|live| live.quota_readings())
            .filter(|reading| reading.resets_at.is_none_or(|resets| resets > now))
            .collect();
        chips.push(QuotaChip {
            label: vendor_label(&vendor),
            reading: argonauts_core::limits::binding_reading(&windows).cloned(),
            health: health.state(&vendor).state,
            windows,
        });
        seen.push(vendor);
    }
    chips
}

/// The short name the status line shows, such as `Claude` for `claude-code`.
pub(crate) fn vendor_label(vendor: &VendorId) -> String {
    match vendor.0.as_str() {
        "claude-code" => "Claude".to_owned(),
        "codex" => "Codex".to_owned(),
        "antigravity-cli" => "agy".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board that counts what it was asked to read again.
    #[derive(Default)]
    struct Counting {
        board: MemoryQuotaBoard,
        probed: Mutex<Vec<VendorId>>,
    }

    impl QuotaBoard for Counting {
        fn observe(&self, reading: QuotaReading) {
            self.board.observe(reading);
        }
        fn get(&self, vendor: &VendorId) -> Option<QuotaReading> {
            self.board.get(vendor)
        }
        fn all(&self) -> Vec<QuotaReading> {
            self.board.all()
        }
        fn windows(&self, vendor: &VendorId) -> Vec<QuotaReading> {
            self.board.windows(vendor)
        }
        fn plan(&self, vendor: &VendorId) -> Option<String> {
            self.board.plan(vendor)
        }
        fn observe_plan(&self, vendor: &VendorId, plan: String) {
            self.board.observe_plan(vendor, plan);
        }
        fn observe_live(&self, reading: argonauts_core::plan_usage::LiveUsage) {
            self.board.observe_live(reading);
        }
        fn live(&self, vendor: &VendorId) -> Option<argonauts_core::plan_usage::LiveUsage> {
            self.board.live(vendor)
        }
        fn probe(
            &self,
            vendor: &VendorId,
        ) -> argonauts_core::future::BoxFuture<'_, Option<QuotaReading>> {
            self.probed.lock().unwrap().push(vendor.clone());
            Box::pin(async { None })
        }
    }

    /// A live reading old enough to show its age is read again where that's free, once in a
    /// while, and a fresh one isn't. A CLI read only from a session's stream has no live
    /// reading, so it's due one.
    #[tokio::test]
    async fn an_old_reading_is_read_again_and_a_fresh_one_is_left() {
        let counting = Arc::new(Counting::default());
        let now = OffsetDateTime::now_utc();
        let live = |vendor: &str, read_at: OffsetDateTime| {
            let mut live = argonauts_core::plan_usage::LiveUsage::new(
                VendorId(vendor.to_owned()),
                argonauts_core::limits::QuotaSource::AppServer,
                read_at,
            );
            live.account = Some("me@example.com".to_owned());
            live
        };
        counting.observe_live(live("codex", now - time::Duration::days(2)));
        counting.observe_live(live("antigravity-cli", now - time::Duration::minutes(5)));
        let mut streamed = reading("opencode", 0.2);
        streamed.observed_at = now - time::Duration::minutes(5);
        counting.observe(streamed);
        let board: Arc<dyn QuotaBoard> = counting.clone();
        let crew: Vec<WorkerRef> = [
            "codex/gpt-5.6",
            "codex",
            "antigravity-cli",
            "claude-code",
            "opencode",
        ]
        .iter()
        .map(|worker| worker.parse().unwrap())
        .collect();
        let asked = Mutex::new(BTreeMap::new());
        read_stale(&board, &crew, now, &asked);
        // Asked once a while, not on every line, and a CLI never read yet waits until it works.
        read_stale(&board, &crew, now + time::Duration::minutes(1), &asked);
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut probed = counting.probed.lock().unwrap().clone();
        probed.sort();
        assert_eq!(
            probed,
            [
                VendorId("codex".to_owned()),
                VendorId("opencode".to_owned())
            ]
        );
        // What start reads again: the same CLIs, each named once for all its models.
        let names: Vec<String> = stale(counting.as_ref(), &crew, now)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(names, ["codex/gpt-5.6", "opencode"]);
    }

    /// A reading that names no account shows for ten minutes, so it's read again in its tenth
    /// minute rather than after the half hour a named one waits.
    #[test]
    fn a_reading_that_names_no_account_is_read_again_before_it_fades() {
        let board = Counting::default();
        let now = OffsetDateTime::now_utc();
        let unnamed = argonauts_core::plan_usage::LiveUsage::new(
            VendorId("antigravity-cli".to_owned()),
            argonauts_core::limits::QuotaSource::AppServer,
            now - time::Duration::minutes(9),
        );
        board.observe_live(unnamed);
        let crew: Vec<WorkerRef> = vec!["antigravity-cli".parse().unwrap()];
        assert_eq!(stale(&board, &crew, now).len(), 1);
        assert!(stale(&board, &crew, now - time::Duration::minutes(2)).is_empty());
    }

    /// A read that fails after a reading, or a sign-in since, means the account may have
    /// changed: the chip drops the reading kept from before.
    #[test]
    fn a_doubted_reading_leaves_the_chip() {
        struct Doubted(MemoryQuotaBoard);
        impl QuotaBoard for Doubted {
            fn observe(&self, reading: QuotaReading) {
                self.0.observe(reading);
            }
            fn get(&self, vendor: &VendorId) -> Option<QuotaReading> {
                self.0.get(vendor)
            }
            fn all(&self) -> Vec<QuotaReading> {
                self.0.all()
            }
            fn observe_live(&self, reading: argonauts_core::plan_usage::LiveUsage) {
                self.0.observe_live(reading);
            }
            fn live(&self, vendor: &VendorId) -> Option<argonauts_core::plan_usage::LiveUsage> {
                self.0.live(vendor)
            }
            fn doubted_at(&self, _: &VendorId) -> Option<OffsetDateTime> {
                Some(OffsetDateTime::now_utc())
            }
            fn probe(
                &self,
                _: &VendorId,
            ) -> argonauts_core::future::BoxFuture<'_, Option<QuotaReading>> {
                Box::pin(async { None })
            }
        }
        let board = Doubted(MemoryQuotaBoard::default());
        let now = OffsetDateTime::now_utc();
        let mut named = argonauts_core::plan_usage::LiveUsage::new(
            VendorId("codex".to_owned()),
            QuotaSource::AppServer,
            now - time::Duration::minutes(3),
        );
        named.account = Some("one@example.com".to_owned());
        named.windows = vec![
            argonauts_core::plan_usage::LiveWindow::new("five_hour", "session", "Current session")
                .used(Some(0.95))
                .resets_at(Some(now + time::Duration::hours(1))),
        ];
        board.observe_live(named);
        let crew: Vec<WorkerRef> = vec!["codex".parse().unwrap()];
        let chips = quota_chips(&crew, &board, &MemoryHealthBoard::default());
        assert!(chips[0].reading.is_none(), "{:?}", chips[0].reading);
        assert!(chips[0].windows.is_empty());
    }
    use argonauts_core::limits::{HealthState, QuotaReading, QuotaSource, QuotaStatus};
    use argonauts_testkit::TempRepo;
    use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};
    use time::OffsetDateTime;

    use crate::controller::test_support::{HarnessOptions, TestCtx};

    fn reading(vendor: &str, used: f64) -> QuotaReading {
        QuotaReading {
            vendor: VendorId(vendor.to_owned()),
            status: QuotaStatus::Allowed,
            used: Some(used),
            resets_at: None,
            window: Some("five_hour".to_owned()),
            source: QuotaSource::RateLimitEvent,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            detail: None,
            wrap_up: false,
        }
    }

    /// The status line carries the agent the header names, so the mode line names none until
    /// you change it: the model your settings give, or the one the CLI runs by default.
    #[test]
    fn the_status_line_names_the_agent_the_header_does() {
        let (parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(
                    repo.global_config(),
                    "main_agent = \"codex\"\n\n[agents.codex]\nmodel = \"gpt-5.6-luna\"\n",
                )
                .unwrap();
            })),
            ..HarnessOptions::default()
        });
        let status = status_line(&parts.ctx, "", RunStatusParts::default());
        assert_eq!(status.main_agent.to_string(), "codex/gpt-5.6-luna");

        let (parts, _) = TestCtx::new(HarnessOptions::default());
        let status = status_line(&parts.ctx, "", RunStatusParts::default());
        assert!(status.main_agent.model.is_some(), "{}", status.main_agent);
        assert_eq!(status.main_agent, header_agent(&parts.ctx).0);
    }

    /// The bottom line's settings come from `mods/bottom-line.toml`, yours under the
    /// repository's, and a part costs nothing until one of them puts it on the line.
    #[test]
    fn the_bottom_lines_file_picks_its_parts_and_what_they_need() {
        let (parts, _) = TestCtx::new(HarnessOptions::default());
        let env = &parts.ctx.env;
        let now = OffsetDateTime::now_utc();
        let shipped = line_facts(env, now);
        assert_eq!(shipped.settings, BottomLineSettings::default());
        assert_eq!((shipped.branch, shipped.spend_days.len()), (None, 0));

        let yesterday = now - time::Duration::days(1);
        let ledger = UsageLedger::new(&env.paths);
        let worker: WorkerRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let cost = |cost_usd| Spend {
            cost_usd,
            output_tokens: 10,
            ..Spend::default()
        };
        ledger.turn_ended(&env.repo_root, &worker, &cost(0.42), yesterday);
        ledger.turn_ended(&env.repo_root, &worker, &cost(1.44), now);

        let yours = BottomLineSettings::global_file(env.paths.home());
        std::fs::create_dir_all(yours.parent().unwrap()).unwrap();
        std::fs::write(
            &yours,
            "left = [\"mode\", \"branch\"]\nform = \"compact\"\n",
        )
        .unwrap();
        let facts = line_facts(env, now);
        assert_eq!(facts.settings.left, ["mode", "branch"]);
        assert_eq!(facts.branch.as_deref(), Some("main"));
        assert!(facts.spend_days.is_empty(), "spend isn't on the line");

        // The repository's file lays its keys over yours, and is read as soon as it's there.
        let repos = BottomLineSettings::project_file(&env.repo_root);
        std::fs::create_dir_all(repos.parent().unwrap()).unwrap();
        std::fs::write(&repos, "right = [\"plan-usage\", \"spend\"]\n").unwrap();
        let facts = line_facts(env, now);
        assert_eq!(facts.settings.right, ["plan-usage", "spend"]);
        assert_eq!(facts.settings.left, ["mode", "branch"]);
        assert_eq!(
            facts.settings.form,
            argonauts_core::settings::bottom_line::LineForm::Compact
        );
        assert_eq!(facts.spend_days, [0.0, 0.0, 0.0, 0.0, 0.0, 0.42, 1.44]);
        assert_eq!(
            *status_line(&parts.ctx, "", RunStatusParts::default()).line,
            facts
        );
    }

    #[test]
    fn the_branch_reads_from_head_without_git() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        assert_eq!(branch(repo), None, "no repository, no branch");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feat/due-dates\n").unwrap();
        assert_eq!(branch(repo).as_deref(), Some("feat/due-dates"));
        std::fs::write(
            repo.join(".git/HEAD"),
            "4f2a9c1d0e7b6a5f4e3d2c1b0a9f8e7d6c5b4a39\n",
        )
        .unwrap();
        assert_eq!(branch(repo).as_deref(), Some("4f2a9c1"));

        // A linked worktree's `.git` is a file naming where its HEAD is.
        let linked = repo.join("linked");
        let its_git = repo.join(".git/worktrees/linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir_all(&its_git).unwrap();
        std::fs::write(its_git.join("HEAD"), "ref: refs/heads/r1-line\n").unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", its_git.display()),
        )
        .unwrap();
        assert_eq!(branch(&linked).as_deref(), Some("r1-line"));
    }

    #[test]
    fn one_chip_per_vendor_in_crew_order() {
        let quota = MemoryQuotaBoard::default();
        let health = MemoryHealthBoard::default();
        // A stream reading names no account, so the line waits for the live one.
        quota.observe(reading("codex", 0.5));
        let mut live = argonauts_core::plan_usage::LiveUsage::new(
            VendorId("codex".to_owned()),
            argonauts_core::limits::QuotaSource::AppServer,
            OffsetDateTime::now_utc(),
        );
        live.account = Some("me@example.com".to_owned());
        live.windows = vec![
            argonauts_core::plan_usage::LiveWindow::new("five_hour", "5-hour", "5h limit")
                .used(Some(0.38)),
        ];
        quota.observe_live(live);
        let crew: Vec<WorkerRef> = ["codex/gpt-5.6", "claude-code", "codex/gpt-5.6-luna"]
            .iter()
            .map(|worker| worker.parse().unwrap())
            .collect();
        health.set(
            &VendorId("claude-code".to_owned()),
            HealthState::Down,
            Some("overloaded"),
        );
        let chips = quota_chips(&crew, &quota, &health);
        let labels: Vec<&str> = chips.iter().map(|chip| chip.label.as_str()).collect();
        assert_eq!(labels, ["Codex", "Claude"]);
        assert_eq!(chips[0].reading.as_ref().and_then(|r| r.used), Some(0.38));
        // Every window the board has, so the CLI in use can show both.
        assert_eq!(
            chips[0].windows,
            chips[0].reading.iter().cloned().collect::<Vec<_>>()
        );
        assert_eq!(chips[1].reading, None);
        assert_eq!(chips[0].health, HealthState::Healthy);
        assert_eq!(chips[1].health, HealthState::Down);
    }
}
