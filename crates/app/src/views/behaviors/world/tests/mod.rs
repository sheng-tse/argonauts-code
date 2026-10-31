mod made_up;
use argonauts_core::behaviors::Behavior;
use argonauts_core::behaviors::behavior_world::{
    Burst, CliFailure, Crash, ModelBehavior, MonthActions, NewerModel, QuietStretch, StormWord,
};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use super::super::read::{Outcome, Recorded, SessionEnded};
use super::*;

/// Sunday 25 October 2026, 9:00 PM in Taipei, where these made-up crews work.
const END: OffsetDateTime = datetime!(2026-10-25 21:00 +8);

fn month() -> Period {
    Period::new(END, Some(30), WallClock::pinned(END))
}

/// A made-up action of `behavior`, a label's id, in `session` of run `run-1`.
fn act(session: &str, behavior: &str, at: OffsetDateTime) -> Recorded {
    act_in("run-1", session, behavior, at)
}

fn act_in(run: &str, session: &str, behavior: &str, at: OffsetDateTime) -> Recorded {
    Recorded {
        key: format!(
            "notes-app-0123456789ab/{run}/{session}/{}",
            at.unix_timestamp()
        ),
        session: format!("notes-app-0123456789ab/{run}/{session}"),
        run: Some(run.to_owned()),
        cli: "codex".to_owned(),
        model: Some("gpt-6-luna".to_owned()),
        role: "builder".to_owned(),
        agent: "build:due-flag".to_owned(),
        workflow: "spec-first".to_owned(),
        project: "notes-app".to_owned(),
        at,
        nth: 0,
        of: 0,
        tool: "command_execution".to_owned(),
        what: String::new(),
        behavior: Behavior::from_id(behavior).unwrap_or(Behavior::Unlabelled),
        failed: false,
        in_task: true,
        outcome: None,
        settled_at: None,
        ended: None,
        ended_at: None,
        place: None,
        run_shows: None,
    }
}

/// `count` actions of `behavior` in `session`, a minute apart from `at`.
fn acts(session: &str, behavior: &str, at: OffsetDateTime, count: i64) -> Vec<Recorded> {
    (0..count)
        .map(|n| act(session, behavior, at + Duration::minutes(n)))
        .collect()
}

fn days_ago(days: i64, hour: u8) -> OffsetDateTime {
    let day = END.date() - Duration::days(days);
    day.with_hms(hour, 0, 0)
        .unwrap()
        .assume_offset(END.offset())
}

fn facts_of(all: &[Recorded]) -> WorldFacts {
    facts(all, &month(), &DisplayNames::default(), Some("Asia/Taipei"))
}

#[test]
fn days_go_back_from_the_end_on_your_clock() {
    let period = month();
    assert_eq!(period.day(END), 0);
    assert_eq!(
        period.day(datetime!(2026-10-24 16:00 UTC)),
        0,
        "midnight in Taipei"
    );
    assert_eq!(period.day(datetime!(2026-10-24 15:59 UTC)), 1);
    assert_eq!(period.day(days_ago(29, 0)), 29);
    assert_eq!(period.start(), Some(datetime!(2026-09-26 00:00 +8)));
    assert_eq!(
        period.before(),
        Some((
            datetime!(2026-08-27 00:00 +8),
            datetime!(2026-09-26 00:00 +8)
        ))
    );
    let all_time = Period::new(END, None, WallClock::pinned(END));
    assert_eq!((all_time.start(), all_time.before()), (None, None));
}

