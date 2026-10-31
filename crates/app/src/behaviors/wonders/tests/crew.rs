//! The wonders a crew raises over time, and the made-up crews that raise every one.

use argonauts_core::runs::events::task::TaskKind;
use time::Duration;

use super::log::task_id;
use super::*;

/// A task whose protected-path check stops it `stops` times, fixed on the next try unless
/// `breached`, and merged.
fn guarded(run: &str, stops: u32, breached: bool) -> Log {
    let task = "task-001";
    let mut log = Log::new(run, monday());
    log.task(task, LUNA);
    for n in 1..=stops {
        log.try_(task, n, TryCause::CheckRetry, LUNA)
            .check(task, CheckKind::ProtectedPaths, false);
    }
    if !breached {
        log.try_(task, stops + 1, TryCause::CheckRetry, LUNA).check(
            task,
            CheckKind::ProtectedPaths,
            true,
        );
    }
    log.merged(task);
    log
}

#[test]
fn talos_needs_ten_guard_stops_and_nothing_merged_past_them() {
    let (six, four) = (guarded("run-six", 6, false), guarded("run-four", 4, false));
    let found = the(Wonder::Talos, &[&six, &four]);
    assert_eq!(found.line, "stopped work 10 times, and nothing got past");
    assert_eq!(found.at, monday() + Duration::minutes(13));

    assert!(!wonders(&[&guarded("run-nine", 9, false)]).contains(&Wonder::Talos));
    let breached = guarded("run-breached", 1, true);
    assert!(!wonders(&[&six, &four, &breached]).contains(&Wonder::Talos));
}

fn merged_run(run: &str, at: OffsetDateTime) -> Log {
    let mut log = Log::new(run, at - Duration::minutes(1));
    log.finished(RunOutcome::Merged);
    log
}

/// Work merged every other day, `days` times from the start of December.
fn every_other_day(days: i64) -> Vec<Log> {
    let first = datetime!(2025-12-01 15:00 UTC);
    (0..days)
        .map(|day| {
            merged_run(
                &format!("run-day-{day:03}"),
                first + Duration::days(day * 2),
            )
        })
        .collect()
}

#[test]
fn the_great_pyramid_needs_work_merged_on_a_hundred_different_days() {
    let runs = every_other_day(100);
    let logs: Vec<&Log> = runs.iter().collect();
    let found = the(Wonder::Pyramid, &logs);
    assert_eq!(found.line, "work merged on 100 different days");
    assert_eq!(found.at, runs[99].at());
    assert!(!wonders(&logs[..99]).contains(&Wonder::Pyramid));

    // Late at night in UTC and just after midnight are one evening five hours behind it.
    let runs = every_other_day(98);
    let late = merged_run("run-late", datetime!(2026-07-01 23:30 UTC));
    let after = merged_run("run-after", datetime!(2026-07-02 00:30 UTC));
    let mut logs: Vec<&Log> = runs.iter().collect();
    logs.extend([&late, &after]);
    assert!(wonders(&logs).contains(&Wonder::Pyramid));
    let chicago = Zone::Fixed(offset!(-5));
    assert!(found_in_zone(&logs, chicago).is_empty());
}

/// Twenty merged tasks that added tests: fifteen whose own actions did, three test tasks and
/// two whose reviewer's acceptance tests merged. A merge checked again and failing, if `broken`.
fn gardens_run(broken: bool) -> (Log, Vec<Act<'static>>) {
    const TASKS: [&str; 15] = [
        "task-001", "task-002", "task-003", "task-004", "task-005", "task-006", "task-007",
        "task-008", "task-009", "task-010", "task-011", "task-012", "task-013", "task-014",
        "task-015",
    ];
    let mut log = Log::new("run-gardens", monday());
    let mut acts = Vec::new();
    for (n, task) in TASKS.iter().enumerate() {
        log.clean_task(task, HAIKU);
        let behavior = if n % 2 == 0 {
            Behavior::One(Label::EditTestAdded)
        } else {
            Behavior::One(Label::WriteTest)
        };
        acts.push(Act {
            run: "run-gardens",
            task: Some(task),
            merged: true,
            ..act(log.at(), behavior, HAIKU)
        });
    }
    for task in ["test-001", "test-002", "test-003"] {
        let kind = TaskKind::AcceptanceTests {
            for_task: task_id("task-001"),
        };
        log.task_of(task, LUNA, kind).merged(task);
    }
    for task in ["task-016", "task-017"] {
        log.clean_task(task, HAIKU).tests_written(task, LUNA, task);
    }
    if broken {
        log.check("task-004", CheckKind::AffectedTests, false);
    }
    (log, acts)
}

