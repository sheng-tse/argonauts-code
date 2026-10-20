use std::sync::Arc;
use std::time::Instant;

use argonauts_core::api::{PlanUsage, UsageSummary};
use argonauts_core::app::ViewRequest;
use argonauts_core::limits::PlanUsageSource;
use argonauts_core::names::AgentId;
use argonauts_core::plan_usage::{
    Behavior, Behaviors, ExtraUsage, LiveUsage, LiveWindow, UsageReport, UsageShare,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use super::*;
use crate::overview::{OverviewView, Tab, UsageWatch};
use crate::stability;
use crate::state::AppState;
use crate::theme::{ColorLevel, Glyphs, Theme};
use crate::views::{View, ViewCtx, ViewOutcome};

/// Thursday, October 8, 8:12pm, on a clock seven hours behind UTC.
const NOW: OffsetDateTime = datetime!(2026-10-08 20:12 -7);

fn at(local: OffsetDateTime) -> Option<OffsetDateTime> {
    Some(local)
}

fn window(key: &str, label: &str, title: &str, used: f64, resets: OffsetDateTime) -> LiveWindow {
    LiveWindow::new(key, label, title)
        .used(Some(used))
        .resets_at(at(resets))
}

fn live(agent: &str, plan: &str, windows: Vec<LiveWindow>) -> LiveUsage {
    let mut live = LiveUsage::new(AgentId(agent.into()), PlanUsageSource::AppServer, NOW);
    live.account = Some("me@example.com".into());
    live.plan = Some(plan.into());
    live.windows = windows;
    live
}

fn plan(agent: &str, name: &str, live: LiveUsage, in_use: bool) -> PlanUsage {
    PlanUsage {
        agent: agent.into(),
        name: name.into(),
        plan: live.plan.clone(),
        in_use,
        windows: live.plan_usage_readings(),
        live: Some(live),
        ..PlanUsage::default()
    }
}

/// The sample figures, read at 8:12pm on Thursday, October 8.
fn summary() -> UsageSummary {
    let mut claude = live(
        "claude-code",
        "Max",
        vec![
            window(
                "five_hour",
                "session",
                "Current session",
                0.14,
                datetime!(2026-10-08 22:40 -7),
            ),
            window(
                "seven_day",
                "week",
                "Current week (all models)",
                0.21,
                datetime!(2026-10-13 22:00 -7),
            ),
            window(
                "model:Fable",
                "Fable week",
                "Current week (Fable)",
                0.0,
                datetime!(2026-10-13 22:00 -7),
            ),
        ],
    );
    claude.extra = Some(ExtraUsage {
        enabled: true,
        spent: 0.0,
        limit: Some(100.0),
        currency: "USD".into(),
        used: None,
    });
    claude.behaviors = Some(Behaviors {
        day: vec![
            Behavior {
                key: "long_context".into(),
                share: 0.95,
            },
            Behavior {
                key: "subagent_heavy".into(),
                share: 0.9,
            },
        ],
        week: Vec::new(),
        day_report: Some(UsageReport {
            requests: 312,
            sessions: 4,
            skills: vec![UsageShare {
                name: "review".into(),
                share: 0.4,
            }],
            mcp_servers: vec![UsageShare {
                name: "github".into(),
                share: 0.2,
            }],
            ..UsageReport::default()
        }),
        week_report: None,
    });
    let codex = live(
        "codex",
        "Plus",
        vec![
            window(
                "five_hour",
                "5-hour",
                "5h limit",
                0.0,
                datetime!(2026-10-09 00:15 -7),
            ),
            window(
                "weekly",
                "weekly",
                "Weekly limit",
                0.01,
                datetime!(2026-10-14 11:22 -7),
            ),
        ],
    );
    let agy = live(
        "antigravity-cli",
        "Pro",
        vec![
            window(
                "g:5h",
                "Gemini 5h",
                "Gemini · 5-hour limit",
                0.02,
                datetime!(2026-10-08 21:40 -7),
            ),
            window(
                "g:week",
                "Gemini week",
                "Gemini · weekly limit",
                0.01,
                datetime!(2026-10-14 22:52 -7),
            ),
            window(
                "o:5h",
                "Claude, GPT",
                "Claude and GPT · 5-hour limit",
                0.0,
                datetime!(2026-10-08 21:40 -7),
            )
            .note("5-hour"),
            window(
                "o:week",
                "Claude, GPT week",
                "Claude and GPT · weekly limit",
                0.0,
                datetime!(2026-10-14 22:52 -7),
            )
            .page_only(),
        ],
    );
    UsageSummary {
        plans: vec![
            plan("claude-code", "Claude Code", claude, true),
            plan("codex", "Codex", codex, false),
            plan("antigravity-cli", "Antigravity CLI", agy, false),
        ],
        conversation: Some(argonauts_core::Spend {
            cost_usd: 0.42,
            ..argonauts_core::Spend::default()
        }),
        ..UsageSummary::default()
    }
}

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn state() -> AppState {
    let mut state = AppState::default();
    state.usage = Some(Arc::new(summary()));
    state
}

fn rows(view: &mut OverviewView, state: &AppState, width: u16) -> Vec<String> {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let screen = Size::new(width, 80);
    let height = view.dock_height(screen, &ctx).unwrap_or(40);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    stability::assert_frames(&buf);
    crate::views::dock::lines(&buf)
        .iter()
        .map(|line| line.to_string().trim_end().to_owned())
        .collect()
}

fn key(view: &mut OverviewView, state: &AppState, code: KeyCode) -> ViewOutcome {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::from(code), &ctx)
}

