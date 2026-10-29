//! Four made-up crews on the app's own label places, each with its own world: R, a busy crew
//! over 30 days; S, a crew that tests and works nights; T, a crew that reads and has been quiet
//! for 9 days; and U, R's crew after it stopped changing git state itself 34 days ago. A seeded
//! generator sets every day, minute, session and ending, so each crew is the same every run.

use argonauts_core::behaviors::Behavior;
use argonauts_tui::format::WallClock;
use regex::Regex;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use super::super::read::{Outcome, Recorded, SessionEnded};
use super::Period;

/// The behaviors the crews do, by their place in this list.
const BEHAVIORS: [&str; 44] = [
    "read_source",
    "read_test",
    "read_config",
    "read_doc",
    "print_files",
    "print_test_files",
    "search_code",
    "list_files",
    "run_tests",
    "run_tests_failed",
    "build",
    "lint",
    "git_history",
    "git_status",
    "git_change",
    "shell",
    "python_snippet",
    "file_ops",
    "edit_source",
    "edit_failed",
    "edit_test",
    "edit_test_added",
    "write_source",
    "write_test",
    "write_doc",
    "edit_config",
    "edit_doc",
    "install",
    "web_fetch",
    "web_search",
    "answer",
    "answer_failed",
    "ask_review",
    "subagent",
    "workflow",
    "todo",
    "tool_search",
    "mcp_tool",
    "read_records",
    "run_status",
    "git_status+git_history",
    "run_tests+git_status",
    "shell+file_ops",
    "git_status+git_change",
];

/// The models the crews run, by their place in this list: the CLI and the model's id.
const MODELS: [(&str, &str); 12] = [
    ("claude-code", "claude-haiku-5-5"),
    ("codex", "gpt-6-luna"),
    ("claude-code", "claude-opus-5-5"),
    ("opencode", "ollama/qwen3-coder:30b"),
    ("codex", "gpt-6-codex"),
    ("codex", "gpt-6-astra"),
    ("codex", "gpt-6.1-sol"),
    ("antigravity-cli", "gemini-3.8-flash"),
    ("claude-code", "claude-sonnet-5-5"),
    ("opencode", "zai/glm-5"),
    ("opencode", "moonshotai/kimi-k2.6"),
    ("opencode", "ollama/gemma3:12b"),
];

