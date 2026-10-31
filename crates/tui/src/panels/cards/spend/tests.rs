use argonauts_core::behaviors::crew_stats::{CrewStats, ReviewTally, StatsRole, WorkRow};
use argonauts_core::models::model_catalog::{CatalogEntry, ModelCatalog};
use argonauts_core::runs::api::{DaySpend, PlanUsage, UsageShare};
use time::macros::{date, datetime};

use super::*;
use crate::base::kit::{Form, Frame};
use crate::base::text::plain_text;
use crate::base::theme::{ColorLevel, Glyphs, Theme};
use crate::testing::pictures::golden;
use crate::{
    panels::{UsageCard, UsageCards},
    testing::sample,
};

/// A Saturday, the last day of a week that starts on Sunday, so the week has no day ahead.
const TODAY: Date = date!(2026 - 10 - 10);

/// Names that know `haiku` as Claude Code's alias for Haiku 5.5, which is also the model
/// its settings give it.
fn names() -> DisplayNames {
    let mut haiku = CatalogEntry::new("claude-code/claude-haiku-5-5".parse().unwrap());
    haiku.aliases = Some(vec!["haiku".to_owned()]);
    DisplayNames {
        catalog: ModelCatalog {
            models: vec![haiku],
            ..ModelCatalog::default()
        },
        configured: [("claude-code".to_owned(), "haiku".to_owned())].into(),
        ..sample::names()
    }
}

fn spent(usd: f64, tokens: u64, estimated: bool) -> Spend {
    Spend {
        input_tokens: tokens,
        cost_usd: usd,
        cost_estimated: estimated,
        ..Spend::default()
    }
}

/// A share with what it spent on each day of the week of [`TODAY`], Sunday first.
fn share(agent: &str, model: Option<&str>, spent: Spend, week: [f64; 7]) -> UsageShare {
    let sunday = TODAY - Duration::days(6);
    UsageShare {
        agent: agent.to_owned(),
        model: model.map(str::to_owned),
        agent_id: agent.to_owned(),
        spent,
        pays_per_token: false,
        local: false,
        billing: None,
        this_week: week
            .iter()
            .enumerate()
            .filter(|(_, usd)| **usd > 0.0)
            .map(|(at, usd)| DaySpend {
                day: sunday + Duration::days(at as i64),
                spent: self::spent(*usd, 0, false),
            })
            .collect(),
    }
}

fn work(agent: &str, model: &str, role: StatsRole, done: u32) -> WorkRow {
    WorkRow {
        agent: agent.to_owned(),
        model: Some(model.to_owned()),
        role,
        kind: None,
        spent: Spend::default(),
        done,
        sent_back: 0,
        took_secs: 0,
        review: ReviewTally::default(),
        sessions: 0,
        tool_calls: 0,
        turns: 0,
    }
}

/// The 14 days up to [`TODAY`], which spent `today`.
fn history(today: f64) -> Vec<DaySpend> {
    (0..14)
        .rev()
        .map(|back| DaySpend {
            day: TODAY - Duration::days(back),
            spent: spent(if back == 0 { today } else { 0.0 }, 0, false),
        })
        .collect()
}

/// The picture's week: Haiku 5.5 recorded three ways, as a run's alias, as a conversation's
/// full id and with no model at all, and GPT 5.6 Luna priced by estimate.
///
/// The picture's small bars can't come from its sums: GPT 5.6 Luna's seven days would come to
/// ten cents at least where it spent two, and today's bar is twelve cents where the header
/// says four. So each model is given the days that draw its bars, apart from what it spent.
fn the_pictures_week() -> UsageSummary {
    let no_days = [0.0; 7];
    UsageSummary {
        period: UsagePeriod::Week,
        total: spent(0.26, 3_500_000, true),
        by_agent: vec![
            share(
                "claude-code",
                Some("haiku"),
                spent(0.19, 2_700_000, false),
                [0.015, 0.015, 0.015, 0.015, 0.03, 0.045, 0.12],
            ),
            share(
                "claude-code",
                Some("claude-haiku-5-5"),
                spent(0.03, 400_000, false),
                no_days,
            ),
            share(
                "codex",
                Some("gpt-5.6-luna"),
                spent(0.02, 100_000, true),
                [0.015, 0.015, 0.015, 0.015, 0.015, 0.03, 0.045],
            ),
            share("claude-code", None, spent(0.02, 300_000, false), no_days),
        ],
        history: history(0.04),
        stats: Some(CrewStats {
            work: vec![
                work("claude-code", "haiku", StatsRole::Worker, 3),
                work("claude-code", "haiku", StatsRole::Reviewer, 9),
                work("claude-code", "claude-haiku-5-5", StatsRole::Reviewer, 4),
                work("codex", "gpt-5.6-luna", StatsRole::Reviewer, 3),
            ],
            ..CrewStats::default()
        }),
        ..sample::usage()
    }
}