fn usage_view() -> OverviewView {
    OverviewView::new(Tab::Usage, UsageWatch::default()).at(NOW)
}

fn find(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|row| row.contains(text))
        .unwrap_or_else(|| panic!("{text}: {rows:#?}"))
}

#[test]
fn a_reset_is_said_as_the_time_left_and_the_clock() {
    let words = |local| reset_words(local, NOW);
    assert_eq!(
        words(datetime!(2026-10-08 22:40 -7)),
        "resets in 2h 28m · today 10:40pm"
    );
    assert_eq!(
        words(datetime!(2026-10-09 00:15 -7)),
        "resets in 4h 3m · Fri 12:15am"
    );
    assert_eq!(
        words(datetime!(2026-10-13 22:00 -7)),
        "resets in 5d 2h · Tue Oct 13, 10:00pm"
    );
    assert_eq!(
        words(datetime!(2026-10-14 11:22 -7)),
        "resets in 5d 15h · Wed Oct 14, 11:22am"
    );
    assert_eq!(
        words(datetime!(2026-10-14 22:52 -7)),
        "resets in 6d 3h · Wed Oct 14, 10:52pm"
    );
}

#[test]
fn the_edges_of_the_reset_words() {
    // Within the hour it counts minutes, never down to nothing.
    assert_eq!(
        reset_words(datetime!(2026-10-08 20:54:30 -7), NOW),
        "resets in 43m · today 8:54pm"
    );
    assert_eq!(left_words(Duration::seconds(20)), "1m");
    assert_eq!(left_words(Duration::hours(2)), "2h");
    // Just past midnight is tomorrow's weekday; tomorrow after dawn gives the date too.
    let late = datetime!(2026-10-08 23:58 -7);
    assert_eq!(
        reset_words(datetime!(2026-10-09 00:03 -7), late),
        "resets in 5m · Fri 12:03am"
    );
    assert_eq!(
        reset_words(datetime!(2026-10-09 09:00 -7), late),
        "resets in 9h 2m · Fri Oct 9, 9:00am"
    );
    // A week and more is days alone, rounded.
    assert_eq!(
        reset_words(datetime!(2026-10-16 08:00 -7), NOW),
        "resets in 7d · Fri Oct 16, 8:00am"
    );
    assert_eq!(left_words(Duration::days(23) + Duration::hours(4)), "23d");
    // A reset that has passed says when, and that a fresh reading is on its way.
    assert_eq!(
        reset_words(datetime!(2026-10-08 19:40 -7), NOW),
        "reset today 7:40pm · reading again"
    );
    // A reading in UTC is shown on the local clock.
    assert_eq!(
        reset_words(datetime!(2026-10-09 05:40 UTC), NOW),
        "resets in 2h 28m · today 10:40pm"
    );
}