/// R's actions in this period, as (behavior, model, how many), in the order they're dealt out.
#[rustfmt::skip]
const R_ACTIONS: [(usize, usize, usize); 170] = [
    (0, 0, 2), (1, 0, 20), (4, 0, 16), (5, 0, 4), (6, 0, 7), (7, 0, 1), (8, 0, 21), (9, 0, 11),
    (10, 0, 28), (11, 0, 2), (14, 0, 29), (15, 0, 3), (17, 0, 7), (18, 0, 25), (19, 0, 78),
    (21, 0, 12), (22, 0, 7), (23, 0, 2), (24, 0, 99), (25, 0, 3), (28, 0, 1), (29, 0, 2),
    (30, 0, 148), (31, 0, 20), (32, 0, 8), (33, 0, 1), (34, 0, 13), (35, 0, 4), (36, 0, 2),
    (37, 0, 16), (38, 0, 8), (39, 0, 6), (41, 0, 11), (42, 0, 2), (43, 0, 17), (1, 1, 1),
    (5, 1, 1), (6, 1, 1), (8, 1, 8), (10, 1, 1), (14, 1, 1), (18, 1, 3), (22, 1, 2), (25, 1, 1),
    (28, 1, 1), (30, 1, 35), (38, 1, 2), (39, 1, 1), (40, 1, 6), (1, 2, 1), (3, 2, 1), (4, 2, 5),
    (5, 2, 1), (6, 2, 1), (8, 2, 2), (9, 2, 1), (11, 2, 1), (14, 2, 6), (15, 2, 2), (18, 2, 2),
    (19, 2, 1), (22, 2, 6), (24, 2, 10), (25, 2, 1), (27, 2, 1), (29, 2, 2), (35, 2, 2),
    (36, 2, 1), (39, 2, 1), (40, 2, 2), (43, 2, 3), (0, 3, 1), (1, 3, 1), (4, 3, 1), (6, 3, 3),
    (8, 3, 1), (9, 3, 7), (10, 3, 8), (13, 3, 1), (14, 3, 2), (15, 3, 3), (17, 3, 1), (19, 3, 1),
    (22, 3, 5), (24, 3, 1), (29, 3, 2), (38, 3, 2), (39, 3, 1), (40, 3, 3), (42, 3, 1), (1, 4, 6),
    (5, 4, 1), (6, 4, 1), (8, 4, 5), (9, 4, 1), (10, 4, 4), (14, 4, 4), (18, 4, 1), (19, 4, 10),
    (22, 4, 3), (25, 4, 2), (29, 4, 1), (36, 4, 1), (38, 4, 1), (39, 4, 2), (4, 5, 1), (5, 5, 1),
    (6, 5, 2), (8, 5, 1), (9, 5, 2), (10, 5, 10), (11, 5, 1), (14, 5, 1), (17, 5, 1), (19, 5, 2),
    (24, 5, 1), (29, 5, 2), (38, 5, 8), (40, 5, 2), (43, 5, 1), (4, 6, 1), (8, 6, 2), (9, 6, 4),
    (11, 6, 1), (12, 6, 1), (13, 6, 1), (14, 6, 3), (15, 6, 1), (18, 6, 1), (19, 6, 4),
    (21, 6, 1), (22, 6, 3), (24, 6, 2), (35, 6, 1), (38, 6, 1), (39, 6, 1), (40, 6, 1),
    (42, 6, 1), (43, 6, 1), (1, 7, 2), (10, 7, 3), (19, 7, 7), (22, 7, 2), (25, 7, 1), (29, 7, 1),
    (43, 7, 2), (4, 8, 2), (8, 8, 1), (14, 8, 3), (22, 8, 1), (26, 8, 1), (37, 8, 1), (38, 8, 1),
    (40, 8, 1), (9, 9, 1), (10, 9, 2), (18, 9, 1), (19, 9, 2), (22, 9, 1), (24, 9, 1), (6, 10, 1),
    (10, 10, 5), (24, 10, 1), (31, 10, 1), (40, 10, 1), (41, 10, 1), (10, 11, 3), (14, 11, 1),
    (15, 11, 1), (29, 11, 1),
];

/// S's and T's actions, as each behavior with how many of it each model did.
type Spread = &'static [(usize, &'static [(usize, usize)])];

const S_ACTIONS: Spread = &[
    (8, &[(2, 60), (6, 45), (4, 25), (3, 20)]),
    (9, &[(2, 35), (6, 30), (4, 15), (3, 15)]),
    (18, &[(2, 50), (6, 40), (4, 20), (3, 10)]),
    (19, &[(2, 20), (6, 15), (3, 20)]),
    (20, &[(2, 30), (6, 25), (4, 15)]),
    (21, &[(2, 20), (6, 15), (4, 10)]),
    (10, &[(2, 25), (6, 20), (4, 15)]),
    (11, &[(6, 20), (4, 15)]),
    (15, &[(2, 15), (3, 20), (0, 10)]),
    (13, &[(2, 20), (6, 20), (0, 10)]),
    (14, &[(2, 15), (6, 10)]),
    (30, &[(2, 30), (6, 25), (4, 15)]),
    (1, &[(2, 15), (6, 15), (3, 10)]),
    (0, &[(2, 20), (6, 20), (3, 15)]),
    (35, &[(2, 20)]),
    (32, &[(2, 15), (6, 10)]),
    (6, &[(2, 15), (3, 15)]),
];

const T_ACTIONS: Spread = &[
    (0, &[(7, 50), (10, 30), (8, 30)]),
    (3, &[(7, 40), (9, 25), (8, 25)]),
    (4, &[(7, 30), (10, 25)]),
    (6, &[(7, 45), (10, 30), (9, 25)]),
    (7, &[(7, 20), (9, 20)]),
    (28, &[(7, 30), (10, 25), (8, 20)]),
    (29, &[(7, 40), (10, 25), (9, 20)]),
    (24, &[(8, 30), (7, 25)]),
    (26, &[(8, 25), (7, 20)]),
    (30, &[(7, 20), (8, 15), (10, 15)]),
    (32, &[(8, 20)]),
    (33, &[(8, 15)]),
    (38, &[(0, 20)]),
    (39, &[(0, 15)]),
];