#[test]
fn a_behavior_counts_its_period_the_one_before_and_when_it_was_last_used() {
    let mut all = acts("s-1", "run_tests", days_ago(2, 10), 3);
    all.extend(acts("s-2", "run_tests", days_ago(5, 10), 2));
    all.extend(acts("s-3", "run_tests", days_ago(12, 10), 4));
    all.extend(acts("s-4", "run_tests", days_ago(40, 10), 6));
    all.extend(acts("s-5", "web_fetch", days_ago(35, 10), 7));
    all.extend(acts("s-6", "git_history", days_ago(80, 10), 1));
    let world = facts_of(&all);
    let tests = world.behavior("ran the test suite").unwrap();
    assert_eq!(
        (tests.count, tests.last_period, tests.last_used),
        (9, 6, Some(2))
    );
    assert_eq!(tests.days_in_last_7, 2);
    assert_eq!(
        tests.weeks,
        [5, 4, 0, 0],
        "four full weeks, the latest first"
    );
    let fetched = world.behavior("fetched from the web").unwrap();
    assert_eq!(
        (fetched.count, fetched.last_period, fetched.last_used),
        (0, 7, Some(35)),
        "used only before the period: an atoll, and its climate from 35 days ago"
    );
    assert_eq!(
        world.behaviors.len(),
        2,
        "one used 80 days ago and not since is in neither"
    );
    assert_eq!(world.end, END);
}

#[test]
fn a_session_s_order_gives_its_waits_runs_moves_and_end() {
    let start = days_ago(1, 9);
    let mut session = acts("s-1", "read_source", start, 2);
    session.extend(acts("s-1", "run_tests", start + Duration::minutes(2), 6));
    session.push(act("s-1", "edit_source", start + Duration::minutes(20)));
    session.push(act("s-1", "run_tests", start + Duration::minutes(21)));
    session.push(act("s-1", "kill_shell", start + Duration::minutes(40)));
    session.push(act("s-1", "run_tests", start + Duration::minutes(41)));
    for recorded in &mut session {
        recorded.ended = Some(SessionEnded::Stopped);
    }
    let world = facts_of(&session);
    let tests = world.behavior("ran the test suite").unwrap();
    assert_eq!((tests.runs_in_a_row, tests.longest_run), (1, 6));
    assert_eq!(
        world.behavior("edited a source file").unwrap().after_a_wait,
        1,
        "13 minutes after the test run before it"
    );
    assert_eq!(
        tests.after_a_wait, 0,
        "an action no label fits still came between, so its wait isn't the tests'"
    );
    assert_eq!((tests.session_ends, tests.stopped), (1, 1));
    let moves: Vec<(&str, &str, u32)> = world
        .moves
        .iter()
        .map(|m| (m.from.as_str(), m.to.as_str(), m.count))
        .collect();
    assert_eq!(
        moves,
        [
            ("edited a source file", "ran the test suite", 1),
            ("ran the test suite", "edited a source file", 1),
            ("read a source file", "ran the test suite", 1),
        ],
        "a run of one behavior is one step, and an action no label fits is passed over"
    );
}

#[test]
fn sessions_that_end_on_a_behavior_say_how() {
    let mut all = Vec::new();
    for (n, how) in [
        SessionEnded::Completed,
        SessionEnded::Failed,
        SessionEnded::Crashed,
        SessionEnded::Crashed,
    ]
    .into_iter()
    .enumerate()
    {
        let at = days_ago(n as i64, 11);
        let mut session = vec![act(&format!("s-{n}"), "git_status", at)];
        session.push(act(
            &format!("s-{n}"),
            "git_change",
            at + Duration::minutes(1),
        ));
        for recorded in &mut session {
            recorded.ended = Some(how);
            recorded.ended_at = Some(at + Duration::minutes(3));
        }
        all.extend(session);
    }
    all.extend(acts("s-going", "git_change", days_ago(0, 12), 2));
    let world = facts_of(&all);
    let changed = world.behavior("changed git state").unwrap();
    assert_eq!(
        (changed.session_ends, changed.cli_failed, changed.stopped),
        (4, 3, 0),
        "a session still going hasn't ended"
    );
    let crash = |days: i64| Crash {
        at: days_ago(days, 11) + Duration::minutes(3),
        agent: "codex".to_owned(),
        model: Some("gpt-6-luna".to_owned()),
    };
    assert_eq!(changed.crashes, [crash(2), crash(3)], "newest first");
    assert_eq!(
        world
            .behavior("checked git status or diff")
            .unwrap()
            .session_ends,
        0
    );
}

