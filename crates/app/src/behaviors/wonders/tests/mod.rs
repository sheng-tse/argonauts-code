mod crew;
mod log;
mod view;

use std::collections::BTreeMap;
use std::fs;

use argonauts_core::Paths;
use argonauts_core::base::names::RoleName;
use argonauts_core::behaviors::behavior_world::Wonder;
use argonauts_core::behaviors::{Behavior, Label};
use argonauts_core::models::model_catalog::ModelCatalog;
use argonauts_core::runs::events::council::MessageKind;
use argonauts_core::runs::events::run::{ResumeCause, RunOutcome};
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::runs::events::task::{CheckKind, TryCause};
use argonauts_core::workflows::steps::judgment::ReviewVerdict;
use time::macros::{datetime, offset};
use time::{OffsetDateTime, UtcOffset};

use super::rules::{self, Found, Records};
use super::store::{KeptRun, Store};
use super::tally::{TALLY_RULES, tally};
use super::{Act, Sky, Zone};
use log::{Log, PROJECT};

const OPUS: &str = "claude/claude-opus-5-5";
const HAIKU: &str = "claude/claude-haiku-5-5";
const LUNA: &str = "codex/gpt-6-luna";
const FLASH: &str = "agy/gemini-3.8-flash";
const QWEN: &str = "opencode/ollama/qwen3-coder:30b";
const KIMI: &str = "opencode/moonshot/kimi-k2.6";

/// When the made-up records are read.
const TODAY: OffsetDateTime = datetime!(2026-10-25 12:00 UTC);

fn monday() -> OffsetDateTime {
    datetime!(2026-09-14 10:00 UTC)
}

fn tallies(logs: &[&Log], zone: Zone) -> BTreeMap<String, KeptRun> {
    let catalog = ModelCatalog::default();
    logs.iter()
        .map(|log| {
            let run = KeptRun {
                rules: TALLY_RULES,
                log_bytes: 0,
                tally: tally(log.events(), &catalog, zone),
            };
            (format!("{PROJECT}/{}", log.run()), run)
        })
        .collect()
}

const UTC: Zone = Zone::Fixed(UtcOffset::UTC);

