//! A made-up year for the Overview's tests: Claude Code imported since October 2025, Codex
//! since February, Antigravity CLI only through Argonauts since June.

use argonauts_core::overview::{
    CliSource, ImportStatus, OverviewData, OverviewDay, OverviewModel, OverviewMonth,
    OverviewPeriod, OverviewWeek, ThroughArgonauts, Tokens,
};
use time::macros::date;
use time::{Date, Duration};

pub fn today() -> Date {
    date!(2026 - 10 - 08)
}

/// `total` split as a long session's tokens are: nearly all input from the cache.
fn tokens(total: u64) -> Tokens {
    Tokens {
        input: total * 2 / 100,
        output: total * 3 / 100,
        cache_read: total * 94 / 100,
        cache_write: total - total * 2 / 100 - total * 3 / 100 - total * 94 / 100,
    }
}

fn model(agent: &str, model: &str, billions: f64, usd: f64, sessions: u32) -> OverviewModel {
    OverviewModel {
        agent: agent.to_owned(),
        model: Some(model.to_owned()),
        tokens: tokens((billions * 1e9) as u64),
        api_value: Some(usd),
        sessions,
    }
}

/// A day's tokens by CLI, from a fixed little generator so the year looks lived in.
fn day_tokens(day: Date) -> [u64; 3] {
    let n = (day.to_julian_day() as u64).wrapping_mul(2_654_435_761) % 1000;
    if n < 130 {
        return [0; 3];
    }
    let claude = 150_000_000 + n * 450_000;
    let codex = if day >= date!(2026 - 02 - 01) {
        60_000_000 + (n * 7 % 1000) * 250_000
    } else {
        0
    };
    let agy = if day >= date!(2026 - 06 - 01) {
        10_000_000 + (n * 13 % 1000) * 70_000
    } else {
        0
    };
    [claude, codex, agy]
}

const CLIS: [(&str, &str); 3] = [
    ("claude-code", "claude-opus-5-5"),
    ("codex", "gpt-5.6"),
    ("antigravity-cli", "gemini-3.6-pro"),
];

pub fn overview() -> OverviewData {
    let today = today();
    let mut data = OverviewData::empty(OverviewPeriod::All, today);
    data.import = ImportStatus::Done;
    data.sources = vec![
        CliSource {
            agent: "claude-code".into(),
            since: Some(date!(2025 - 10 - 09)),
            imported: true,
            sessions: 812,
        },
        CliSource {
            agent: "codex".into(),
            since: Some(date!(2026 - 02 - 03)),
            imported: true,
            sessions: 221,
        },
        CliSource {
            agent: "antigravity-cli".into(),
            since: Some(date!(2026 - 06 - 02)),
            imported: false,
            sessions: 53,
        },
    ];
    data.models = vec![
        model("claude-code", "claude-opus-5-5", 54.5, 5_910.0, 584),
        model("codex", "gpt-5.6", 26.0, 1_560.0, 163),
        model("claude-code", "claude-sonnet-5-5", 15.4, 1_180.0, 201),
        model("antigravity-cli", "gemini-3.6-pro", 8.3, 420.0, 31),
        model("codex", "gpt-5.6-luna", 7.1, 190.0, 58),
        model("claude-code", "claude-opus-5", 3.6, 320.0, 27),
        model("antigravity-cli", "gemini-3.6-flash", 3.5, 60.0, 22),
    ];
    for model in &data.models {
        data.total.add(&model.tokens);
    }
    data.api_value = 9_640.0;
    data.recent = 11_200_000_000;
    data.before = 10_000_000_000;
    data.active_days = 318;
    data.span_days = 365;
    data.streak = 18;
    data.longest_streak = 64;
    data.hours = [
        8, 4, 2, 1, 1, 2, 5, 12, 30, 52, 61, 58, 40, 55, 70, 74, 66, 60, 48, 52, 71, 88, 64, 30,
    ]
    .map(|share| share * 100_000_000)
    .to_vec();
    for back in (0..371).rev() {
        let day = today - Duration::days(back);
        let parts = day_tokens(day);
        let total: u64 = parts.iter().sum();
        if total == 0 {
            continue;
        }
        let main = (0..3).max_by_key(|&k| parts[k]).unwrap_or(0);
        data.days.push(OverviewDay {
            day,
            tokens: total,
            agent: CLIS[main].0.to_owned(),
            model: Some(CLIS[main].1.to_owned()),
        });
    }
    let monday = today - Duration::days(i64::from(today.weekday().number_days_from_monday()));
    for back in (0..26).rev() {
        let start = monday - Duration::weeks(back);
        let mut by_cli = [0u64; 3];
        for offset in 0..7 {
            let parts = day_tokens(start + Duration::days(offset));
            for k in 0..3 {
                by_cli[k] += parts[k];
            }
        }
        data.weeks.push(OverviewWeek {
            start,
            by_cli: (0..3)
                .filter(|&k| by_cli[k] > 0)
                .map(|k| (CLIS[k].0.to_owned(), by_cli[k]))
                .collect(),
        });
    }
    data.folders = vec![
        ("/Users/you/code/argonauts-code".into(), 52_100_000_000),
        ("/Users/you/code/minitorch".into(), 23_400_000_000),
        ("/Users/you/code/bytesplit".into(), 14_200_000_000),
    ];
    for k in 0..9 {
        data.folders
            .push((format!("/Users/you/code/other-{k}"), 3_188_000_000));
    }
    data.argonauts = Some(ThroughArgonauts {
        since: Some(date!(2026 - 06 - 02)),
        workflows: 14,
        by_workflow: vec![
            ("council".into(), 8),
            ("orchestrator".into(), 5),
            ("tidy-docs".into(), 1),
        ],
        merged: 63,
        first_pass: Some(0.92),
        fallbacks: 4,
        for_limit: 3,
        answered: 11,
    });
    data.subagent_tokens = data.total.total() * 64 / 100;
    data.longest_session_secs = 57 * 24 * 3600 + 3_000;
    data.months = vec![
        OverviewMonth {
            month: date!(2026 - 03 - 01),
            by_cli: vec![("claude-code".into(), 86), ("codex".into(), 14)],
        },
        OverviewMonth {
            month: date!(2026 - 09 - 01),
            by_cli: vec![
                ("claude-code".into(), 56),
                ("codex".into(), 34),
                ("antigravity-cli".into(), 10),
            ],
        },
    ];
    data
}