#[test]
fn failures_and_what_became_of_the_work_keep_their_days() {
    let mut all = acts("s-1", "build", days_ago(9, 10), 4);
    all[0].failed = true;
    all[1].failed = true;
    all[2].outcome = Some(Outcome::SentBack);
    all.extend(acts("s-2", "build", days_ago(3, 10), 3));
    all[4].failed = true;
    for dropped in &mut all[5..7] {
        dropped.outcome = Some(Outcome::Dropped);
        dropped.settled_at = Some(days_ago(1, 18));
    }
    all[3].outcome = Some(Outcome::Dropped);
    let world = facts_of(&all);
    let build = world.behavior("built the project").unwrap();
    assert_eq!((build.failed, build.last_failed), (3, Some(3)));
    assert_eq!(build.sent_back, 1);
    assert_eq!(
        (build.dropped, build.last_dropped),
        (3, Some(1)),
        "dropped when its run ended, two days after the action"
    );
}

#[test]
fn the_busiest_hour_and_the_most_agents_within_5_minutes() {
    let start = days_ago(4, 14);
    let mut all = Vec::new();
    for n in 0..31 {
        all.push(act(
            &format!("s-{}", n % 7),
            "shell",
            start + Duration::minutes(2 * n),
        ));
    }
    all.extend(acts("s-late", "shell", start + Duration::hours(3), 3));
    let world = facts_of(&all);
    let shell = world.behavior("ran a shell command").unwrap();
    assert_eq!(
        shell.busiest_hour,
        Some(Burst {
            count: 31,
            at: start
        }),
        "the hour from its first action holds 31 of them, the 60th minute among them"
    );
    assert_eq!(
        shell.most_agents_in_5_minutes,
        Some(Burst {
            count: 3,
            at: start
        })
    );
}

#[test]
fn the_crew_s_idle_and_quiet_days_its_nights_and_earlier_months() {
    let mut all = Vec::new();
    for day in [3, 4, 7, 8, 9, 12, 20] {
        all.push(act(&format!("s-{day}"), "read_source", days_ago(day, 10)));
    }
    all.push(act("s-night", "read_source", days_ago(6, 23)));
    all.push(act("s-dawn", "read_source", days_ago(6, 4)));
    all.push(act("s-early", "read_source", days_ago(6, 5)));
    all.push(act("s-before", "read_source", days_ago(31, 10)));
    all.push(act("s-before", "read_source", days_ago(32, 10)));
    all.push(act(
        "s-august",
        "read_source",
        datetime!(2026-08-14 10:00 +8),
    ));
    let world = facts_of(&all);
    let crew = &world.crew;
    assert_eq!(crew.idle_days, 3);
    assert_eq!(
        crew.quiet_days,
        [
            5, 10, 11, 13, 14, 15, 16, 17, 18, 19, 21, 22, 23, 24, 25, 26, 27, 28, 29
        ]
        .len() as u32
    );
    assert_eq!(
        crew.quiet_stretches[..2],
        [
            QuietStretch { ends: 10, days: 2 },
            QuietStretch { ends: 13, days: 7 }
        ]
    );
    assert_eq!(crew.night_actions, 2, "11:00 PM and 4:00 AM, not 5:00 AM");
    assert_eq!(
        crew.earlier_months,
        [
            MonthActions {
                month: "2026-09".to_owned(),
                actions: 2
            },
            MonthActions {
                month: "2026-08".to_owned(),
                actions: 1
            }
        ]
    );
    assert_eq!(crew.storm, StormWord::Typhoon);
}