#[test]
fn a_wide_panel_draws_the_three_cards_side_by_side_as_the_picture() {
    let state = state();
    let rows = rows(&mut usage_view(), &state, 140);
    assert!(rows[1].ends_with("Today  Week  Month  All"), "{rows:#?}");
    assert!(
        rows[2].ends_with("read live 8:12pm · ctrl+r reads again"),
        "{rows:#?}"
    );
    assert!(
        rows[2].starts_with("  All CLIs    Claude Code    Codex    Antigravity CLI"),
        "{rows:#?}"
    );
    assert!(
        rows[4].starts_with(" This conversation · $0.42"),
        "{rows:#?}"
    );
    let head = find(&rows, "● Claude Code");
    assert_eq!(head, find(&rows, "● Codex"), "side by side: {rows:#?}");
    assert_eq!(head, find(&rows, "● Antigravity CLI"));
    assert!(rows[head].contains("Max · in use"), "{rows:#?}");
    assert!(rows[head].contains("Plus"));
    let session = find(&rows, "session");
    assert!(rows[session].contains("14%"), "{rows:#?}");
    assert!(rows[session].contains("Gemini 5h"));
    // Under each bar, in the same column on every card, broken at its ` · ` where it's long.
    let under = |row: usize, words: &str| {
        let at = rows[row]
            .find(words)
            .unwrap_or_else(|| panic!("{words}: {rows:#?}"));
        rows[row][..at].chars().count()
    };
    let bar = rows[session]
        .chars()
        .position(|c| matches!(c, '━' | '─' | '╸'))
        .unwrap();
    assert_eq!(under(session + 1, "resets in 2h 28m"), bar, "{rows:#?}");
    assert!(rows[session + 2].contains("today 10:40pm"), "{rows:#?}");
    assert!(rows[session + 1].contains("resets in 4h 3m · Fri 12:15am"));
    assert!(rows[session + 1].contains("resets in 1h 28m"));
    assert!(rows[session + 2].contains("today 9:40pm"));
    let fable = find(&rows, "Fable week");
    assert!(rows[fable].contains("Claude, GPT"), "{rows:#?}");
    assert!(rows[fable + 1].contains("5-hour · resets in 1h 28m"));
    assert!(rows[fable + 2].contains("today 9:40pm"));
    // The group's other window waits for its page.
    assert!(!rows.iter().any(|row| row.contains("Claude, GPT week")));
    let extra = find(&rows, "extra");
    assert!(
        rows[extra + 1].contains("$0 of $100 · resets in 23d"),
        "{rows:#?}"
    );
    assert!(rows[extra + 2].contains("Sun Nov 1"), "{rows:#?}");
    let most = find(&rows, "Most of your usage");
    assert!(
        rows[most].contains("· last 24h · 95% long context · 90% subagent-heavy · enter for more"),
        "{rows:#?}"
    );
    assert!(
        rows.last().unwrap().starts_with(
            " r to cycle dates · tab · ← → tabs · ↓ CLIs · ctrl+r reads again · esc closes"
        ),
        "{rows:#?}"
    );
}

#[test]
fn below_130_columns_the_cards_stack_each_whole() {
    let state = state();
    let rows = rows(&mut usage_view(), &state, 129);
    let claude = find(&rows, "● Claude Code");
    let codex = find(&rows, "● Codex");
    let agy = find(&rows, "● Antigravity CLI");
    assert!(claude < codex && codex < agy, "{rows:#?}");
    // Each card is whole: its reset lines under it, none cut.
    assert!(
        rows[claude..codex]
            .iter()
            .any(|row| row.contains("resets in 5d 2h · Tue Oct 13, 10:00pm"))
    );
    assert!(!rows.iter().any(|row| row.contains('…')), "{rows:#?}");
}