fn found_with(logs: &[&Log], acts: &[Act<'_>], zone: Zone) -> Vec<Found> {
    let tallies = tallies(logs, zone);
    let catalog = ModelCatalog::default();
    rules::found(&Records {
        tallies: &tallies,
        acts,
        now: TODAY,
        zone,
        catalog: &catalog,
    })
}

fn found_in_zone(logs: &[&Log], zone: Zone) -> Vec<Found> {
    found_with(logs, &[], zone)
}

fn found(logs: &[&Log]) -> Vec<Found> {
    found_in_zone(logs, UTC)
}

fn wonders(logs: &[&Log]) -> Vec<Wonder> {
    found(logs).into_iter().map(|found| found.wonder).collect()
}

fn the(wonder: Wonder, logs: &[&Log]) -> Found {
    found(logs)
        .into_iter()
        .find(|found| found.wonder == wonder)
        .unwrap_or_else(|| panic!("{} didn't rise", wonder.name()))
}

/// A run with models of three makers aboard, still going.
fn three_makers_aboard(run: &str) -> Log {
    let mut log = Log::new(run, monday());
    log.session(1, None, RoleName::Orchestrator, OPUS, "anthropic")
        .acts(1, 5)
        .session(2, Some("task-001"), RoleName::Worker, LUNA, "openai")
        .acts(2, 3)
        .session(3, Some("task-001"), RoleName::Reviewer, FLASH, "google")
        .acts(3, 1)
        .clean_task("task-001", LUNA);
    log
}

fn three_makers(run: &str, outcome: RunOutcome) -> Log {
    let mut log = three_makers_aboard(run);
    log.finished(outcome);
    log
}

#[test]
fn the_argo_rises_when_three_makers_merge_work_together() {
    let argo = three_makers("run-argo", RunOutcome::Merged);
    let found = the(Wonder::Argo, &[&argo]);
    assert_eq!(found.line, "3 makers aboard, merged");
    assert_eq!(found.models, [OPUS, LUNA, FLASH]);
    assert_eq!(found.at, argo.at());

    let failed = three_makers("run-failed", RunOutcome::Failed);
    assert!(!wonders(&[&failed]).contains(&Wonder::Argo));
    let offered = three_makers("run-offered", RunOutcome::PullRequest);
    assert!(!wonders(&[&offered]).contains(&Wonder::Argo));
    let mut two = Log::new("run-two", monday());
    two.session(1, None, RoleName::Orchestrator, OPUS, "anthropic")
        .session(2, None, RoleName::Worker, LUNA, "openai")
        .session(3, None, RoleName::Reviewer, KIMI, "unknown")
        .finished(RunOutcome::PullRequest);
    assert!(!wonders(&[&two]).contains(&Wonder::Argo));
}

#[test]
fn a_wonder_rises_from_the_first_run_it_holds_in() {
    let mut later = Log::new("run-later", monday() + time::Duration::days(3));
    later
        .session(1, None, RoleName::Orchestrator, OPUS, "anthropic")
        .session(2, None, RoleName::Worker, LUNA, "openai")
        .session(3, None, RoleName::Reviewer, FLASH, "google")
        .finished(RunOutcome::Merged);
    let first = three_makers("run-first", RunOutcome::Merged);
    assert_eq!(the(Wonder::Argo, &[&later, &first]).at, first.at());
}

/// A run of five tasks, each merged on its first try, whose work merges.
fn first_try_run(run: &str) -> Log {
    let mut log = Log::new(run, monday());
    log.session(1, None, RoleName::Worker, HAIKU, "anthropic");
    for n in 1..=5 {
        log.clean_task(&format!("task-00{n}"), HAIKU);
    }
    log.finished(RunOutcome::Merged);
    log
}

#[test]
fn the_golden_fleece_needs_five_tasks_each_merged_on_its_first_try() {
    let fleece = first_try_run("run-fleece");
    assert_eq!(
        the(Wonder::Fleece, &[&fleece]).line,
        "5 tasks in one run, each merged on its first try"
    );

    let mut stumbled = Log::new("run-stumbled", monday());
    for n in 1..=4 {
        stumbled.clean_task(&format!("task-00{n}"), HAIKU);
    }
    stumbled
        .task("task-005", HAIKU)
        .try_("task-005", 1, TryCause::First, HAIKU)
        .check("task-005", CheckKind::Command, false)
        .try_("task-005", 2, TryCause::CheckRetry, HAIKU)
        .check("task-005", CheckKind::Command, true)
        .merged("task-005")
        .finished(RunOutcome::Merged);
    assert!(!wonders(&[&stumbled]).contains(&Wonder::Fleece));

    let mut sent_back = Log::new("run-sent-back", monday());
    for n in 1..=5 {
        sent_back.clean_task(&format!("task-00{n}"), HAIKU);
    }
    sent_back
        .review("task-003", 2, LUNA, ReviewVerdict::RequestChanges)
        .finished(RunOutcome::Merged);
    assert!(!wonders(&[&sent_back]).contains(&Wonder::Fleece));

    let mut ruled_against = Log::new("run-ruled", monday());
    for n in 1..=5 {
        ruled_against.clean_task(&format!("task-00{n}"), HAIKU);
    }
    ruled_against
        .settle("task-002", OPUS, ReviewVerdict::RequestChanges)
        .finished(RunOutcome::Merged);
    assert!(!wonders(&[&ruled_against]).contains(&Wonder::Fleece));
}

/// A task whose check rounds fail as `rounds` says, `false` for a failing one, and then pass.
fn rocky(run: &str, rounds: &[bool]) -> Log {
    let task = "task-001";
    let mut log = Log::new(run, monday());
    log.session(1, Some(task), RoleName::Worker, LUNA, "openai")
        .task(task, LUNA);
    for (n, passes) in (1..).zip(rounds) {
        let cause = if n == 1 {
            TryCause::First
        } else {
            TryCause::CheckRetry
        };
        log.try_(task, n, cause, LUNA)
            .check(task, CheckKind::Command, *passes);
    }
    log.try_(task, rounds.len() as u32 + 1, TryCause::CheckRetry, LUNA)
        .check(task, CheckKind::Command, true)
        .merged(task);
    log
}

#[test]
fn the_clashing_rocks_need_five_failed_rounds_in_a_row_and_then_a_merge() {
    let rocks = rocky("run-rocks", &[false; 5]);
    let found = the(Wonder::Symplegades, &[&rocks]);
    assert_eq!(found.line, "checks failed 5 times in a row, then it merged");
    assert_eq!(found.models, [LUNA]);

    let broken = rocky("run-broken", &[false, false, false, true, false, false]);
    assert!(!wonders(&[&broken]).contains(&Wonder::Symplegades));

    let mut never_merged = Log::new("run-never", monday());
    never_merged.task("task-001", LUNA);
    for n in 1..=6 {
        never_merged
            .try_("task-001", n, TryCause::CheckRetry, LUNA)
            .check("task-001", CheckKind::Command, false);
    }
    assert!(!wonders(&[&never_merged]).contains(&Wonder::Symplegades));
}

/// A task the test check catches weakening its tests, which merges with every test kept if
/// `kept`, or as it was.
fn caught_run(run: &str, kept: bool) -> Log {
    let task = "task-001";
    let mut log = Log::new(run, monday());
    log.task(task, HAIKU)
        .try_(task, 1, TryCause::First, HAIKU)
        .check(task, CheckKind::TestTamper, false);
    if kept {
        log.try_(task, 2, TryCause::CheckRetry, HAIKU)
            .check(task, CheckKind::TestTamper, true)
            .check(task, CheckKind::Command, true);
    }
    log.merged(task);
    log
}

#[test]
fn the_sirens_need_a_caught_test_and_a_merge_with_every_test_kept() {
    let found = the(Wonder::Sirens, &[&caught_run("run-sirens", true)]);
    assert_eq!(
        found.line,
        "the test check caught it, then the task merged with every test kept"
    );
    assert_eq!(found.models, [HAIKU]);

    let let_through = caught_run("run-let-through", false);
    assert!(!wonders(&[&let_through]).contains(&Wonder::Sirens));
}

/// A run that starts at 9 PM UTC and works through to 6 AM, with you at 1 AM if `you`.
fn night_run(run: &str, you: bool) -> Log {
    let mut log = Log::new(run, datetime!(2026-09-14 21:00 UTC));
    log.session(1, None, RoleName::Worker, OPUS, "anthropic");
    if you {
        log.works(1, 4).you().works(1, 5);
    } else {
        log.works(1, 9);
    }
    log.finished(RunOutcome::Merged);
    log
}

#[test]
fn the_sleepless_dragon_needs_six_hours_of_your_night_without_you() {
    let dragon = night_run("run-dragon", false);
    let found = the(Wonder::Dragon, &[&dragon]);
    assert_eq!(
        found.line,
        "worked 7 hours of your night without you, and merged"
    );
    assert_eq!(found.models, [OPUS]);

    let watched = night_run("run-watched", true);
    assert!(!wonders(&[&watched]).contains(&Wonder::Dragon));
    // Nine hours ahead of UTC, the same hours are your day.
    let tokyo = Zone::Fixed(offset!(+9));
    assert!(found_in_zone(&[&dragon], tokyo).is_empty());
}

#[test]
fn a_run_that_waits_or_falls_silent_is_not_working_through_the_night() {
    let mut silent = Log::new("run-silent", datetime!(2026-09-14 21:00 UTC));
    silent
        .session(1, None, RoleName::Worker, OPUS, "anthropic")
        .works(1, 2)
        .later(240)
        .works(1, 3)
        .finished(RunOutcome::Merged);
    assert!(!wonders(&[&silent]).contains(&Wonder::Dragon));
}

/// A task whose model usage limits take `limits` times, each time falling to the next model.
fn harpies(run: &str, limits: usize) -> Log {
    let task = "task-001";
    let chain = [
        (OPUS, "anthropic"),
        (LUNA, "openai"),
        (FLASH, "google"),
        (QWEN, "alibaba"),
    ];
    let mut log = Log::new(run, monday());
    log.task(task, OPUS);
    for (n, (worker, maker)) in (1..).zip(&chain[..limits]) {
        let (next, _) = chain[n as usize];
        log.session(n, Some(task), RoleName::Worker, worker, maker)
            .ended(n, task, EndReason::UsageLimit { resets_at: None });
        if n == 2 {
            log.try_(task, n + 1, TryCause::Fallback, next);
        } else {
            log.fell_back(task, next);
        }
    }
    log.merged(task);
    log
}

#[test]
fn the_harpies_need_three_usage_limits_on_one_task_and_a_merge() {
    let found = the(Wonder::Harpies, &[&harpies("run-harpies", 3)]);
    assert_eq!(
        found.line,
        "usage limits took its model 3 times, and it still merged"
    );
    assert_eq!(found.models, [OPUS, LUNA, FLASH, QWEN]);

    assert!(!wonders(&[&harpies("run-twice", 2)]).contains(&Wonder::Harpies));
}

/// A council of three makers' seats that agrees after `rounds` rounds, and merges its work.
fn council(run: &str, rounds: u32) -> Log {
    let mut log = Log::new(run, monday());
    log.seat_session(1, 1, OPUS, "anthropic")
        .seat_session(2, 2, LUNA, "openai")
        .seat_session(3, 3, FLASH, "google")
        .seated(&[OPUS, LUNA, FLASH]);
    for round in 0..rounds {
        let kind = if round == 0 {
            MessageKind::FirstAnswer
        } else {
            MessageKind::Talk
        };
        for seat in 1..=3 {
            log.seat_writes(seat, kind);
        }
    }
    log.seat_writes(1, MessageKind::Notes)
        .agreed()
        .finished(RunOutcome::Merged);
    log
}

#[test]
fn the_oak_of_dodona_needs_three_makers_two_rounds_and_agreement() {
    let dodona = council("run-council", 2);
    let found = the(Wonder::Dodona, &[&dodona]);
    assert_eq!(found.line, "a council of 3 makers agreed");
    assert_eq!(found.models, [OPUS, LUNA, FLASH]);

    assert!(!wonders(&[&council("run-hasty", 1)]).contains(&Wonder::Dodona));
}

/// A merged run of `sessions` agents of its own and `cli_agents` its first agent's CLI ran.
fn big_crew(run: &str, sessions: u32, cli_agents: u32) -> Log {
    let mut log = Log::new(run, monday());
    for n in 1..=sessions {
        log.session(n, None, RoleName::Worker, HAIKU, "anthropic")
            .acts(n, 1);
    }
    if cli_agents > 0 {
        log.cli_workflow(1, cli_agents, "claude-opus-5-5");
    }
    log.finished(RunOutcome::Merged);
    log
}

#[test]
fn the_colossus_needs_ten_agents_in_one_merged_workflow() {
    let colossus = the(Wonder::Colossus, &[&big_crew("run-big", 10, 0)]);
    assert_eq!(colossus.line, "10 agents in one workflow, merged");
    assert_eq!(colossus.models, [HAIKU]);
    let with_its_cli = the(Wonder::Colossus, &[&big_crew("run-cli", 2, 8)]);
    assert_eq!(with_its_cli.models, [OPUS, HAIKU]);

    assert!(!wonders(&[&big_crew("run-nine", 9, 0)]).contains(&Wonder::Colossus));
}

/// Work built on your own machine, reviewed by `reviewer`, and merged. The model runs on
/// Ollama by its route, or on a free plan if `billed`.
fn lighthouse(run: &str, reviewer: &str, maker: &str, billed: bool) -> Log {
    let task = "task-001";
    let mut log = Log::new(run, monday());
    if billed {
        log.local_session(1, task, KIMI, "moonshot");
    } else {
        log.session(1, Some(task), RoleName::Worker, QWEN, "alibaba");
    }
    log.session(2, Some(task), RoleName::Reviewer, reviewer, maker)
        .task(task, QWEN)
        .review(task, 2, reviewer, ReviewVerdict::Approve)
        .merged(task);
    log
}

#[test]
fn the_lighthouse_needs_local_work_merged_after_another_maker_reviewed_it() {
    let found = the(
        Wonder::Pharos,
        &[&lighthouse("run-route", OPUS, "anthropic", false)],
    );
    assert_eq!(found.models, [QWEN, OPUS]);
    let free = the(
        Wonder::Pharos,
        &[&lighthouse("run-free", LUNA, "openai", true)],
    );
    assert_eq!(free.models, [KIMI, LUNA]);

    let same_maker = lighthouse("run-same", "opencode/ollama/qwen3:8b", "alibaba", false);
    assert!(!wonders(&[&same_maker]).contains(&Wonder::Pharos));
}

/// A run that stops, is carried on by `cause` ten hours later, and merges.
fn temple(run: &str, cause: ResumeCause) -> Log {
    let mut log = Log::new(run, monday());
    log.session(1, None, RoleName::Worker, OPUS, "anthropic")
        .finished(RunOutcome::Stopped)
        .later(600)
        .resumed(cause)
        .finished(RunOutcome::Merged);
    log
}

#[test]
fn the_temple_of_artemis_needs_a_stopped_run_you_carried_on() {
    let found = the(Wonder::Artemis, &[&temple("run-temple", ResumeCause::User)]);
    assert_eq!(
        found.line,
        "stopped, then /resume carried it on and it merged"
    );
    assert!(!wonders(&[&temple("run-timer", ResumeCause::Timer)]).contains(&Wonder::Artemis));
}

#[test]
fn an_ordinary_run_raises_nothing() {
    let mut log = Log::new("run-ordinary", monday());
    log.session(1, None, RoleName::Main, OPUS, "anthropic")
        .acts(1, 12)
        .clean_task("task-001", OPUS)
        .finished(RunOutcome::Merged);
    assert!(found(&[&log]).is_empty());
}

/// An action of `worker`, such as `claude/opus`, at `at`, outside any task.
fn act(at: OffsetDateTime, behavior: Behavior, worker: &'static str) -> Act<'static> {
    let (cli, model) = worker.split_once('/').unwrap();
    Act {
        at,
        behavior,
        cli,
        model: Some(model),
        repo: PROJECT,
        run: "run-made-up",
        task: None,
        merged: false,
    }
}

#[test]
fn a_wonder_stands_by_the_behavior_its_records_are_about() {
    let workflow = Behavior::One(Label::Workflow);
    let read = Behavior::One(Label::ReadSource);
    let paired = Behavior::Two(Label::Workflow, Label::ReadSource);
    let acts = [
        (read, OPUS),
        (read, OPUS),
        (read, QWEN),
        (paired, OPUS),
        (paired, OPUS),
        (workflow, OPUS),
        (Behavior::Unlabelled, QWEN),
        (Behavior::Unlabelled, QWEN),
    ]
    .map(|(behavior, worker)| act(monday(), behavior, worker));
    let argo = rules::About::Labels(&[&[Label::Workflow], &[Label::Subagent]]);
    assert_eq!(argo.place(&acts), Some(workflow));
    let artemis = rules::About::Labels(&[&[Label::RunStatus]]);
    assert_eq!(artemis.place(&acts), Some(read));
    assert_eq!(
        rules::About::Model(QWEN.to_owned()).place(&acts),
        Some(read)
    );
    assert_eq!(rules::About::SecondBusiest.place(&acts), Some(workflow));
    assert_eq!(rules::About::Busiest.place(&acts), Some(read));
    assert_eq!(argo.place(&[]), None);
}

struct Home {
    _dir: tempfile::TempDir,
    paths: Paths,
    catalog: ModelCatalog,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        Self {
            _dir: dir,
            paths,
            catalog: ModelCatalog::default(),
        }
    }

    fn sky(&self) -> Sky<'_> {
        Sky {
            store: Store::of(&self.paths),
            catalog: &self.catalog,
            zone: Zone::Fixed(UtcOffset::UTC),
        }
    }

    fn write(&self, log: &Log) {
        log.write(self.paths.home());
    }

    fn store(&self) -> std::path::PathBuf {
        self.paths.home().join("behaviors").join("wonders.json")
    }

    fn run_dir(&self, run: &str) -> std::path::PathBuf {
        self.paths
            .home()
            .join("state")
            .join(PROJECT)
            .join("runs")
            .join(run)
    }
}