fn card(usage: &UsageSummary, kit: &Kit<'_>, form: Form, screen: usize) -> Drawn {
    let names = names();
    let cards = UsageCards {
        usage,
        names: &names,
        now: datetime!(2026-10-10 13:00 UTC),
        left: false,
    };
    cards.draw(kit, UsageCard::Spend, form, Frame::view(screen))
}

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The rows of the card's table that name a model, each starting at its swatch: two cells of
/// the model's shape and two blanks.
fn model_rows(rows: &[String]) -> Vec<&String> {
    rows.iter()
        .filter(|row| {
            let cells: Vec<char> = row
                .strip_prefix(" │ ")
                .unwrap_or_default()
                .chars()
                .collect();
            matches!(cells.as_slice(), [a, b, ' ', ' ', ..] if a == b && "█▚⣿░".contains(*a))
        })
        .collect()
}

fn rows(usage: &UsageSummary, form: Form, screen: usize) -> Vec<String> {
    let theme = plain();
    let drawn = card(usage, &Kit::new(&theme), form, screen);
    plain_text(&drawn.lines)
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

#[test]
fn a_model_recorded_three_ways_is_one_row_and_no_cli_has_a_row_to_itself() {
    let rows = rows(&the_pictures_week(), Form::Full, 120);
    let models = model_rows(&rows);
    assert_eq!(models.len(), 2, "{rows:#?}");
    // The week of every name it was recorded under, on one scale with the other model's.
    assert!(
        models[0].starts_with(
            " │ ██  Haiku 5.5       Claude Code    $0.24   92%    3.40M   ▌   ▌   ▌   ▌   █   █   ███ "
        ),
        "{rows:#?}"
    );
    assert!(
        models[1].starts_with(
            " │ ██  GPT 5.6 Luna    Codex         ~$0.02    8%    0.10M   ▌   ▌   ▌   ▌   ▌   █   █   "
        ),
        "{rows:#?}"
    );
}

/// Where nothing says which model a CLI runs, its unnamed spend goes to the model of that
/// CLI that spent the most, and a CLI with no named model keeps the row as its default.
#[test]
fn spend_with_no_model_goes_to_its_clis_model_or_reads_as_the_default() {
    let mut usage = the_pictures_week();
    usage.by_agent = vec![
        share("codex", Some("gpt-5.6"), spent(0.50, 1000, false), [0.0; 7]),
        share(
            "codex",
            Some("gpt-5.6-luna"),
            spent(0.20, 1000, false),
            [0.0; 7],
        ),
        share("codex", None, spent(0.10, 1000, false), [0.0; 7]),
        share("antigravity-cli", None, spent(0.05, 1000, false), [0.0; 7]),
    ];
    usage.total = spent(0.85, 4000, false);
    let rows = rows(&usage, Form::Full, 120);
    let models = model_rows(&rows);
    assert_eq!(models.len(), 3, "{rows:#?}");
    assert!(models[0].contains("██  GPT 5.6 ") && models[0].contains("$0.60"));
    assert!(models[1].contains("▚▚  GPT 5.6 Luna") && models[1].contains("$0.20"));
    assert!(
        models[2].starts_with(" │ ██  default model    agy     $0.05"),
        "{rows:#?}"
    );
}

#[test]
fn a_narrow_card_gives_up_columns_from_the_right_and_keeps_each_models_spend() {
    let usage = the_pictures_week();
    let heads = |screen: usize| -> String {
        let rows = rows(&usage, Form::Full, screen);
        let head = rows.iter().find(|row| row.contains(" model "));
        let head = head.unwrap_or_else(|| panic!("{rows:#?}"));
        head.trim_matches(|c| c == '│' || c == ' ').to_owned()
    };
    let words = |screen: usize| -> Vec<String> {
        heads(screen)
            .split("  ")
            .map(|word| word.trim().to_owned())
            .filter(|word| !word.is_empty())
            .collect()
    };
    let days = ["S", "M", "T", "W", "T", "F", "S"];
    let with = |heads: &[&str], days: &[&str]| -> Vec<String> {
        heads
            .iter()
            .chain(days)
            .map(|word| (*word).to_owned())
            .collect()
    };
    assert_eq!(words(120), with(&["model", "on", "spend", "tokens"], &days));
    // Short of room the days narrow to a cell each, their letters a blank apart.
    assert_eq!(
        words(90),
        with(&["model", "on", "spend", "tokens", "S M T W T F S"], &[])
    );
    // A letter now starts its day, so it keeps a column of words' room from the tokens.
    assert!(heads(90).contains("tokens    S M"), "{}", heads(90));
    assert_eq!(words(70), ["model", "on", "spend", "tokens"]);
    assert_eq!(words(55), ["model", "spend", "tokens"]);
    // The share has no head of its own, as on Stats.
    assert_eq!(words(40), ["model", "spend"]);
    for screen in [120, 100, 90, 70, 55, 40, 30] {
        let rows = rows(&usage, Form::Full, screen);
        for row in &rows {
            assert_eq!(row.chars().count(), screen - 1, "{screen}: {row}");
        }
        assert!(rows.iter().any(|row| row.contains("$0.24")), "{rows:#?}");
    }
}

/// A font that draws a square or a bar wider than its cell runs it into whatever touches it,
/// so at every width a blank stands between each of them and the words beside it.
#[test]
fn no_word_touches_the_bar_a_square_or_a_days_small_bar() {
    let theme = golden::theme();
    let kit = Kit::new(&theme);
    for screen in [120, 100, 80, 60, 40] {
        let drawn = card(&the_pictures_week(), &kit, Form::Full, screen);
        for row in plain_text(&drawn.lines).lines() {
            assert_eq!(
                crate::base::kit::bars::touching_bar(row),
                None,
                "{screen}: {row}"
            );
        }
    }
}

#[test]
fn the_header_puts_today_beside_the_period_and_counts_it_toward_the_days_limit() {
    let header = |usage: &UsageSummary| rows(usage, Form::Compact, 120)[1].clone();
    let week = the_pictures_week();
    assert!(header(&week).starts_with(" │ ◆ spend  this week "));
    assert!(header(&week).ends_with("~$0.26 · no limit set · $0.04 today │"));
    let limited = UsageSummary {
        spend_limit: Some(0.5),
        ..week.clone()
    };
    assert!(
        header(&limited).ends_with("~$0.26 · $0.04 of $0.50 today   [8%] │"),
        "{}",
        header(&limited)
    );
    // When the period is today, its spend is the day's.
    let today = UsageSummary {
        period: UsagePeriod::Today,
        ..week.clone()
    };
    assert!(header(&today).starts_with(" │ ◆ spend  today "));
    assert!(header(&today).ends_with("~$0.26 · no limit set │"));
    let today = UsageSummary {
        spend_limit: Some(0.5),
        ..today
    };
    assert!(
        header(&today).ends_with("~$0.26 of $0.50 today  [52%] │"),
        "{}",
        header(&today)
    );
    // A narrower header says less of the limit and of today, then gives up the period's
    // words, and keeps what the period spent.
    let at = |screen: usize| rows(&week, Form::Compact, screen)[1].clone();
    assert_eq!(
        at(60),
        " │ ◆ spend  this week               ~$0.26 · no limit set │"
    );
    assert_eq!(at(40), " │ ◆ spend  this week          ~$0.26 │");
    assert_eq!(at(24), " │ ◆ spend     ~$0.26 │");
}

/// Today's bar is the day's limit, with what's unspent left as track. Any other period's bar
/// is the period's spend.
#[test]
fn against_a_days_limit_todays_bar_leaves_the_unspent_as_track() {
    let theme = golden::theme();
    let kit = Kit::new(&theme);
    let bar = |usage: &UsageSummary| -> Vec<(usize, Option<ratatui::style::Color>)> {
        let drawn = card(usage, &kit, Form::Compact, 120);
        let spans = drawn.lines[2].spans.iter();
        // A model's part in its texture, the track as a line.
        spans
            .filter(|span| span.content.contains(['█', '━']))
            .map(|span| (span.content.chars().count(), span.style.fg))
            .collect()
    };
    let (claude, codex) = (theme.color(Role::CliClaude), theme.color(Role::CliCodex));
    let week = UsageSummary {
        spend_limit: Some(0.52),
        ..the_pictures_week()
    };
    assert_eq!(bar(&week), [(105, claude), (9, codex)]);
    let today = UsageSummary {
        period: UsagePeriod::Today,
        ..week
    };
    let track = theme.color(Role::Faint);
    assert_eq!(bar(&today), [(53, claude), (4, codex), (57, track)]);
}

#[test]
fn spend_on_an_api_key_is_told_from_spend_inside_a_plan() {
    let mut usage = the_pictures_week();
    let mut router = share(
        "openrouter",
        Some("qwen3-coder"),
        spent(0.74, 900_000, false),
        [0.0; 7],
    );
    router.pays_per_token = true;
    usage.by_agent.push(router);
    usage.total = spent(1.00, 4_400_000, true);
    let rows = rows(&usage, Form::Full, 120);
    let foot = &rows[rows.len() - 2];
    assert_eq!(
        foot.trim_end_matches('│').trim_end(),
        " │ on plans ~$0.26 (inside your plans, not charged) · on API keys $0.74 · ~ estimated \
         at list prices"
    );
    // A plan whose name isn't known is still a plan, and an estimate is explained only where
    // one shows.
    usage.plans = vec![PlanUsage {
        agent: "claude-code".to_owned(),
        ..PlanUsage::default()
    }];
    usage.by_agent.retain(|share| share.agent == "claude-code");
    let rows = self::rows(&usage, Form::Full, 120);
    let foot = &rows[rows.len() - 2];
    assert_eq!(
        foot.trim_end_matches('│').trim_end(),
        " │ on plans $0.24 (inside your plans, not charged) · on API keys $0.00"
    );
}

/// The foot says `your plans` at every width, however many plans the rows are on.
#[test]
fn the_foot_says_your_plans_at_every_width() {
    let mut usage = the_pictures_week();
    let mut router = share(
        "openrouter",
        Some("qwen3-coder"),
        spent(0.74, 900_000, false),
        [0.0; 7],
    );
    router.pays_per_token = true;
    usage.by_agent.push(router);
    usage.total = spent(1.00, 4_400_000, true);
    for cols in [104, 120, 177, 265] {
        let rows = rows(&usage, Form::Full, cols);
        let foot = rows[rows.len() - 2].trim_end_matches('│').trim_end();
        assert_eq!(
            foot,
            " │ on plans ~$0.26 (inside your plans, not charged) · on API keys $0.74 · ~ \
             estimated at list prices",
            "at {cols}"
        );
    }
}

/// A model on this machine charges nothing and no plan covers it: its row reads a dim `local`,
/// and the foot gives local spend a part of its own.
#[test]
fn local_spend_reads_as_local() {
    let mut usage = the_pictures_week();
    let mut qwen = share(
        "opencode",
        Some("ollama/qwen3-coder:30b"),
        spent(0.0, 490_538, false),
        [0.0; 7],
    );
    qwen.local = true;
    usage.by_agent.push(qwen);
    let rows = rows(&usage, Form::Full, 120);
    let foot = &rows[rows.len() - 2];
    assert!(
        foot.trim_end_matches('│')
            .trim_end()
            .ends_with("· on API keys $0.00 · local $0.00 · ~ estimated at list prices"),
        "{foot}"
    );
    let row = rows.iter().find(|row| row.contains("OpenCode")).unwrap();
    assert!(row.contains(" local "), "{row}");
    assert!(!row.contains("$0 "), "{row}");
}

#[test]
fn a_day_with_no_spend_is_on_the_track_and_a_day_ahead_is_blank() {
    let theme = golden::theme();
    let kit = Kit::new(&theme);
    let mut usage = the_pictures_week();
    usage.by_agent = vec![share(
        "claude-code",
        Some("haiku"),
        spent(0.10, 1000, false),
        [0.02, 0.0, 0.08, 0.0, 0.0, 0.0, 0.0],
    )];
    // A Wednesday: Thursday to Saturday are still ahead.
    usage.history = history(0.0)
        .into_iter()
        .map(|day| DaySpend {
            day: day.day - Duration::days(3),
            ..day
        })
        .collect();
    let drawn = card(&usage, &kit, Form::Full, 120);
    let row = drawn
        .lines
        .iter()
        .find(|line| line.spans.iter().any(|span| span.content.contains("Haiku")))
        .expect("the model's row");
    // The days' bars, each a span of its own after the name, not the swatch before it: across
    // the day's three cells in the model's shape, in half cells on one scale.
    let week: Vec<(String, Option<ratatui::style::Color>)> = row
        .spans
        .iter()
        .skip_while(|span| !span.content.contains("Haiku"))
        .filter(|span| !span.content.is_empty() && span.content.chars().all(|c| "▁█▌".contains(c)))
        .map(|span| (span.content.to_string(), span.style.fg))
        .collect();
    let (own, track) = (theme.color(Role::CliClaude), theme.color(Role::Faint));
    assert_eq!(
        week,
        [
            ("█".to_owned(), own),
            ("▁▁▁".to_owned(), track),
            ("███".to_owned(), own),
            ("▁▁▁".to_owned(), track),
        ]
    );
    let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
    let inside = text.trim_end().trim_end_matches('│').trim_end();
    assert!(inside.ends_with("█   ▁▁▁ ███ ▁▁▁"), "{text:?}");
}

/// A week no model spent anything in says so once, where the days would be, under the days'
/// letters with today's in the accent.
#[test]
fn a_week_with_no_spend_says_so_once() {
    let theme = golden::theme();
    let kit = Kit::new(&theme);
    let mut usage = the_pictures_week();
    for share in &mut usage.by_agent {
        share.this_week.clear();
    }
    let drawn = card(&usage, &kit, Form::Full, 120);
    let text = plain_text(&drawn.lines);
    assert_eq!(text.matches("nothing spent this week").count(), 1, "{text}");
    let first = model_rows(&text.lines().map(str::to_owned).collect::<Vec<_>>())[0].clone();
    assert!(first.contains("3.40M   nothing spent this week"), "{first}");
    let head = drawn
        .lines
        .iter()
        .find(|line| line.spans.iter().any(|span| span.content == "model"))
        .expect("the heads");
    let today = head
        .spans
        .iter()
        .rev()
        .find(|span| span.content.trim() == "S");
    assert_eq!(
        today.map(|span| span.style),
        Some(kit.bold(Role::Accent)),
        "a Saturday is today: {head:?}"
    );
}

#[test]
fn a_period_with_no_spend_draws_an_empty_bar_and_no_rows() {
    let usage = UsageSummary {
        total: Spend::default(),
        by_agent: Vec::new(),
        stats: None,
        ..the_pictures_week()
    };
    let rows = rows(&usage, Form::Full, 80);
    assert_eq!(rows.len(), 5, "frame, header, blank, bar: {rows:#?}");
    assert!(rows[1].ends_with("$0.00 · no limit set · $0.04 today │"));
    assert!(rows[3].contains("─"), "{rows:#?}");
}

/// Where the terminal draws logos, the "on" column puts each CLI's logo before its name, and
/// every row keeps its words and its width.
#[test]
fn the_on_column_puts_each_clis_logo_before_its_name() {
    use crate::base::kit::logo::golden::{lines_with_stand_ins, pictured};

    let usage = the_pictures_week();
    let theme = pictured(&plain());
    let drawn = card(&usage, &Kit::new(&theme), Form::Full, 118);
    let pictured_rows: Vec<String> = plain_text(&lines_with_stand_ins(&drawn.lines))
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect();
    let plain_rows = rows(&usage, Form::Full, 118);
    assert_eq!(pictured_rows.len(), plain_rows.len());
    let words = |row: &str| {
        row.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let mut logos = 0;
    for (with_logo, without) in pictured_rows.iter().zip(&plain_rows) {
        logos += usize::from(with_logo.contains("◧◨"));
        assert_eq!(words(&with_logo.replacen("◧◨", "", 1)), words(without));
        assert_eq!(with_logo.chars().count(), without.chars().count());
    }
    assert!(logos >= 2, "{pictured_rows:#?}");
    // The head sits over the CLI's name, not over its logo.
    let column = |row: &str, word: &str| row.find(word).map(|at| row[..at].chars().count());
    let head = pictured_rows
        .iter()
        .find(|row| row.contains(" model "))
        .unwrap();
    let first = pictured_rows.iter().find(|row| row.contains("◧◨")).unwrap();
    assert_eq!(
        column(head, " on ").map(|at| at + 1),
        column(first, "Claude Code")
    );
}