#[test]
fn a_new_crew_has_no_quiet_days_before_its_first_action() {
    let mut all = acts("s-1", "read_source", days_ago(2, 10), 2);
    all.extend(acts("s-2", "read_source", days_ago(0, 10), 2));
    let crew = facts_of(&all).crew;
    assert_eq!((crew.idle_days, crew.quiet_days), (0, 1));
    assert!(crew.quiet_stretches.is_empty() && crew.earlier_months.is_empty());
    let nothing = facts_of(&[]);
    assert_eq!((nothing.crew.idle_days, nothing.crew.quiet_days), (0, 0));
    assert!(nothing.behaviors.is_empty() && nothing.moves.is_empty());
}

#[test]
fn all_time_counts_its_weeks_back_to_the_first_action() {
    let mut all = acts("s-1", "read_source", days_ago(20, 10), 2);
    all.extend(acts("s-2", "read_source", days_ago(1, 10), 3));
    let all_time = Period::new(END, None, WallClock::pinned(END));
    let world = facts(&all, &all_time, &DisplayNames::default(), None);
    let read = world.behavior("read a source file").unwrap();
    assert_eq!((read.count, read.last_period), (5, 0));
    assert_eq!(read.weeks, [3, 0, 2], "21 days back to the first action");
    assert!(world.crew.earlier_months.is_empty());
    assert_eq!(world.crew.storm, StormWord::Hurricane);
}

/// Codex's 6 Luna loops on the tests and its older 5.6 Luna crashed nine days ago; Haiku and a
/// local Qwen worked beside 6 Luna in one run of the last hours.
fn a_mixed_crew() -> Vec<Recorded> {
    let model = |mut recorded: Recorded, cli: &str, model: &str| {
        recorded.cli = cli.to_owned();
        recorded.model = Some(model.to_owned());
        recorded
    };
    let mut all = Vec::new();
    for minute in [5, 4, 3, 2, 1] {
        let at = END - Duration::minutes(minute);
        let mut tests = act_in("run-1", "s-luna", "run_tests", at);
        tests.outcome = Some(Outcome::Merged);
        all.push(model(tests, "codex", "gpt-6-luna"));
    }
    for minute in 0..3 {
        let at = days_ago(9, 10) + Duration::minutes(minute);
        let mut read = act_in("run-2", "s-old", "read_source", at);
        read.failed = minute == 1;
        read.ended = Some(SessionEnded::Crashed);
        read.ended_at = Some(days_ago(9, 11));
        all.push(model(read, "codex", "gpt-5.6-luna"));
    }
    for minute in [0, 1] {
        let at = END - Duration::hours(2) + Duration::minutes(minute);
        let edit = act_in("run-1", "s-haiku", "edit_source", at);
        all.push(model(edit, "claude-code", "claude-haiku-5-5"));
    }
    let local = act_in("run-1", "s-qwen", "shell", END - Duration::minutes(30));
    all.push(model(local, "opencode", "ollama/qwen3-coder:30b"));
    all
}