const ACTS: [Act<'static>; 2] = [
    Act {
        at: datetime!(2026-09-14 10:05 UTC),
        behavior: Behavior::One(Label::Workflow),
        cli: "claude",
        model: Some("claude-opus-5-5"),
        repo: PROJECT,
        run: "run-argo",
        task: None,
        merged: false,
    },
    Act {
        at: datetime!(2026-09-14 10:10 UTC),
        behavior: Behavior::One(Label::RunTests),
        cli: "codex",
        model: Some("gpt-6-luna"),
        repo: PROJECT,
        run: "run-argo",
        task: Some("task-001"),
        merged: true,
    },
];

#[test]
fn a_wonder_stays_when_the_run_it_rose_from_is_deleted() {
    let home = Home::new();
    let argo = three_makers("run-argo", RunOutcome::Merged);
    home.write(&argo);
    let risen = home.sky().risen(&ACTS, TODAY);
    assert_eq!(risen.len(), 1);
    assert_eq!(risen[0].wonder, Wonder::Argo);
    assert_eq!(risen[0].behavior, "wrote a workflow script");
    assert_eq!(risen[0].risen, argo.at());
    assert!(home.store().is_file());

    fs::remove_dir_all(home.run_dir("run-argo")).unwrap();
    assert_eq!(home.sky().risen(&ACTS, TODAY), risen);
    let kept = fs::read_to_string(home.store()).unwrap();
    assert!(!kept.contains("run-argo"), "{kept}");
}