#[test]
fn the_hanging_gardens_need_tests_added_in_twenty_merged_tasks() {
    let (log, acts) = gardens_run(false);
    let found = found_with(&[&log], &acts, UTC)
        .into_iter()
        .find(|found| found.wonder == Wonder::Gardens)
        .unwrap();
    assert_eq!(found.line, "20 merged tasks added tests");

    let (broken, acts) = gardens_run(true);
    let found = found_with(&[&broken], &acts, UTC);
    assert!(!found.iter().any(|found| found.wonder == Wonder::Gardens));
    let unmerged: Vec<Act<'_>> = gardens_run(false)
        .1
        .into_iter()
        .map(|act| Act {
            merged: act.task != Some("task-009"),
            ..act
        })
        .collect();
    let found = found_with(&[&log], &unmerged, UTC);
    assert!(!found.iter().any(|found| found.wonder == Wonder::Gardens));
}

/// `settles` tasks, each a disagreement the judge settled by letting the work stand. The first
/// fails its checks after merging if `fails`.
fn judged(run: &str, settles: u32, fails: Option<bool>) -> Log {
    let mut log = Log::new(run, monday());
    for n in 1..=settles {
        let task = format!("task-{n:03}");
        log.task(&task, HAIKU)
            .try_(&task, 1, TryCause::First, HAIKU)
            .check(&task, CheckKind::Command, true)
            .review(&task, 2, LUNA, ReviewVerdict::RequestChanges)
            .settle(&task, OPUS, ReviewVerdict::Approve)
            .merged(&task);
    }
    match fails {
        Some(true) => log.check("task-001", CheckKind::AffectedTests, false),
        Some(false) => log.final_check(false),
        None => log.final_check(true),
    };
    log
}

#[test]
fn the_statue_of_zeus_needs_twenty_five_rulings_and_nothing_let_stand_failing() {
    let (first, rest) = (judged("run-first", 20, None), judged("run-rest", 5, None));
    let found = the(Wonder::Zeus, &[&first, &rest]);
    assert_eq!(
        found.line,
        "settled 25 disagreements, and nothing it let stand failed after merging"
    );
    assert_eq!(found.models, [OPUS]);

    assert!(!wonders(&[&judged("run-short", 24, None)]).contains(&Wonder::Zeus));
    let failed_after = judged("run-failed", 5, Some(true));
    assert!(!wonders(&[&first, &failed_after]).contains(&Wonder::Zeus));
    let suite_failed = judged("run-suite", 5, Some(false));
    assert!(!wonders(&[&first, &suite_failed]).contains(&Wonder::Zeus));
}

const HAIKU_OLDER: &str = "claude/claude-haiku-4-5";
const SONNET: &str = "claude/claude-sonnet-5-5";

/// `actions` actions of an older Haiku, the last `quiet` days ago, and the same work since by
/// `newer`.
fn retired(actions: i64, quiet: i64, newer: &'static str) -> Vec<Act<'static>> {
    let last = TODAY - Duration::days(quiet);
    let mut acts: Vec<Act<'static>> = (0..actions)
        .rev()
        .map(|n| {
            let behavior = if n % 3 == 0 {
                Behavior::One(Label::Shell)
            } else {
                Behavior::One(Label::EditSource)
            };
            act(last - Duration::minutes(n), behavior, HAIKU_OLDER)
        })
        .collect();
    for label in [Label::Shell, Label::EditSource] {
        acts.push(act(TODAY - Duration::days(2), Behavior::One(label), newer));
    }
    acts
}

#[test]
fn the_mausoleum_needs_a_busy_model_gone_quiet_and_its_successor() {
    let acts = retired(500, 61, HAIKU);
    let found = found_with(&[], &acts, UTC);
    let [mausoleum] = found.as_slice() else {
        panic!("{found:?}");
    };
    assert_eq!(mausoleum.wonder, Wonder::Mausoleum);
    assert_eq!(
        mausoleum.line,
        "rests here after 500 actions; Haiku 5.5 took its place"
    );
    assert_eq!(mausoleum.models, [HAIKU_OLDER, HAIKU]);
    assert_eq!(mausoleum.at, TODAY - Duration::days(1));
    assert_eq!(
        mausoleum.about.place(&acts),
        Some(Behavior::One(Label::EditSource))
    );

    for (actions, quiet, newer) in [(499, 61, HAIKU), (500, 59, HAIKU), (500, 61, SONNET)] {
        let acts = retired(actions, quiet, newer);
        assert!(
            found_with(&[], &acts, UTC).is_empty(),
            "{actions} {quiet} {newer}"
        );
    }
}

/// A crew that changed git state 130 times until `quiet` days ago and has done other things
/// since, the last of them `idle` days ago.
fn sunk(quiet: i64, idle: i64) -> Vec<Act<'static>> {
    let mut acts = Vec::new();
    let sank = TODAY - Duration::days(quiet);
    for n in (0..130).rev() {
        acts.push(act(
            sank - Duration::minutes(n),
            Behavior::One(Label::GitChange),
            OPUS,
        ));
    }
    let busy = TODAY - Duration::days(idle);
    let others = [
        (Label::ReadSource, 300),
        (Label::EditSource, 200),
        (Label::RunTests, 150),
        (Label::SearchCode, 100),
    ];
    for (label, count) in others {
        for n in 0..count {
            acts.push(act(busy - Duration::minutes(n), Behavior::One(label), LUNA));
        }
    }
    acts.sort_by_key(|act| act.at);
    acts
}

