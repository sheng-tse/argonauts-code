//! The header and bottom line's data, with what the bottom line draws from: each CLI's plan
//! windows, `mods/bottom-line.toml`, and the facts of the parts that file puts on the line.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use argonauts_core::RunId;
use argonauts_core::Spend;
use argonauts_core::app::{HeaderAndBottomLine, LineFacts, PlanUsageChip, UltraColour};
use argonauts_core::limits::{HealthBoard, PlanUsageBoard, PlanUsageReading};
use argonauts_core::names::{AgentId, ModelRef};
use argonauts_core::plan_usage::{LiveUsage, UNNAMED_SHOWN_FOR, current_live};
use argonauts_core::registry::effort_to_run;
use argonauts_core::registry::listed::nearest_listed;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::bottom_line::{BRANCH, BottomLineSettings, SPEND};
use argonauts_core::settings::limit::LimitAmount;
use time::OffsetDateTime;

use super::Ctx;
use super::names::cli_defaults;
use super::turn::conversation_agent;
use crate::env::AppEnv;
use crate::usage_ledger::{UsageLedger, by_day, local_day};

/// What the attached run adds to the header and bottom line.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RunStatusParts {
    pub run: Option<RunId>,
    pub spent: Option<Spend>,
    pub limit: Option<LimitAmount>,
    /// The attached run's mode, else Shift+Tab's for this app session. `None` leaves it to
    /// the settings, which every new run starts from.
    pub approval_mode: Option<ApprovalMode>,
}

pub(crate) fn header_and_bottom_line(
    ctx: &Ctx,
    title: &str,
    run: RunStatusParts,
) -> HeaderAndBottomLine {
    let settings = ctx.settings();
    static ASKED: Mutex<BTreeMap<AgentId, OffsetDateTime>> = Mutex::new(BTreeMap::new());
    read_stale(
        &ctx.services.plan_usage,
        &settings.crew.members(),
        OffsetDateTime::now_utc(),
        &ASKED,
    );
    let (main_agent, main_effort) = header_agent(ctx);
    let ultra = ultra_look(
        &main_agent.agent,
        settings.ultracode,
        main_effort.as_deref(),
    );
    let mut line = line_facts(&ctx.env, OffsetDateTime::now_utc());
    line.main_effort = main_effort;
    HeaderAndBottomLine {
        title: title.to_owned(),
        repo: ctx.env.display_repo(),
        workflow: settings.workflow.clone(),
        ultracode: settings.ultracode,
        ultra,
        run: run.run,
        main_agent,
        plan_chips: plan_usage_chips(
            &settings.crew.members(),
            ctx.services.plan_usage.as_ref(),
            ctx.services.health.as_ref(),
        ),
        line: Box::new(line),
        spent: run.spent,
        limit: run.limit,
        narrator: settings.ui.narrator,
        approval_mode: run
            .approval_mode
            .unwrap_or_else(|| settings.approvals.mode()),
        awake: ctx.awake.held(),
    }
}