#[test]
fn nothing_is_kept_before_anything_rises() {
    let home = Home::new();
    assert!(home.sky().risen(&ACTS, TODAY).is_empty());
    assert!(!home.store().exists());
}

#[test]
fn a_wonder_with_no_behavior_to_stand_by_waits_for_one() {
    let home = Home::new();
    home.write(&three_makers("run-argo", RunOutcome::Merged));
    assert!(home.sky().risen(&[], TODAY).is_empty());
    assert_eq!(home.sky().risen(&ACTS, TODAY).len(), 1);
}

#[test]
fn a_run_is_read_again_only_once_its_log_has_grown() {
    let home = Home::new();
    let mut argo = three_makers_aboard("run-argo");
    home.write(&argo);
    assert!(home.sky().risen(&ACTS, TODAY).is_empty());

    // A kept tally stands for its log while the log is as long as it was.
    let key = format!("{PROJECT}/run-argo");
    let doctor = |change: &dyn Fn(&mut serde_json::Value)| {
        let mut kept: serde_json::Value =
            serde_json::from_slice(&fs::read(home.store()).unwrap()).unwrap();
        change(&mut kept["runs"][&key]["tally"]);
        fs::write(home.store(), kept.to_string()).unwrap();
    };
    doctor(&|tally| {
        tally["tamper_caught"] = serde_json::json!({
            "times": 1, "models": [], "at": "2026-09-14T09:00:00Z"
        });
    });
    let risen: Vec<Wonder> = home
        .sky()
        .risen(&ACTS, TODAY)
        .iter()
        .map(|risen| risen.wonder)
        .collect();
    assert_eq!(risen, [Wonder::Sirens]);

    argo.finished(RunOutcome::Merged);
    home.write(&argo);
    let risen: Vec<Wonder> = home
        .sky()
        .risen(&ACTS, TODAY)
        .iter()
        .map(|risen| risen.wonder)
        .collect();
    assert_eq!(risen, [Wonder::Sirens, Wonder::Argo]);
    let kept: serde_json::Value = serde_json::from_slice(&fs::read(home.store()).unwrap()).unwrap();
    assert!(kept["runs"][&key]["tally"]["tamper_caught"].is_null());
}