#[test]
fn atlantis_needs_a_most_used_behavior_quiet_for_thirty_days_while_the_crew_works() {
    let acts = sunk(34, 0);
    let found = found_with(&[], &acts, UTC);
    let [atlantis] = found.as_slice() else {
        panic!("{found:?}");
    };
    assert_eq!(atlantis.wonder, Wonder::Atlantis);
    assert_eq!(
        atlantis.line,
        "130 actions until 34 days ago, none since, while your crew did 750 others"
    );
    assert_eq!(
        atlantis.lasting.as_deref(),
        Some("130 actions, then none for 34 days, while your crew did 750 others")
    );
    assert_eq!(
        atlantis.about.place(&acts),
        Some(Behavior::One(Label::GitChange))
    );

    assert!(found_with(&[], &sunk(29, 0), UTC).is_empty());
    assert!(found_with(&[], &sunk(34, 4), UTC).is_empty());
    let mut not_among_five = sunk(34, 0);
    for label in [Label::ListFiles, Label::Lint] {
        for n in 0..140 {
            not_among_five.push(act(
                TODAY - Duration::minutes(n),
                Behavior::One(label),
                LUNA,
            ));
        }
    }
    assert!(found_with(&[], &not_among_five, UTC).is_empty());
}

#[test]
fn atlantis_keeps_a_line_that_stays_true_once_its_behavior_comes_back() {
    let home = Home::new();
    let acts = sunk(34, 0);
    let risen = home.sky().risen(&acts, TODAY);
    assert_eq!(risen.len(), 1);
    assert_eq!(risen[0].behavior, "changed git state");
    assert_eq!(risen[0].risen, TODAY - Duration::days(4));
    assert!(risen[0].why.starts_with("130 actions until 34 days ago"));

    let mut back = acts.clone();
    back.push(act(TODAY, Behavior::One(Label::GitChange), OPUS));
    let risen = home.sky().risen(&back, TODAY);
    assert_eq!(risen.len(), 1);
    assert_eq!(
        risen[0].why,
        "130 actions, then none for 34 days, while your crew did 750 others"
    );
    assert_eq!(risen[0].risen, TODAY - Duration::days(4));
}

#[test]
fn every_wonder_rises_from_made_up_records() {
    let runs = every_other_day(100);
    let (gardens, mut acts) = gardens_run(false);
    acts.extend(retired(500, 61, HAIKU));
    acts.extend(sunk(34, 0));
    let mut logs: Vec<Log> = vec![
        three_makers("run-argo", RunOutcome::Merged),
        first_try_run("run-fleece"),
        rocky("run-rocks", &[false; 5]),
        guarded("run-guarded", 10, false),
        caught_run("run-sirens", true),
        night_run("run-dragon", false),
        harpies("run-harpies", 3),
        council("run-council", 2),
        gardens,
        big_crew("run-colossus", 10, 0),
        lighthouse("run-lighthouse", OPUS, "anthropic", false),
        temple("run-temple", ResumeCause::User),
        judged("run-judged", 25, None),
    ];
    logs.extend(runs);
    let logs: Vec<&Log> = logs.iter().collect();
    let mut risen: Vec<Wonder> = found_with(&logs, &acts, UTC)
        .into_iter()
        .map(|found| found.wonder)
        .collect();
    risen.sort();
    assert_eq!(risen, Wonder::ALL);
}

#[test]
fn the_store_names_only_the_wonders_that_rose() {
    let home = Home::new();
    home.write(&three_makers("run-argo", RunOutcome::Merged));
    home.sky().risen(&ACTS, TODAY);
    let kept = fs::read_to_string(home.store()).unwrap();
    for wonder in Wonder::ALL {
        let named = serde_json::to_string(&wonder).unwrap();
        assert_eq!(
            kept.contains(&named),
            wonder == Wonder::Argo,
            "{named} in {kept}"
        );
    }
}