/// The look of the main agent's own ultra mode while it is on, which only a CLI that has one
/// declares: Claude Code's ultracode, Codex's ultra effort.
fn ultra_look(agent: &str, ultracode: bool, effort: Option<&str>) -> Option<UltraColour> {
    crate::adapters::builtin_adapters()
        .into_iter()
        .find(|adapter| adapter.id() == agent)?
        .ultra_mode()?
        .colour_when(ultracode, effort)
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
        // The caller that knows the main session fills this in.
        main_effort: None,
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

/// The conversation's agent as the header names it. The bottom line carries the same, so it
/// names an agent only once you change it. With no model set the CLI runs its own default: the one
/// its model list marks, else the one the catalog's "Default (recommended)" runs. With no effort
/// set it runs the level that model starts at, as the CLI says, else the catalog. A model fixed at
/// one level, or taking none, runs as it does whatever is set. A model its CLI no longer lists
/// runs as the nearest one it does, so that is the one named.
pub(crate) fn header_agent(ctx: &Ctx) -> (ModelRef, Option<String>) {
    let settings = ctx.settings();
    let (mut agent, effort) = conversation_agent(settings, &settings.crew.main);
    let registry = &ctx.services.registry;
    let catalog = registry.catalog();
    if agent.model.is_none() {
        agent.model = cli_defaults(ctx, [agent.agent.as_str()]).remove(&agent.agent);
    }
    // The model that runs sets the level it takes, so the swap comes first, as a turn's does.
    if let Some(nearest) = nearest_listed(registry.as_ref(), &agent) {
        agent = nearest;
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
/// the next header and bottom line.
fn read_stale(
    plan_usage: &Arc<dyn PlanUsageBoard>,
    crew: &[ModelRef],
    now: OffsetDateTime,
    asked: &Mutex<BTreeMap<AgentId, OffsetDateTime>>,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
    for worker in stale(plan_usage.as_ref(), crew, now) {
        let agent_id = worker.agent_id();
        let every = again_after(plan_usage.live(&agent_id).as_ref());
        let fresh = |at: &OffsetDateTime| now - *at < every;
        if asked.get(&agent_id).is_some_and(fresh) {
            continue;
        }
        asked.insert(agent_id.clone(), now);
        let plan_usage = Arc::clone(plan_usage);
        runtime.spawn(async move {
            plan_usage.probe(&agent_id).await;
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
    let plan_usage = Arc::clone(&ctx.services.plan_usage);
    let crew = ctx.settings().crew.members();
    let agent_ids: Vec<AgentId> = stale(plan_usage.as_ref(), &crew, OffsetDateTime::now_utc())
        .into_iter()
        .filter(|worker| ctx.services.workers.get(worker).is_ok())
        .map(|worker| worker.agent_id())
        .collect();
    if agent_ids.is_empty() {
        return;
    }
    let home = ctx.env.paths.clone();
    runtime.spawn(async move {
        let every = REREAD_EVERY.unsigned_abs();
        argonauts_conductor::plan_usage::probe_at_start(
            plan_usage.as_ref(),
            &home,
            &agent_ids,
            every,
        )
        .await;
    });
}

/// The crew's CLIs, one per agent, whose newest reading is older than [`REREAD_EVERY`]. One
/// never read isn't stale: it's left until its first session.
fn stale(plan_usage: &dyn PlanUsageBoard, crew: &[ModelRef], now: OffsetDateTime) -> Vec<ModelRef> {
    let mut seen: Vec<AgentId> = Vec::new();
    let mut stale = Vec::new();
    for worker in crew {
        let agent_id = worker.agent_id();
        if seen.contains(&agent_id) {
            continue;
        }
        seen.push(agent_id.clone());
        // A CLI read only from a session's stream has no live reading yet, so it's due one.
        let live = plan_usage.live(&agent_id);
        let read_at_all =
            plan_usage.get(&agent_id).is_some() || !plan_usage.windows(&agent_id).is_empty();
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

/// One chip per agent, in `crew` order, so two models of one agent share a chip.
pub(crate) fn plan_usage_chips(
    crew: &[ModelRef],
    plan_usage: &dyn PlanUsageBoard,
    health: &dyn HealthBoard,
) -> Vec<PlanUsageChip> {
    let mut seen: Vec<AgentId> = Vec::new();
    let mut chips = Vec::new();
    for worker in crew {
        let agent_id = worker.agent_id();
        if seen.contains(&agent_id) {
            continue;
        }
        // The CLI's own live reading of the account signed in now, as the Usage tab shows it.
        let now = OffsetDateTime::now_utc();
        let live = current_live(plan_usage, &agent_id, now);
        let windows: Vec<PlanUsageReading> = live
            .iter()
            .flat_map(|live| live.plan_usage_readings())
            .filter(|reading| reading.resets_at.is_none_or(|resets| resets > now))
            .collect();
        chips.push(PlanUsageChip {
            label: agent_label(&agent_id),
            reading: argonauts_core::limits::binding_reading(&windows).cloned(),
            health: health.state(&agent_id).state,
            windows,
        });
        seen.push(agent_id);
    }
    chips
}

/// The short name the bottom line shows, such as `Claude` for `claude-code`.
pub(crate) fn agent_label(agent_id: &AgentId) -> String {
    match agent_id.0.as_str() {
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

    #[test]
    fn only_claude_codes_ultracode_and_codexs_ultra_effort_take_an_ultra_look() {
        let purple = Some(UltraColour::Purple);
        assert_eq!(ultra_look("claude-code", true, Some("xhigh")), purple);
        assert_eq!(ultra_look("claude-code", false, Some("ultra")), None);
        assert_eq!(ultra_look("codex", true, Some("xhigh")), None);
        assert_eq!(
            ultra_look("codex", false, Some("ultra")),
            Some(UltraColour::Blue)
        );
        // A top effort or a model named Ultra elsewhere keeps the normal look.
        assert_eq!(ultra_look("antigravity-cli", true, Some("ultra")), None);
        assert_eq!(ultra_look("opencode", true, Some("max")), None);
        assert_eq!(ultra_look("ollama", true, Some("ultra")), None);
    }

    /// A board that counts what it was asked to read again.
    #[derive(Default)]
    struct Counting {
        board: MemoryPlanUsageBoard,
        probed: Mutex<Vec<AgentId>>,
    }

    impl PlanUsageBoard for Counting {
        fn observe(&self, reading: PlanUsageReading) {
            self.board.observe(reading);
        }
        fn get(&self, agent_id: &AgentId) -> Option<PlanUsageReading> {
            self.board.get(agent_id)
        }
        fn all(&self) -> Vec<PlanUsageReading> {
            self.board.all()
        }
        fn windows(&self, agent_id: &AgentId) -> Vec<PlanUsageReading> {
            self.board.windows(agent_id)
        }
        fn plan(&self, agent_id: &AgentId) -> Option<String> {
            self.board.plan(agent_id)
        }
        fn observe_plan(&self, agent_id: &AgentId, plan: String) {
            self.board.observe_plan(agent_id, plan);
        }
        fn observe_live(&self, reading: argonauts_core::plan_usage::LiveUsage) {
            self.board.observe_live(reading);
        }
        fn live(&self, agent_id: &AgentId) -> Option<argonauts_core::plan_usage::LiveUsage> {
            self.board.live(agent_id)
        }
        fn probe(
            &self,
            agent_id: &AgentId,
        ) -> argonauts_core::future::BoxFuture<'_, Option<PlanUsageReading>> {
            self.probed.lock().unwrap().push(agent_id.clone());
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
        let live = |agent_id: &str, read_at: OffsetDateTime| {
            let mut live = argonauts_core::plan_usage::LiveUsage::new(
                AgentId(agent_id.to_owned()),
                argonauts_core::limits::PlanUsageSource::AppServer,
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
        let board: Arc<dyn PlanUsageBoard> = counting.clone();
        let crew: Vec<ModelRef> = [
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
            [AgentId("codex".to_owned()), AgentId("opencode".to_owned())]
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
            AgentId("antigravity-cli".to_owned()),
            argonauts_core::limits::PlanUsageSource::AppServer,
            now - time::Duration::minutes(9),
        );
        board.observe_live(unnamed);
        let crew: Vec<ModelRef> = vec!["antigravity-cli".parse().unwrap()];
        assert_eq!(stale(&board, &crew, now).len(), 1);
        assert!(stale(&board, &crew, now - time::Duration::minutes(2)).is_empty());
    }

    /// A read that fails after a reading, or a sign-in since, means the account may have
    /// changed: the chip drops the reading kept from before.
    #[test]
    fn a_doubted_reading_leaves_the_chip() {
        struct Doubted(MemoryPlanUsageBoard);
        impl PlanUsageBoard for Doubted {
            fn observe(&self, reading: PlanUsageReading) {
                self.0.observe(reading);
            }
            fn get(&self, agent_id: &AgentId) -> Option<PlanUsageReading> {
                self.0.get(agent_id)
            }
            fn all(&self) -> Vec<PlanUsageReading> {
                self.0.all()
            }
            fn observe_live(&self, reading: argonauts_core::plan_usage::LiveUsage) {
                self.0.observe_live(reading);
            }
            fn live(&self, agent_id: &AgentId) -> Option<argonauts_core::plan_usage::LiveUsage> {
                self.0.live(agent_id)
            }
            fn doubted_at(&self, _: &AgentId) -> Option<OffsetDateTime> {
                Some(OffsetDateTime::now_utc())
            }
            fn probe(
                &self,
                _: &AgentId,
            ) -> argonauts_core::future::BoxFuture<'_, Option<PlanUsageReading>> {
                Box::pin(async { None })
            }
        }
        let board = Doubted(MemoryPlanUsageBoard::default());
        let now = OffsetDateTime::now_utc();
        let mut named = argonauts_core::plan_usage::LiveUsage::new(
            AgentId("codex".to_owned()),
            PlanUsageSource::AppServer,
            now - time::Duration::minutes(3),
        );
        named.account = Some("one@example.com".to_owned());
        named.windows = vec![
            argonauts_core::plan_usage::LiveWindow::new("five_hour", "session", "Current session")
                .used(Some(0.95))
                .resets_at(Some(now + time::Duration::hours(1))),
        ];
        board.observe_live(named);
        let crew: Vec<ModelRef> = vec!["codex".parse().unwrap()];
        let chips = plan_usage_chips(&crew, &board, &MemoryHealthBoard::default());
        assert!(chips[0].reading.is_none(), "{:?}", chips[0].reading);
        assert!(chips[0].windows.is_empty());
    }
    use argonauts_core::limits::{HealthState, PlanUsageReading, PlanUsageSource, PlanUsageStatus};
    use argonauts_testkit::TempRepo;
    use argonauts_testkit::services::{MemoryHealthBoard, MemoryPlanUsageBoard};
    use time::OffsetDateTime;

    use crate::controller::test_support::{CliOptions, TestCtx};

    fn reading(agent_id: &str, used: f64) -> PlanUsageReading {
        PlanUsageReading {
            agent_id: AgentId(agent_id.to_owned()),
            status: PlanUsageStatus::Allowed,
            used: Some(used),
            resets_at: None,
            window: Some("five_hour".to_owned()),
            source: PlanUsageSource::RateLimitEvent,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            detail: None,
            wrap_up: false,
        }
    }

    /// The bottom line carries the agent the header names, so it names none until you change
    /// it: the model your settings give, or the one the CLI runs by default.
    #[test]
    fn the_bottom_line_names_the_agent_the_header_does() {
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(
                    repo.global_config(),
                    "crew.main = \"codex\"\n\n[agents.codex]\nmodel = \"gpt-5.6-luna\"\n",
                )
                .unwrap();
            })),
            ..CliOptions::default()
        });
        let status = header_and_bottom_line(&parts.ctx, "", RunStatusParts::default());
        assert_eq!(status.main_agent.to_string(), "codex/gpt-5.6-luna");

        let (parts, _) = TestCtx::new(CliOptions::default());
        let status = header_and_bottom_line(&parts.ctx, "", RunStatusParts::default());
        assert!(status.main_agent.model.is_some(), "{}", status.main_agent);
        assert_eq!(status.main_agent, header_agent(&parts.ctx).0);
    }

    /// The bottom line's settings come from `mods/bottom-line.toml`, yours under the
    /// repository's, and a part costs nothing until one of them puts it on the line.
    #[test]
    fn the_bottom_lines_file_picks_its_parts_and_what_they_need() {
        let (parts, _) = TestCtx::new(CliOptions::default());
        let env = &parts.ctx.env;
        let now = OffsetDateTime::now_utc();
        let shipped = line_facts(env, now);
        assert_eq!(shipped.settings, BottomLineSettings::default());
        assert_eq!((shipped.branch, shipped.spend_days.len()), (None, 0));

        let yesterday = now - time::Duration::days(1);
        let ledger = UsageLedger::new(&env.paths);
        let worker: ModelRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let cost = |cost_usd| Spend {
            cost_usd,
            output_tokens: 10,
            ..Spend::default()
        };
        let turn = crate::usage_ledger::TurnNote::default();
        ledger.turn_ended(&env.repo_root, &worker, &cost(0.42), turn, yesterday);
        ledger.turn_ended(&env.repo_root, &worker, &cost(1.44), turn, now);

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
        // The header's line is the same, with the main session's effort for the working line.
        let shown = header_and_bottom_line(&parts.ctx, "", RunStatusParts::default()).line;
        assert_eq!(shown.main_effort, header_agent(&parts.ctx).1);
        assert_eq!(
            LineFacts {
                main_effort: None,
                ..*shown
            },
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
    fn one_chip_per_agent_in_crew_order() {
        let plan_usage = MemoryPlanUsageBoard::default();
        let health = MemoryHealthBoard::default();
        // A stream reading names no account, so the line waits for the live one.
        plan_usage.observe(reading("codex", 0.5));
        let mut live = argonauts_core::plan_usage::LiveUsage::new(
            AgentId("codex".to_owned()),
            argonauts_core::limits::PlanUsageSource::AppServer,
            OffsetDateTime::now_utc(),
        );
        live.account = Some("me@example.com".to_owned());
        live.windows = vec![
            argonauts_core::plan_usage::LiveWindow::new("five_hour", "5-hour", "5h limit")
                .used(Some(0.38)),
        ];
        plan_usage.observe_live(live);
        let crew: Vec<ModelRef> = ["codex/gpt-5.6", "claude-code", "codex/gpt-5.6-luna"]
            .iter()
            .map(|worker| worker.parse().unwrap())
            .collect();
        health.set(
            &AgentId("claude-code".to_owned()),
            HealthState::Down,
            Some("overloaded"),
        );
        let chips = plan_usage_chips(&crew, &plan_usage, &health);
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