#[test]
fn a_cli_sub_tab_lays_out_like_claude_codes_usage() {
    let state = state();
    let mut view = usage_view();
    assert!(matches!(
        key(&mut view, &state, KeyCode::Down),
        ViewOutcome::Consumed
    ));
    key(&mut view, &state, KeyCode::Right);
    let page = rows(&mut view, &state, 100);
    assert!(page[2].contains("[Claude Code]"), "{page:#?}");
    let session = find(&page, "Current session");
    assert!(page[session + 1].ends_with("14% used"), "{page:#?}");
    assert_eq!(page[session + 2], " Resets in 2h 28m · today 10:40pm");
    let week = find(&page, "Current week (all models)");
    assert_eq!(page[week + 2], " Resets in 5d 2h · Tue Oct 13, 10:00pm");
    let credits = find(&page, "Usage credits");
    assert!(page[credits + 1].ends_with("$0 / $100 spent"), "{page:#?}");
    let report = find(&page, "What's contributing to your limits usage?");
    assert!(page[report + 1].starts_with(" Approximate, based on local sessions on this machine"));
    let day = find(&page, " Last 24h · 312 requests · 4 sessions");
    assert_eq!(page[day + 1], "   95% of your usage was at >150k context");
    assert_eq!(
        page[day + 2],
        "   90% of your usage came from subagent-heavy sessions"
    );
    assert_eq!(page[day + 3], "   Top skills: /review 40%");
    assert_eq!(page[day + 4], "   Top MCP servers: github 20%");
    assert!(
        page.last()
            .unwrap()
            .starts_with(" r to cycle dates · tab · ← → CLIs · ↓ into the cards · ↑ tabs"),
        "{page:#?}"
    );
    // ←→ wrap around the sub-tabs.
    key(&mut view, &state, KeyCode::Left);
    key(&mut view, &state, KeyCode::Left);
    let agy = rows(&mut view, &state, 100);
    assert!(agy[2].contains("[Antigravity CLI]"), "{agy:#?}");
    let gpt = find(&agy, "Claude and GPT · 5-hour limit");
    assert_eq!(agy[gpt + 2], " Resets in 1h 28m · today 9:40pm");
    let week = find(&agy, "Claude and GPT · weekly limit");
    assert_eq!(agy[week + 2], " Resets in 6d 3h · Wed Oct 14, 10:52pm");
}

#[test]
fn ctrl_r_reads_again_and_opening_the_tab_does_too() {
    let state = state();
    let mut view = usage_view();
    let ctx = ViewCtx {
        theme: &plain(),
        state: &state,
        now: Instant::now(),
    };
    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(matches!(
        view.handle_key(ctrl_r, &ctx),
        ViewOutcome::Request(ViewRequest::ReadPlanUsage)
    ));
    // `r` alone is the period's, here as on every tab that has one.
    assert!(matches!(
        key(&mut view, &state, KeyCode::Char('r')),
        ViewOutcome::Request(ViewRequest::Usage { .. })
    ));
    let mut other = OverviewView::new(Tab::Config, UsageWatch::default()).at(NOW);
    assert!(matches!(
        key(&mut other, &state, KeyCode::Right),
        ViewOutcome::Request(ViewRequest::ReadPlanUsage)
    ));
}

#[test]
fn the_conversation_line_says_how_long_and_how_much_of_it_working() {
    let usage = summary();
    let tab = UsageTab {
        usage: &usage,
        now: NOW,
        cli: 0,
        left: false,
        talked: Some((
            std::time::Duration::from_secs(52 * 60 + 30),
            std::time::Duration::from_secs(4 * 60 + 12),
        )),
        changed: (182, 21),
        focus: None,
    };
    let theme = plain();
    let kit = Kit::new(&theme);
    let line = tab.conversation(&kit, 120);
    let text: String = line.lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(
        text,
        " This conversation · $0.42 · 52m 30s, 4m 12s of it working · +182 −21 lines"
    );
    assert_eq!(talk_words(std::time::Duration::from_secs(3900)), "1h 5m");
}

#[test]
fn a_window_turns_amber_from_90_percent_and_red_when_full() {
    assert_eq!(level(Some(0.89)), Level::Ok);
    assert_eq!(level(Some(0.9)), Level::Warn);
    assert_eq!(level(Some(1.0)), Level::Danger);
    assert_eq!(level(None), Level::Ok);
}