/// "Changed git state", which U stopped doing.
const GIT_CHANGE: usize = 14;

/// What sets a crew apart besides its counts.
struct Crew {
    seed: u32,
    /// Days since its last action, and the days of the period it acted on before them.
    idle: u32,
    span: u32,
    quiet: &'static [u32],
    /// The share of its sessions at night, and how often an agent loops.
    night: f64,
    loops: f64,
    /// The work reviews send back and the work that's dropped, and how strongly.
    sent: Option<(&'static str, f64)>,
    dropped: Option<(&'static str, f64)>,
    /// The share of sessions whose CLI crashes, and of those stopped.
    crash: f64,
    stop: f64,
    /// One busy afternoon: its day, the share of some behaviors' actions on it, and the
    /// behavior its first sessions open with.
    crunch: Option<Crunch>,
    /// Behaviors used only between two days back from the end.
    era: &'static [(usize, u32, u32)],
    /// Behaviors agents waited before, and how often.
    waits: Option<(&'static str, f64)>,
    /// Behaviors that fail more often than the usual 1 in 80, and how often.
    fails: Option<(&'static str, f64)>,
    /// Actions no label fits.
    unlabelled: usize,
    /// Behaviors used last period and not in this one.
    before: &'static [usize],
    /// Its actions before this period, and its usual actions a day.
    earlier: u32,
    per_day: f64,
}

struct Crunch {
    day: u32,
    shares: &'static [(usize, f64)],
    opens_with: usize,
    sessions: usize,
}

const R: Crew = Crew {
    seed: 41,
    idle: 0,
    span: 30,
    quiet: &[5, 6, 12, 13, 19, 20, 26, 27],
    night: 0.04,
    loops: 0.12,
    sent: Some(("^built the project", 2.6)),
    dropped: Some(("wrote a doc", 1.4)),
    crash: 0.06,
    stop: 0.2,
    crunch: Some(Crunch {
        day: 8,
        shares: &[(30, 0.3), (19, 0.22), (1, 0.5)],
        opens_with: 1,
        sessions: 8,
    }),
    era: &[
        (12, 15, 29),
        (27, 15, 29),
        (28, 15, 29),
        (29, 15, 29),
        (33, 15, 29),
        (7, 15, 29),
        (3, 15, 29),
        (26, 8, 13),
        (23, 8, 13),
        (36, 8, 13),
        (0, 8, 13),
    ],
    waits: Some(("^ran a shell command$|^searched code", 0.75)),
    fails: None,
    unlabelled: 38,
    before: &[2, 20],
    earlier: 4180,
    per_day: 30.0,
};

const S: Crew = Crew {
    seed: 71,
    idle: 0,
    span: 30,
    quiet: &[],
    night: 0.3,
    loops: 0.45,
    sent: Some(("edited a test file", 1.1)),
    dropped: None,
    crash: 0.1,
    stop: 0.07,
    crunch: Some(Crunch {
        day: 2,
        shares: &[(8, 0.25), (9, 0.3)],
        opens_with: 8,
        sessions: 7,
    }),
    era: &[],
    waits: Some(("^built the project$", 0.3)),
    fails: Some(("shell|Python|built", 0.2)),
    unlabelled: 12,
    before: &[],
    earlier: 3720,
    per_day: 33.0,
};

const T: Crew = Crew {
    seed: 83,
    idle: 9,
    span: 21,
    quiet: &[11, 12, 15, 18, 19, 25, 26, 27],
    night: 0.01,
    loops: 0.06,
    sent: None,
    dropped: Some(("doc|web", 0.7)),
    crash: 0.03,
    stop: 0.16,
    crunch: Some(Crunch {
        day: 13,
        shares: &[(29, 0.35), (6, 0.3)],
        opens_with: 29,
        sessions: 6,
    }),
    era: &[
        (28, 12, 20),
        (29, 12, 20),
        (24, 10, 16),
        (26, 10, 16),
        (33, 16, 29),
        (39, 16, 29),
    ],
    waits: Some(("^fetched from the web$", 0.45)),
    fails: None,
    unlabelled: 90,
    before: &[8, 10, 18, 9],
    earlier: 1460,
    per_day: 4.0,
};

const U: Crew = Crew {
    era: &[
        (12, 15, 29),
        (27, 15, 29),
        (29, 21, 29),
        (33, 15, 29),
        (7, 15, 29),
        (3, 15, 29),
        (26, 8, 13),
        (23, 8, 13),
        (36, 8, 13),
        (0, 8, 13),
    ],
    before: &[2, 20, GIT_CHANGE],
    ..R
};

/// Which behaviors usually follow which: the first rule whose first pattern fits a behavior
/// gives the behaviors its second pattern fits.
const NEXT: [(&str, &str); 16] = [
    (
        "^read a (source|test|config)|^printed",
        "edited a source|searched code|edited a test|^read a",
    ),
    (
        "searched code",
        "^read a source|edited a source|^read a test|^printed",
    ),
    ("listed files", "^read a|searched code"),
    (
        "^ran the test suite, which failed",
        "edited a source|^read a source|edited a test|tried an edit",
    ),
    (
        "^ran the test suite",
        "checked git status|returned its|ran the test suite",
    ),
    ("edited a test|wrote a test", "^ran the test suite"),
    (
        "edited a source|wrote a source|tried an edit",
        "^ran the test suite|built the project|edited a source|linter",
    ),
    (
        "built the project|linter",
        "^ran the test suite|edited a source",
    ),
    (
        "checked git status",
        "changed git state|read git history|returned its",
    ),
    (
        "changed git state|git history",
        "returned its|checked git status",
    ),
    (
        "shell command|Python|moved, copied",
        "ran a shell|^read a|edited a",
    ),
    ("doc", "edited a doc|wrote a doc|read a doc"),
    (
        "web",
        "fetched from the web|searched the web|read a doc|wrote a doc",
    ),
    (
        "to-do|deferred tool|MCP",
        "^read a source|edited a source|searched code",
    ),
    ("review|subagent|workflow", "returned its|^read a"),
    (
        "run records|run's status",
        "read Argonauts|checked the run's|returned its",
    ),
];
const STARTER: &str = "^read a |searched code|listed files|checked git status|updated its to-do|run records|run's status|searched the web";
const LOOPY: &str = "^ran the test suite|tried an edit|ran a shell command|edited a source|ran a Python|searched code";
const ENDER: &str = "returned its structured answer|asked another agent to review";
const FAILED: &str = "which failed|didn't apply|didn't fit|failed to";

/// One of the four crews: every action recorded, and the period its map shows.
pub(super) struct MadeUp {
    pub records: Vec<Recorded>,
    pub period: Period,
}

pub(super) fn crew(key: char) -> MadeUp {
    let (crew, actions) = match key {
        'R' => (&R, dealt(&R_ACTIONS)),
        'S' => (&S, spread(S_ACTIONS)),
        'T' => (&T, spread(T_ACTIONS)),
        'U' => (&U, without(dealt(&R_ACTIONS), GIT_CHANGE)),
        other => panic!("no made-up crew {other}"),
    };
    Generated::of(crew, &actions).records(crew, &actions)
}

fn dealt(actions: &[(usize, usize, usize)]) -> Vec<(usize, usize)> {
    actions
        .iter()
        .flat_map(|&(behavior, model, count)| std::iter::repeat_n((behavior, model), count))
        .collect()
}

fn spread(actions: Spread) -> Vec<(usize, usize)> {
    actions
        .iter()
        .flat_map(|&(behavior, models)| {
            models
                .iter()
                .flat_map(move |&(model, count)| std::iter::repeat_n((behavior, model), count))
        })
        .collect()
}

fn without(actions: Vec<(usize, usize)>, behavior: usize) -> Vec<(usize, usize)> {
    actions
        .into_iter()
        .filter(|(b, _)| *b != behavior)
        .collect()
}

fn words(behavior: usize) -> String {
    Behavior::from_id(BEHAVIORS[behavior])
        .unwrap_or(Behavior::Unlabelled)
        .words()
}

/// A small fast generator: the same seed gives the same numbers, each in 0..1.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ending {
    Completed,
    Stopped,
    Crashed,
}

struct Session {
    actions: Vec<usize>,
    outcome: Option<Outcome>,
    ended: Ending,
}

/// When each action was, in minutes from midnight of the period's first day, and the
/// sessions they were dealt into.
struct Generated {
    minutes: Vec<f32>,
    sessions: Vec<Session>,
    failed: Vec<bool>,
    days: u32,
    now: f64,
}

impl Generated {
    fn of(crew: &Crew, actions: &[(usize, usize)]) -> Self {
        let days = crew.idle + crew.span;
        let mut r = Rng(crew.seed.wrapping_mul(977).wrapping_add(13));
        let mut waits = Rng(crew.seed.wrapping_mul(31).wrapping_add(7));
        let active: Vec<u32> = (crew.idle..days)
            .filter(|day| !crew.quiet.contains(day))
            .collect();
        let crunch = crew
            .crunch
            .as_ref()
            .filter(|crunch| active.contains(&crunch.day));
        let mut day_of = vec![0u32; actions.len()];
        for (at, (behavior, _)) in actions.iter().enumerate() {
            let share = crunch.and_then(|crunch| {
                crunch
                    .shares
                    .iter()
                    .find(|(b, _)| b == behavior)
                    .map(|(_, share)| *share)
            });
            day_of[at] = match (crunch, share) {
                (Some(crunch), Some(share)) if r.next() < share => crunch.day,
                _ => match crew.era.iter().find(|(b, _, _)| b == behavior) {
                    Some(&(_, from, to)) => pick_day(&mut r, &active, from, to, crew.idle),
                    None => pick_day(&mut r, &active, crew.idle, days - 1, crew.idle),
                },
            };
        }
        let labels: Vec<String> = (0..BEHAVIORS.len()).map(words).collect();
        let fits = |pattern: &str| Regex::new(pattern).expect("a made-up crew's pattern reads");
        let next: Vec<Vec<usize>> = labels
            .iter()
            .map(|said| {
                NEXT.iter()
                    .find(|(from, _)| fits(from).is_match(said))
                    .map(|(_, to)| {
                        let to = fits(to);
                        (0..labels.len())
                            .filter(|k| to.is_match(&labels[*k]))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        let (starter, loopy, ender) = (fits(STARTER), fits(LOOPY), fits(ENDER));
        let waits_before = crew.waits.map(|(pattern, share)| (fits(pattern), share));
        let sent = crew
            .sent
            .map(|(pattern, strength)| (fits(pattern), strength));
        let dropped = crew
            .dropped
            .map(|(pattern, strength)| (fits(pattern), strength));

        let mut minutes = vec![0f32; actions.len()];
        let mut sessions: Vec<Session> = Vec::new();
        let mut by_day: Vec<u32> = day_of.clone();
        by_day.sort_unstable_by(|a, b| b.cmp(a));
        by_day.dedup();
        for day in by_day {
            let mut pool = Pool::default();
            for (at, (behavior, _)) in actions.iter().enumerate() {
                if day_of[at] == day {
                    pool.put(*behavior, at);
                }
            }
            let mut opened = 0;
            while !pool.is_empty() {
                let in_crunch = crunch.is_some_and(|crunch| crunch.day == day);
                let fan = crunch.filter(|crunch| in_crunch && opened < crunch.sessions);
                let length = 6 + (r.next() * 20.0) as usize;
                let mut dealt = Vec::new();
                let mut current = match fan {
                    Some(crunch) if pool.has(crunch.opens_with) => crunch.opens_with,
                    _ => {
                        let starters: Vec<usize> = pool
                            .keys()
                            .filter(|b| starter.is_match(&labels[*b]))
                            .collect();
                        let from = if !starters.is_empty() && r.next() < 0.8 {
                            starters
                        } else {
                            pool.keys().collect()
                        };
                        pool.weighted(&from, &mut r)
                    }
                };
                while dealt.len() < length && !pool.is_empty() {
                    if !pool.has(current) {
                        let all: Vec<usize> = pool.keys().collect();
                        current = pool.weighted(&all, &mut r);
                    }
                    dealt.push(pool.take(current));
                    if loopy.is_match(&labels[current])
                        && pool.count(current) >= 4
                        && r.next() < crew.loops * 0.3
                    {
                        let run = 4 + (r.next() * 4.0) as usize;
                        for _ in 0..run {
                            if !pool.has(current) {
                                break;
                            }
                            dealt.push(pool.take(current));
                        }
                    }
                    if pool.is_empty() {
                        break;
                    }
                    if dealt.len() + 1 >= length && r.next() < 0.6 {
                        let enders: Vec<usize> = pool
                            .keys()
                            .filter(|b| ender.is_match(&labels[*b]))
                            .collect();
                        if !enders.is_empty() {
                            let last = pool.weighted(&enders, &mut r);
                            dealt.push(pool.take(last));
                            break;
                        }
                    }
                    let follows: Vec<usize> = next[current]
                        .iter()
                        .copied()
                        .filter(|b| pool.has(*b) && *b != current)
                        .collect();
                    current = if !follows.is_empty() && r.next() < 0.78 {
                        pool.weighted(&follows, &mut r)
                    } else {
                        let all: Vec<usize> = pool.keys().collect();
                        pool.weighted(&all, &mut r)
                    };
                }
                let start = if in_crunch {
                    14.0 * 60.0
                        + if fan.is_some() {
                            r.next() * 4.0
                        } else {
                            r.next() * 50.0
                        }
                } else if r.next() < crew.night {
                    22.0 * 60.0 + r.next() * 6.5 * 60.0
                } else {
                    9.0 * 60.0 + r.next() * 10.0 * 60.0
                };
                let mut minute: f64 = start;
                for (q, at) in dealt.iter().enumerate() {
                    if q > 0 {
                        minute += if actions[dealt[q - 1]].0 == actions[*at].0 {
                            0.15 + r.next() * 0.3
                        } else {
                            0.25 + r.next() * 1.3
                        };
                        if let Some((pattern, share)) = &waits_before
                            && pattern.is_match(&labels[actions[*at].0])
                            && waits.next() < *share
                        {
                            minute += 10.0 + waits.next() * 30.0;
                        }
                    }
                    minutes[*at] = minute as f32;
                }
                let share_of = |fits: &Option<(Regex, f64)>| {
                    fits.as_ref().map_or(0.0, |(pattern, _)| {
                        let matching = dealt
                            .iter()
                            .filter(|at| pattern.is_match(&labels[actions[**at].0]))
                            .count();
                        matching as f64 / dealt.len() as f64
                    })
                };
                let strength = |fits: &Option<(Regex, f64)>| fits.as_ref().map_or(1.1, |f| f.1);
                let p_sent = (0.05 + strength(&sent) * share_of(&sent)).min(0.85);
                let p_dropped = (0.04 + strength(&dropped) * share_of(&dropped)).min(0.85 - p_sent);
                let conversation = r.next() < 0.2;
                let x = r.next();
                let outcome = if conversation {
                    None
                } else if x < p_sent {
                    Some(Outcome::SentBack)
                } else if x < p_sent + p_dropped {
                    Some(Outcome::Dropped)
                } else {
                    Some(Outcome::Merged)
                };
                let y = r.next();
                let ended = if y < crew.crash {
                    Ending::Crashed
                } else if y < crew.crash + crew.stop {
                    Ending::Stopped
                } else {
                    Ending::Completed
                };
                sessions.push(Session {
                    actions: dealt,
                    outcome,
                    ended,
                });
                opened += 1;
            }
        }
        let failed_words = fits(FAILED);
        let fails = crew.fails.map(|(pattern, share)| (fits(pattern), share));
        let failed = actions
            .iter()
            .map(|(behavior, _)| {
                let said = &labels[*behavior];
                if failed_words.is_match(said) {
                    return true;
                }
                let share = fails
                    .as_ref()
                    .filter(|(pattern, _)| pattern.is_match(said))
                    .map_or(0.012, |(_, share)| *share);
                r.next() < share
            })
            .collect();
        // Now is a few minutes after the last day's last action, or 7:30 PM when it was quiet.
        let last_minute = minutes
            .iter()
            .zip(&day_of)
            .filter(|(_, day)| **day == 0)
            .map(|(minute, _)| f64::from(*minute))
            .reduce(f64::max);
        let now = f64::from((days - 1) * 1440) + last_minute.map_or(19.5 * 60.0, |last| last + 3.0);
        let minutes = minutes
            .iter()
            .zip(&day_of)
            .map(|(minute, day)| (f64::from((days - 1 - day) * 1440) + f64::from(*minute)) as f32)
            .collect();
        Self {
            minutes,
            sessions,
            failed,
            days,
            now,
        }
    }

    /// The crew's records: this period's actions in their sessions, the actions no label fits,
    /// last period's and the months' before.
    fn records(&self, crew: &Crew, actions: &[(usize, usize)]) -> MadeUp {
        let first_day = datetime!(2026-09-01 00:00 UTC);
        let at =
            |minutes: f64| first_day + Duration::milliseconds((minutes * 60_000.0).round() as i64);
        let end = at(self.now);
        let mut records = Vec::new();
        for (n, session) in self.sessions.iter().enumerate() {
            let (run, place) = match session.outcome {
                None => (None, format!("conversation-{n:03}")),
                Some(_) => (Some(format!("run-{n:03}")), format!("run-{n:03}")),
            };
            let Some(last) = session
                .actions
                .last()
                .map(|last| at(f64::from(self.minutes[*last])))
            else {
                continue;
            };
            for (q, index) in session.actions.iter().enumerate() {
                let (behavior, model) = actions[*index];
                let mut recorded = made_up(&place, "s-1", q, BEHAVIORS[behavior], model);
                recorded.run = run.clone();
                recorded.in_task = run.is_some();
                recorded.at = at(f64::from(self.minutes[*index]));
                recorded.failed = self.failed[*index];
                recorded.outcome = session.outcome;
                recorded.settled_at = session.outcome.map(|_| last);
                recorded.ended = Some(match session.ended {
                    Ending::Completed => SessionEnded::Completed,
                    Ending::Stopped => SessionEnded::Stopped,
                    Ending::Crashed => SessionEnded::Crashed,
                });
                recorded.ended_at = Some(last);
                records.push(recorded);
            }
        }
        // The actions no label fits, in a session of their own on the period's first busy day.
        let opening = self.sessions.first().and_then(|s| s.actions.first());
        if let Some(opening) = opening {
            let noon = (f64::from(self.minutes[*opening]) / 1440.0).floor() * 1440.0 + 12.0 * 60.0;
            for n in 0..crew.unlabelled {
                let mut recorded = made_up("conversation-unlabelled", "s-1", n, "unlabelled", 0);
                recorded.run = None;
                recorded.at = at(noon + n as f64);
                records.push(recorded);
            }
        }
        records.extend(before(crew, actions, first_day));
        MadeUp {
            records,
            period: Period::new(end, Some(i64::from(self.days)), WallClock::pinned(end)),
        }
    }
}

/// A behavior's day among `active`, between `from` and `to` days back when any is, later days
/// more likely.
fn pick_day(r: &mut Rng, active: &[u32], from: u32, to: u32, idle: u32) -> u32 {
    let within: Vec<u32> = active
        .iter()
        .copied()
        .filter(|day| (from..=to).contains(day))
        .collect();
    let list = if within.is_empty() {
        active.to_vec()
    } else {
        within
    };
    let weight = |day: u32| 1.0 / (1.0 + (f64::from(day) - f64::from(idle)) / 10.0);
    let total: f64 = list.iter().map(|day| weight(*day)).sum();
    let mut x = r.next() * total;
    for day in &list {
        x -= weight(*day);
        if x <= 0.0 {
            return *day;
        }
    }
    list[list.len() - 1]
}

/// A day's actions not yet dealt into a session, by behavior in the order first met.
#[derive(Default)]
struct Pool(Vec<(usize, Vec<usize>)>);

impl Pool {
    fn put(&mut self, behavior: usize, at: usize) {
        match self.0.iter_mut().find(|(b, _)| *b == behavior) {
            Some((_, ats)) => ats.push(at),
            None => self.0.push((behavior, vec![at])),
        }
    }

    fn has(&self, behavior: usize) -> bool {
        self.0.iter().any(|(b, _)| *b == behavior)
    }

    fn count(&self, behavior: usize) -> usize {
        self.0
            .iter()
            .find(|(b, _)| *b == behavior)
            .map_or(0, |(_, ats)| ats.len())
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn keys(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().map(|(b, _)| *b)
    }

    fn take(&mut self, behavior: usize) -> usize {
        let place = self
            .0
            .iter()
            .position(|(b, _)| *b == behavior)
            .expect("a behavior taken is in the pool");
        let at = self.0[place]
            .1
            .pop()
            .expect("a behavior in the pool has actions");
        if self.0[place].1.is_empty() {
            self.0.remove(place);
        }
        at
    }

    /// One of `from`, as likely as it has actions left.
    fn weighted(&self, from: &[usize], r: &mut Rng) -> usize {
        let total: usize = from.iter().map(|b| self.count(*b)).sum();
        let mut x = r.next() * total as f64;
        for behavior in from {
            x -= self.count(*behavior) as f64;
            if x <= 0.0 {
                return *behavior;
            }
        }
        from[from.len() - 1]
    }
}

/// Last period's actions, and those of the months before it: last period about as much of
/// each behavior as this one, and some of those it no longer does; before that the crew's
/// earlier actions month by month.
fn before(crew: &Crew, actions: &[(usize, usize)], first_day: OffsetDateTime) -> Vec<Recorded> {
    let mut count = vec![0usize; BEHAVIORS.len()];
    let mut model_of = vec![0usize; BEHAVIORS.len()];
    for (behavior, model) in actions.iter().rev() {
        count[*behavior] += 1;
        model_of[*behavior] = *model;
    }
    let mut records = Vec::new();
    let mut add = |behavior: usize, day: u32, n: usize| {
        let minutes = 10 * 60 + (n / 30) as i64;
        let mut recorded = made_up(
            &format!("run-before-{day}"),
            &format!("s-{behavior}"),
            n,
            BEHAVIORS[behavior],
            model_of[behavior],
        );
        recorded.at = first_day - Duration::days(i64::from(day) - 29) + Duration::minutes(minutes);
        recorded.ended = Some(SessionEnded::Completed);
        records.push(recorded);
    };
    let mut last_period = 0;
    for (behavior, done) in count.iter().copied().enumerate() {
        let no_longer = crew.before.contains(&behavior) && done == 0;
        let (n, from) = match behavior {
            GIT_CHANGE if no_longer => (130, 34),
            _ if no_longer => (12, 30),
            _ => ((0.92 * done as f64).round() as usize, 30),
        };
        for k in 0..n {
            add(behavior, from + (k as u32 % (60 - from)), k);
        }
        last_period += n;
    }
    let mut r = Rng(7919 + crew.seed);
    let per_month = (crew.per_day * 30.0).max(30.0);
    let months = ((f64::from(crew.earlier) / per_month).ceil() as usize).clamp(1, 12);
    let weights: Vec<f64> = (0..months).map(|_| 0.55 + 0.9 * r.next()).collect();
    let rest = (crew.earlier as usize).saturating_sub(last_period);
    let older: f64 = weights[1..].iter().sum();
    let mut left = rest;
    let busy: Vec<usize> = (0..BEHAVIORS.len()).filter(|b| count[*b] > 0).collect();
    for (k, weight) in weights.iter().enumerate().skip(1) {
        let n = if k == months - 1 {
            left
        } else {
            ((rest as f64 * weight / older).round() as usize).min(left)
        };
        left -= n;
        for j in 0..n {
            add(
                busy[j % busy.len()],
                30 * (k as u32 + 1) + (j as u32 % 30),
                j,
            );
        }
    }
    records
}

fn made_up(place: &str, session: &str, n: usize, behavior: &str, model: usize) -> Recorded {
    let (cli, model) = MODELS[model];
    let session = format!("harbor-0123456789ab/{place}/{session}");
    Recorded {
        key: format!("{session}/call-{n}"),
        session,
        run: Some(place.to_owned()),
        cli: cli.to_owned(),
        model: Some(model.to_owned()),
        role: "builder".to_owned(),
        agent: "builder".to_owned(),
        workflow: "no workflow".to_owned(),
        project: "harbor".to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
        nth: n + 1,
        of: 0,
        tool: "Bash".to_owned(),
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