#[test]
fn each_model_s_facts_are_its_own_actions() {
    let world = facts_of(&a_mixed_crew());
    let order: Vec<(&str, &str, &str)> = world
        .models
        .iter()
        .map(|m| (m.agent.as_str(), m.model.as_str(), m.maker.as_str()))
        .collect();
    assert_eq!(
        order,
        [
            ("codex", "gpt-6-luna", "openai"),
            ("codex", "gpt-5.6-luna", "openai"),
            ("claude-code", "claude-haiku-5-5", "anthropic"),
            ("opencode", "ollama/qwen3-coder:30b", "alibaba"),
        ],
        "most used first, each with the maker whose galaxy it sits in"
    );
    let luna = &world.models[0];
    let recent = (
        luna.last_7_days,
        luna.last_24_hours,
        luna.last_hour,
        luna.last_10_minutes,
    );
    assert_eq!((luna.actions, recent), (5, (5, 5, 5, 5)));
    let days = (
        luna.first_used,
        luna.last_used,
        luna.days_used,
        luna.sessions,
    );
    assert_eq!(days, (0, 0, 1, 1));
    assert_eq!((luna.merged, luna.sent_back, luna.dropped), (5, 0, 0));
    assert_eq!(luna.last_action, Some(END - Duration::minutes(1)));
    assert_eq!(
        luna.behaviors,
        [ModelBehavior {
            behavior: "ran the test suite".to_owned(),
            count: 5,
            failed: 0,
            last_used: 0,
            days_in_last_7: 1,
            runs_in_a_row: 1,
        }]
    );
    assert_eq!(luna.newer, None);
    let older = &world.models[1];
    assert_eq!((older.actions, older.failed, older.last_7_days), (3, 1, 0));
    assert_eq!((older.first_used, older.last_used), (9, 9));
    assert_eq!(
        older.cli_failures,
        [CliFailure {
            at: days_ago(9, 11)
        }]
    );
    assert_eq!(
        older.newer,
        Some(NewerModel {
            model: "gpt-6-luna".to_owned(),
            came: 0
        }),
        "6 Luna is the newer of the Luna line"
    );
    let haiku = &world.models[2];
    assert_eq!((haiku.last_hour, haiku.last_24_hours), (0, 2));
}

#[test]
fn models_that_share_runs_pair_and_a_run_of_makers_flies() {
    let world = facts_of(&a_mixed_crew());
    let pairs: Vec<(&str, &str, u32, f32)> = world
        .pairs
        .iter()
        .map(|p| (p.models[0].as_str(), p.models[1].as_str(), p.runs, p.lift))
        .collect();
    assert_eq!(
        pairs,
        [
            ("gpt-6-luna", "claude-haiku-5-5", 1, 2.0),
            ("gpt-6-luna", "ollama/qwen3-coder:30b", 1, 2.0),
            ("claude-haiku-5-5", "ollama/qwen3-coder:30b", 1, 2.0),
        ],
        "twice as often as chance in two runs; 5.6 Luna shared none"
    );
    assert_eq!(
        world.mixed_runs.len(),
        1,
        "the run nine days ago is long past"
    );
    let run = &world.mixed_runs[0];
    let began = END - Duration::hours(2);
    assert_eq!((run.run.as_str(), run.at, run.actions), ("run-1", began, 8));
    assert_eq!(
        run.makers,
        ["anthropic", "alibaba", "openai"],
        "in the order they first acted"
    );
    assert_eq!(
        run.models,
        ["claude-haiku-5-5", "ollama/qwen3-coder:30b", "gpt-6-luna"]
    );
}

#[test]
fn a_line_is_a_model_s_name_without_its_version() {
    let line = |name: &str| models::line(name);
    assert_eq!(line("GPT 5.6 Luna"), Some(("GPT Luna".to_owned(), 5.6)));
    assert_eq!(line("Opus 5.5"), Some(("Opus".to_owned(), 5.5)));
    assert_eq!(line("GPT 6 Luna"), Some(("GPT Luna".to_owned(), 6.0)));
    assert_eq!(line("Qwen3 Coder 30B"), None);
    assert_eq!(line("Codex"), None);
}

/// A session's CLI failure is its model's: the one that took most of its actions.
#[test]
fn a_failed_session_counts_for_the_model_that_ran_most_of_it() {
    let start = days_ago(2, 10);
    let mut session = acts("s-1", "edit_source", start, 3);
    let mut last = act("s-1", "run_tests", start + Duration::minutes(5));
    last.cli = "claude-code".to_owned();
    last.model = Some("claude-haiku-5-5".to_owned());
    session.push(last);
    for recorded in &mut session {
        recorded.ended = Some(SessionEnded::Failed);
    }
    let world = facts_of(&session);
    let failures: Vec<(&str, usize)> = world
        .models
        .iter()
        .map(|m| (m.model.as_str(), m.cli_failures.len()))
        .collect();
    assert_eq!(failures, [("gpt-6-luna", 1), ("claude-haiku-5-5", 0)]);
}