#[test]
fn a_store_a_newer_app_wrote_is_read_and_never_written_over() {
    let home = Home::new();
    fs::create_dir_all(home.store().parent().unwrap()).unwrap();
    let newer = r#"{"version":99,"risen":[{"wonder":"pyramid","behavior":"read a source file","risen":"2026-09-01T10:00:00Z","why":"work merged on 100 different days"}],"later":true}"#;
    fs::write(home.store(), newer).unwrap();
    home.write(&three_makers("run-argo", RunOutcome::Merged));
    let risen = home.sky().risen(&ACTS, TODAY);
    assert_eq!(risen.len(), 2);
    assert_eq!(fs::read_to_string(home.store()).unwrap(), newer);
}

#[test]
fn a_store_that_cant_be_read_is_moved_aside() {
    let home = Home::new();
    fs::create_dir_all(home.store().parent().unwrap()).unwrap();
    fs::write(home.store(), "not the wonders").unwrap();
    home.write(&three_makers("run-argo", RunOutcome::Merged));
    assert_eq!(home.sky().risen(&ACTS, TODAY).len(), 1);
    let aside = home.store().with_extension("unreadable.json");
    assert_eq!(fs::read_to_string(aside).unwrap(), "not the wonders");
    assert!(
        fs::read_to_string(home.store())
            .unwrap()
            .contains("\"argo\"")
    );
}