#[test]
fn a_read_that_failed_says_so_and_its_page_says_why() {
    let mut usage = summary();
    // The reading before the failed read is kept, but no longer stands as the CLI's own.
    usage.plans[1].last = usage.plans[1].live.take();
    usage.plans[1].windows.clear();
    usage.plans[1].failed =
        Some("Codex couldn't be reached (application network policy is unavailable)".into());
    let mut state = AppState::default();
    state.usage = Some(Arc::new(usage));
    let mut view = usage_view();
    let cards = rows(&mut view, &state, 140);
    let failed = find(&cards, "Failed to load usage data: Codex couldn't");
    let kept = find(&cards, "last read today 8:12pm for me@example.com");
    assert!(kept > failed, "{cards:#?}");

    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Right);
    key(&mut view, &state, KeyCode::Right);
    let page = rows(&mut view, &state, 60);
    assert!(page[2].contains("[Codex]"), "{page:#?}");
    let said = find(
        &page,
        " Failed to load usage data: Codex couldn't be reached",
    );
    // Wrapped to the page rather than cut.
    assert!(
        page[said + 1].contains("network policy is unavailable"),
        "{page:#?}"
    );
    find(&page, " last read today 8:12pm for me@example.com");
}

/// A window whose reset has passed starts again from nothing, so the share read before it
/// stands for nothing: no amber, no 95%, until a new read lands.
#[test]
fn a_window_whose_reset_passed_reads_as_unknown() {
    let mut usage = summary();
    let claude = usage.plans[0].live.as_mut().unwrap();
    claude.windows[0].used = Some(0.95);
    claude.windows[0].resets_at = Some(NOW - Duration::minutes(5));
    let mut state = AppState::default();
    state.usage = Some(Arc::new(usage));
    let mut view = usage_view();
    let cards = rows(&mut view, &state, 140);
    let session = find(&cards, "session");
    assert!(!cards[session].contains("95%"), "{cards:#?}");
    assert!(
        cards[session].contains(crate::format::UNKNOWN),
        "{cards:#?}"
    );
    let reset = find(&cards, "reset today 8:07pm");
    assert!(cards[reset + 1].contains("reading again"), "{cards:#?}");

    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Right);
    let page = rows(&mut view, &state, 100);
    let current = find(&page, "Current session");
    assert!(!page[current + 1].contains("95%"), "{page:#?}");
    assert!(
        page[current + 1].contains(&format!("{} used", crate::format::UNKNOWN)),
        "{page:#?}"
    );
}

/// Stacked on a small screen, each CLI's card is a stop, so ↓ reaches the last of them.
#[test]
fn on_a_small_screen_the_keys_reach_every_card() {
    let mut state = AppState::default();
    state.usage = Some(Arc::new(summary()));
    let mut view = usage_view();
    // A screen 80 by 24, the panel as tall as the dock lets it be there.
    let rows_on = |view: &mut OverviewView| {
        let theme = plain();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let height = view.dock_height(Size::new(80, 24), &ctx).unwrap_or(24);
        let area = Rect::new(0, 0, 80, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        crate::views::dock::lines(&buf)
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect::<Vec<String>>()
    };
    let first = rows_on(&mut view);
    assert!(
        !first.iter().any(|row| row.contains("● Antigravity CLI")),
        "{first:#?}"
    );
    assert!(first.iter().any(|row| row.contains("↓ more")), "{first:#?}");
    key(&mut view, &state, KeyCode::Down);
    let mut seen = Vec::new();
    for _ in 0..6 {
        key(&mut view, &state, KeyCode::Down);
        seen.extend(rows_on(&mut view));
    }
    assert!(
        seen.iter().any(|row| row.contains("● Antigravity CLI")),
        "{seen:#?}"
    );
}

#[test]
fn a_read_on_its_way_and_no_reading_say_so() {
    let mut usage = summary();
    usage.plans[1].reading = true;
    usage.plans[1].live = None;
    usage.plans[2].live = None;
    let mut state = AppState::default();
    state.usage = Some(Arc::new(usage));
    let rows = rows(&mut usage_view(), &state, 140);
    assert!(
        rows[2].ends_with("reading live… · ctrl+r reads again"),
        "{rows:#?}"
    );
    find(&rows, "reading live…");
    find(&rows, "no live reading yet");
}
