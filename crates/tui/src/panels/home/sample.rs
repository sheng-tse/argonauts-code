//! The example the Overview's pictures draw: Claude Code since October 2025, Codex since
//! February, agy since June, 118.4b tokens in all, and the year of days and the
//! three lines of the Models chart exactly as the pictures have them.

use argonauts_core::app::overview::{
    CliSource, ImportStatus, ModelDay, OverviewData, OverviewDay, OverviewModel, OverviewPeriod,
    OwnCount, Tokens,
};
use time::macros::date;
use time::{Date, Duration};

/// The Overview picture's today: a Friday, so its last week ends in a blank Saturday.
pub fn today() -> Date {
    date!(2026 - 10 - 02)
}

/// The Models picture's today, the day after the chart's last day.
pub fn models_today() -> Date {
    date!(2026 - 10 - 09)
}

/// Millions of tokens as the CLIs count them.
fn tokens(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Tokens {
    let m = 1_000_000;
    Tokens {
        input: input * m,
        output: output * m,
        cache_read: cache_read * m,
        cache_write: cache_write * m,
    }
}

fn model(agent: &str, model: &str, tokens: Tokens, usd: f64, sessions: u32) -> OverviewModel {
    OverviewModel {
        agent: agent.to_owned(),
        model: Some(model.to_owned()),
        tokens,
        api_value: Some(usd),
        sessions,
    }
}

fn source(agent: &str, since: Date, sessions: u32) -> CliSource {
    CliSource {
        agent: agent.to_owned(),
        since: Some(since),
        imported: true,
        sessions,
    }
}

/// The Overview picture's numbers.
pub fn overview() -> OverviewData {
    let today = today();
    let mut data = OverviewData::empty(OverviewPeriod::All, today);
    data.import = ImportStatus::Done;
    data.sources = vec![
        source("claude-code", date!(2025 - 10 - 09), 812),
        source("codex", date!(2026 - 02 - 03), 221),
        source("antigravity-cli", date!(2026 - 06 - 02), 53),
    ];
    data.models = vec![
        model(
            "claude-code",
            "claude-opus-5-5",
            tokens(600, 200, 51_100, 2_600),
            5_910.0,
            584,
        ),
        model(
            "codex",
            "gpt-5.6",
            tokens(300, 100, 24_300, 1_300),
            1_560.0,
            163,
        ),
        model(
            "claude-code",
            "claude-sonnet-5-5",
            tokens(200, 50, 14_400, 800),
            1_180.0,
            201,
        ),
        model(
            "antigravity-cli",
            "gemini-3.6-pro",
            tokens(100, 20, 7_700, 500),
            420.0,
            31,
        ),
        model(
            "codex",
            "gpt-5.6-luna",
            tokens(20, 10, 6_400, 670),
            190.0,
            58,
        ),
        model(
            "claude-code",
            "claude-haiku-4-5",
            tokens(10, 10, 3_000, 530),
            320.0,
            27,
        ),
        model(
            "antigravity-cli",
            "gemini-3.6-flash",
            tokens(10, 10, 3_060, 400),
            60.0,
            22,
        ),
    ];
    for model in &data.models {
        data.total.add(&model.tokens);
    }
    data.api_value = data.models.iter().filter_map(|model| model.api_value).sum();
    data.active_days = 318;
    data.span_days = 365;
    data.streak = 18;
    data.longest_streak = 64;
    data.peak_day = Some(date!(2026 - 08 - 28));
    data.hours = [
        8, 4, 2, 1, 1, 2, 5, 12, 30, 52, 61, 58, 40, 55, 70, 74, 66, 60, 48, 52, 71, 88, 64, 30,
    ]
    .map(|share| share * 100_000_000)
    .to_vec();
    data.days = year(today);
    data.folders = vec![
        ("/Users/you/code/argonauts-code".into(), 52_100_000_000),
        ("/Users/you/code/minitorch".into(), 23_400_000_000),
        ("/Users/you/code/bytesplit".into(), 14_200_000_000),
    ];
    data.subagent_tokens = data.total.total() * 64 / 100;
    data.longest_session_secs = 57 * 86_400 + 3 * 3600 + 20 * 60;
    data
}

/// The numbers picture's example under the Overview picture's year of days: Claude Code's own
/// count of 60.6b beside the 69.1b counted here, 147 days of history, and tokens that are
/// nearly all read from cache, most of them Claude Code's.
pub fn numbers() -> OverviewData {
    let mut data = overview();
    let ran = |agent: &str, model: &str, tokens: u64| OverviewModel {
        agent: agent.to_owned(),
        model: Some(model.to_owned()),
        tokens: Tokens {
            cache_read: tokens,
            ..Tokens::default()
        },
        api_value: None,
        sessions: 0,
    };
    data.models = vec![
        ran("claude-code", "claude-opus-5-5", 42_151_000_000),
        ran("claude-code", "claude-sonnet-5-5", 18_657_000_000),
        ran("codex", "gpt-5.6", 8_015_600_000),
        ran("antigravity-cli", "gemini-3.6-pro", 276_400_000),
    ];
    data.total = Tokens {
        input: 69_100_000,
        output: 207_300_000,
        cache_read: 67_165_200_000,
        cache_write: 1_658_400_000,
    };
    data.claude_stats = Some(OwnCount {
        tokens: 60_600_000_000,
        through: data.today - Duration::days(1),
    });
    data.api_value = 32_540.0;
    data.active_days = 122;
    data.span_days = 147;
    data.streak = 20;
    data.longest_streak = 41;
    for (source, sessions) in data.sources.iter_mut().zip([300, 70, 14]) {
        source.sessions = sessions;
    }
    data.subagent_tokens = data.total.total() * 64 / 100;
    data.longest_session_secs = 57 * 86_400 + 23 * 3600 + 47 * 60;
    data
}

/// The numbers picture's example where some of the period's tokens were free or paid: most on
/// plans, some on a model on this computer and a sliver on a key, at list prices.
pub fn numbers_who_paid() -> OverviewData {
    let mut data = numbers();
    let total = data.total.total();
    let free = total * 7 / 100;
    let paid = total * 2 / 100;
    data.who_paid = Some(argonauts_core::models::meter::WhoPaid {
        plan: total - free - paid,
        free,
        paid,
        paid_usd: 41.20,
        cost_partial: false,
        cost_estimated: true,
    });
    data
}

/// The Models picture's numbers: the same, with the chart's 52 days up to the day before.
pub fn models() -> OverviewData {
    let mut data = overview();
    data.today = models_today();
    let first = date!(2026 - 08 - 18);
    data.model_days = (0..52)
        .map(|at| ModelDay {
            day: first + Duration::days(at as i64),
            tokens: vec![OPUS_DAYS[at], GPT_DAYS[at], SONNET_DAYS[at]],
        })
        .collect();
    data
}

/// The picture's year of days: each a CLI's letter (`c` Claude Code, `x` Codex, `a` agy) and
/// its quarter, `.` for a day with nothing and a blank after today. Each quarter's days carry
/// the same tokens, so the quartiles fall between them.
fn year(today: Date) -> Vec<OverviewDay> {
    let this_sunday = today - Duration::days(i64::from(today.weekday().number_days_from_sunday()));
    let first = this_sunday - Duration::weeks(51);
    let mut days = Vec::new();
    for (row, cells) in YEAR.iter().enumerate() {
        let mut week = 0;
        let mut chars = cells.chars().peekable();
        while let Some(letter) = chars.next() {
            let day = first + Duration::days((week * 7 + row) as i64);
            week += 1;
            let agent = match letter {
                'c' => ("claude-code", "claude-opus-5-5"),
                'x' => ("codex", "gpt-5.6"),
                'a' => ("antigravity-cli", "gemini-3.6-pro"),
                _ => continue,
            };
            let level = chars.next().and_then(|c| c.to_digit(10)).unwrap_or(1);
            days.push(OverviewDay {
                day,
                tokens: u64::from(level) * 100_000_000,
                agent: agent.0.to_owned(),
                model: Some(agent.1.to_owned()),
            });
        }
    }
    days.sort_by_key(|day| day.day);
    days
}

const YEAR: [&str; 7] = [
    ".c2..c2.c3.c1c2c3c1.c3.c1c2c1.c2c4c3c3.c3c4c3c2x4c2c2x4c3x2x3a4c4c4x4x2.x3c4x2a3x3c2c2.x4a4.",
    "c3c3c3c1c3c1c1.c1c1c2c3c1c1c3c3c2c2c1c2c4c3c3c4.c4c4c3c4c4c3x3c4c2c3c2x4c2c3.x2.c3c2c4c3.x4c3x2c3x3",
    ".c2c2.c1c2.c3.c3c3c2c3c2c3c2c2c3x3c1c3c4x2c4c4c2c4x3c2c3c3c2x3c4x4c3c3x3c4.a4a2a4c3c3x2x3c3x2x3..",
    "..c2c2..c1.c3c2c2c1c2c3c1c2c1c2x3c2.c4c4c3x4c3c3c4c3c4c3c2x3c4.x3.a4x4c2x4x2c3c4c2c3c2c4c3c4c3c4",
    "c2c2.c2.c3c2..c1.c2c2c3c1c1c2c1x3c1c3c4c4c3c3c4x3c3c3c4c3..c2c3c4c3c4x3c4.c3c3c2c2c2.x3c3c4x2c3",
    ".c3.c2.c2c2c2c1c2c2c2c2c2c2c2c2c1c1.c4c4x2c2c4c4c4c4c4c4c4c2c3c4c4x3c3c4a3c3c2c4x2x2c3c4c2c3..x3x4",
    "c2.c3c1c2c2c1.c1.c2.c1..c1c2c1x2c1c3...c2c3x2c4c4x4c4c4x2x4.c2.x3.c3a4.c4.c2.c3x4a4x4c4 ",
];
const OPUS_DAYS: [u64; 52] = [
    358_385_564,
    425_123_637,
    476_803_068,
    436_915_493,
    440_293_808,
    435_402_200,
    466_888_935,
    523_059_138,
    454_153_864,
    373_456_410,
    438_094_128,
    430_192_201,
    481_602_615,
    396_181_705,
    390_551_400,
    434_628_606,
    390_005_806,
    474_354_531,
    550_811_473,
    470_517_670,
    389_297_925,
    400_952_170,
    484_199_020,
    467_015_782,
    420_203_674,
    410_384_657,
    329_811_999,
    283_916_499,
    276_936_266,
    280_382_469,
    236_537_670,
    192_293_648,
    145_874_235,
    142_802_858,
    126_000_000,
    126_000_000,
    190_964_036,
    205_325_814,
    235_138_799,
    182_801_927,
    275_659_741,
    344_650_116,
    280_610_309,
    254_695_442,
    298_762_636,
    340_977_154,
    423_736_460,
    413_915_720,
    477_522_145,
    512_377_147,
    481_183_479,
    501_147_988,
];
const GPT_DAYS: [u64; 52] = [
    296_144_798,
    361_140_646,
    301_357_838,
    245_539_917,
    294_809_760,
    330_145_558,
    356_207_815,
    331_646_919,
    348_779_102,
    366_031_345,
    379_699_907,
    334_173_509,
    326_767_259,
    314_161_713,
    347_683_405,
    419_258_144,
    484_473_510,
    492_958_296,
    487_537_883,
    457_391_587,
    394_720_993,
    330_863_273,
    328_248_413,
    305_133_531,
    290_635_620,
    347_786_145,
    353_691_532,
    364_462_983,
    329_820_260,
    265_460_391,
    243_280_401,
    194_718_036,
    198_449_374,
    270_565_074,
    297_292_231,
    255_050_321,
    312_450_336,
    356_296_725,
    391_412_962,
    450_636_073,
    489_740_041,
    532_604_710,
    514_434_887,
    584_071_607,
    651_037_738,
    605_903_590,
    643_764_160,
    676_185_286,
    673_082_223,
    679_632_024,
    680_533_973,
    742_310_463,
];
const SONNET_DAYS: [u64; 52] = [
    107_716_816,
    112_997_447,
    102_593_412,
    113_246_215,
    125_861_043,
    88_058_640,
    45_543_759,
    77_215_977,
    56_857_838,
    39_000_000,
    84_908_035,
    83_531_751,
    115_113_281,
    114_285_070,
    128_101_203,
    97_956_681,
    111_394_140,
    145_818_218,
    149_204_527,
    172_217_194,
    188_944_227,
    151_007_056,
    175_547_778,
    185_046_741,
    168_460_830,
    127_551_888,
    161_749_339,
    160_596_757,
    181_590_910,
    216_984_062,
    237_555_716,
    276_754_596,
    268_601_302,
    296_983_092,
    293_298_987,
    333_801_792,
    369_199_791,
    334_270_679,
    302_807_876,
    278_636_701,
    321_829_914,
    317_384_482,
    330_082_828,
    313_475_186,
    315_427_054,
    306_455_017,
    294_336_961,
    303_293_631,
    312_176_292,
    349_854_452,
    367_532_844,
    407_437_948,
];
