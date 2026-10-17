use std::sync::Arc;

use argonauts_core::api::UsageSummary;
use argonauts_core::names::TurnId;

use super::*;
use crate::overview::sample;
use crate::stability;
use crate::state::AppState;
use crate::theme::{ColorLevel, Glyphs, Theme};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn state(usage: UsageSummary) -> AppState {
    let mut state = AppState::default();
    state.usage = Some(Arc::new(usage));
    state.names = sample::names();
    state
}

fn view(tab: Tab) -> OverviewView {
    OverviewView::new(tab, UsageWatch::default()).at(sample::now())
}

/// The panel's rows on a screen `width` by `rows`, at the height it asks for.
fn rows(view: &mut OverviewView, state: &AppState, width: u16, rows: u16) -> Vec<String> {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let screen = Size::new(width, rows);
    let height = view
        .dock_height(screen, &ctx)
        .unwrap_or_else(|| crate::views::dock::height(rows));
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

/// ↓ from the tabs, through any sub-tabs, into the cards.
fn into_the_cards(view: &mut OverviewView, state: &AppState) {
    while view.level() != Level::Cards {
        key(view, state, KeyCode::Down);
    }
}

/// ↓ until the next card has the keys, past the rows of the one before.
fn next_card(view: &mut OverviewView, state: &AppState) {
    let before = view.focus;
    while view.focus == before {
        key(view, state, KeyCode::Down);
    }
}

#[test]
fn the_usage_tab_draws_a_card_per_cli_under_the_tabs_and_takes_only_their_rows() {
    let state = state(sample::usage());
    let mut usage = view(Tab::Usage);
    let rows = rows(&mut usage, &state, 110, 60);
    assert!(rows[0].chars().all(|c| c == '─'), "{rows:#?}");
    assert!(
        rows[1].starts_with(
            " Settings   Overview    Status    Config   [Usage]   Context    Limits    Stats"
        ),
        "{rows:#?}"
    );
    assert!(rows[1].ends_with("Today  Week  Month  All"), "{rows:#?}");
    assert!(
        rows[2].starts_with("  All CLIs    Claude Code    Codex    Antigravity CLI    OpenRouter"),
        "{rows:#?}"
    );
    let at = |text: &str| {
        rows.iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text}: {rows:#?}"))
    };
    // Under 130 columns the cards stack, in the crew's order.
    let order = [
        at("● Claude Code"),
        at("● Codex"),
        at("● Antigravity CLI"),
        at("● OpenRouter"),
    ];
    assert!(order.is_sorted(), "{order:?}: {rows:#?}");
    // Stored readings name no account, so none stands in for a live one.
    assert!(
        rows[at("● Codex") + 2].contains("no live reading yet"),
        "{rows:#?}"
    );
    assert!(
        rows[at("● OpenRouter") + 2].contains("pays per token"),
        "{rows:#?}"
    );
    assert!(
        rows.last().unwrap().starts_with(
            " r to cycle dates · tab · ←→ tabs · ↓ CLIs · ctrl+r reads again · esc closes"
        ),
        "{rows:#?}"
    );
    assert!(
        rows.len() < 40,
        "it takes the rows it needs, not the screen"
    );
}

#[test]
fn the_keys_change_tab_units_and_pin_the_card_in_focus() {
    let state = state(sample::usage());
    let mut overview = view(Tab::Limits).with_period(UsagePeriod::Today);
    key(&mut overview, &state, KeyCode::Char('u'));
    let left = rows(&mut overview, &state, 110, 60);
    assert!(left.iter().any(|row| row.contains(" 29%")), "{left:#?}");

    into_the_cards(&mut overview, &state);
    let pinned = key(&mut overview, &state, KeyCode::Char('p'));
    let ViewOutcome::Send(UiCommand::Slash {
        command:
            SlashCommand::Mods {
                action: ModsAction::Change { the_mod, change },
                ..
            },
    }) = pinned
    else {
        panic!("{pinned:?}");
    };
    assert_eq!(the_mod, UsageCard::Spend.name());
    assert_eq!(
        change,
        ModPlaceChange::Toggle {
            place: Place::AboveInput
        }
    );

    // In the cards ←→ do nothing; a tab without sub-tabs moves the tabs with Shift+Tab.
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Left),
        ViewOutcome::Consumed
    ));
    assert_eq!(overview.tab(), Tab::Limits);
    assert!(matches!(
        key(&mut overview, &state, KeyCode::BackTab),
        ViewOutcome::Request(ViewRequest::Context)
    ));
    assert_eq!(overview.tab(), Tab::Context);
    assert_eq!(
        overview.level(),
        Level::Tabs,
        "the keys are on the tab they moved"
    );
    for _ in 0..4 {
        key(&mut overview, &state, KeyCode::Left);
    }
    assert_eq!(overview.tab(), Tab::Overview);
    // The tabs wrap at both ends, and Tab and Shift+Tab go along them too.
    key(&mut overview, &state, KeyCode::Left);
    assert_eq!(
        overview.tab(),
        Tab::Stats,
        "← on the first tab goes to the last"
    );
    key(&mut overview, &state, KeyCode::Tab);
    assert_eq!(
        overview.tab(),
        Tab::Overview,
        "tab on the last goes to the first"
    );
    key(&mut overview, &state, KeyCode::BackTab);
    assert_eq!(overview.tab(), Tab::Stats);
    key(&mut overview, &state, KeyCode::Right);
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Esc),
        ViewOutcome::Close
    ));
}

#[test]
fn status_shows_its_cards_under_its_tab_once_its_answer_comes() {
    let state = state(sample::usage());
    let mut overview = view(Tab::Overview);
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Right),
        ViewOutcome::Request(ViewRequest::StatusTab)
    ));
    let waiting = rows(&mut overview, &state, 110, 60);
    assert!(waiting[1].contains("[Status]"), "{waiting:#?}");
    assert!(
        !waiting.iter().any(|row| row.contains("Reading")),
        "{waiting:#?}"
    );

    assert!(overview.accept(&ViewData::StatusTab {
        view: Box::default(),
    }));
    let idle = rows(&mut overview, &state, 110, 60);
    assert!(
        idle.iter().any(|row| row.contains("None yet.")),
        "{idle:#?}"
    );

    assert!(overview.accept(&ViewData::StatusTab {
        view: Box::new(argonauts_core::status_tab::StatusTabView {
            run: Some(sample::status()),
            ..Default::default()
        }),
    }));
    // The run's cards come under the setup's, so a tall screen shows them all.
    let shown = rows(&mut overview, &state, 110, 120);
    // No row under the tabs: the cards start after one blank row.
    assert!(shown[2].is_empty(), "{shown:#?}");
    let at = |what: &str| {
        shown
            .iter()
            .position(|row| row.contains(what))
            .unwrap_or_else(|| panic!("{what}: {shown:#?}"))
    };
    // The run, then its spend beside plan usage, then what needs you, then what happened.
    let (run, spend, plan) = (at("◆ run-03"), at("◆ spend"), at("◆ plan usage"));
    assert!(run < spend, "{shown:#?}");
    assert_eq!(spend, plan, "side by side: {shown:#?}");
    assert!(at("◆ needs you") > spend, "{shown:#?}");
    assert!(at(" Recent  ") > at("◆ needs you"), "{shown:#?}");
    let keys = shown.last().unwrap();
    assert!(
        keys.contains("tab · ←→ tabs · ↓ into the cards · esc closes"),
        "{keys}"
    );
    // Your CLIs, their health, the settings and the runs come first, then the run's own cards.
    into_the_cards(&mut overview, &state);
    for _ in 0..4 {
        next_card(&mut overview, &state);
    }
    let shown = rows(&mut overview, &state, 110, 60);
    let keys = shown.last().unwrap();
    assert!(
        keys.contains("↑↓ task · enter agent · tab tabs · p pin above the input"),
        "{keys}"
    );

    // Narrow, the two stack.
    let narrow = rows(&mut overview, &state, 80, 120);
    let at = |what: &str| narrow.iter().position(|row| row.contains(what)).unwrap();
    assert!(at("◆ spend") < at("◆ plan usage"), "{narrow:#?}");

    assert!(matches!(
        key(&mut overview, &state, KeyCode::Char('s')),
        ViewOutcome::SendToConversation(UiCommand::Summary)
    ));
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Enter),
        ViewOutcome::Request(ViewRequest::Workflow { workflow: None, .. })
    ));
    // Usage numbers go on to the state, where the pinned cards read them too.
    assert!(!overview.accept(&ViewData::Usage {
        summary: Box::new(sample::usage())
    }));
}

/// A screen that follows run-03, with one session at work on its second task.
fn following_the_run() -> AppState {
    use argonauts_core::names::{RoleName, SessionKey};
    use argonauts_core::{RunId, TaskId};

    use crate::state::{RunState, SessionState, SessionStatus};

    let mut state = state(sample::usage());
    let id = RunId::new("run-03").expect("a run id");
    let mut run = RunState::new(id.clone());
    let key = SessionKey::numbered(7);
    let worker = "claude-code/claude-opus-5-5".parse().expect("a model");
    let mut session = SessionState::new(
        key.clone(),
        worker,
        String::new(),
        RoleName::Worker,
        sample::now(),
    );
    session.task = Some(TaskId::numbered(2));
    session.status = SessionStatus::Working;
    run.sessions.insert(key.clone(), session);
    run.session_order.push(key);
    state.runs.insert(id, run);
    state
}

#[test]
fn on_status_the_arrows_pick_a_task_and_enter_opens_its_agent() {
    let state = following_the_run();
    let mut status = view(Tab::Status).with_status(sample::status());
    let shown = rows(&mut status, &state, 110, 60);
    assert!(
        !shown.iter().any(|row| row.contains("│ ❯ ")),
        "no task is picked while the keys are on the tabs: {shown:#?}"
    );
    into_the_cards(&mut status, &state);
    let shown = rows(&mut status, &state, 110, 60);
    let picked = |rows: &[String]| {
        let row = rows.iter().find(|row| row.contains("│ ❯ "));
        row.cloned().unwrap_or_default()
    };
    assert!(
        picked(&shown).contains("task-001  CSV writer"),
        "{shown:#?}"
    );

    // Nobody works on the first task that this screen knows of, so its workflow opens.
    assert!(matches!(
        key(&mut status, &state, KeyCode::Enter),
        ViewOutcome::Request(ViewRequest::Workflow { .. })
    ));
    key(&mut status, &state, KeyCode::Down);
    let shown = rows(&mut status, &state, 110, 60);
    assert!(picked(&shown).contains("task-002  Report endpoint"));
    match key(&mut status, &state, KeyCode::Enter) {
        ViewOutcome::Request(ViewRequest::Agent { run, session }) => {
            assert_eq!(run.to_string(), "run-03");
            assert_eq!(session, argonauts_core::names::SessionKey::numbered(7));
        }
        other => panic!("{other:?}"),
    }
    for _ in 0..2 {
        key(&mut status, &state, KeyCode::Down);
    }
    let shown = rows(&mut status, &state, 110, 60);
    assert!(picked(&shown).contains("task-004  Review"), "{shown:#?}");

    // Past the last task ↓ goes to the next card, where no task is picked, the rows stay
    // where they were, and Enter opens the workflow.
    let before = shown
        .iter()
        .position(|row| row.contains("task-002"))
        .unwrap();
    key(&mut status, &state, KeyCode::Down);
    let shown = rows(&mut status, &state, 110, 60);
    assert_eq!(picked(&shown), "");
    assert!(
        shown[before].contains("│   ◐ task-002  Report endpoint"),
        "{shown:#?}"
    );
    assert!(
        shown.last().unwrap().contains("enter workflow"),
        "{shown:#?}"
    );
    assert!(matches!(
        key(&mut status, &state, KeyCode::Enter),
        ViewOutcome::Request(ViewRequest::Workflow { .. })
    ));
}

#[test]
fn on_status_p_pins_the_card_in_focus_and_tab_scrolls_to_it() {
    let state = state(sample::usage());
    let mut status = view(Tab::Status).with_status(sample::status());
    let pinned = |outcome: ViewOutcome| match outcome {
        ViewOutcome::Send(UiCommand::Slash {
            command:
                SlashCommand::Mods {
                    action: ModsAction::Change { the_mod, change },
                    ..
                },
        }) => {
            assert_eq!(
                change,
                ModPlaceChange::Toggle {
                    place: Place::AboveInput
                }
            );
            the_mod
        }
        other => panic!("{other:?}"),
    };
    let mut seen = Vec::new();
    into_the_cards(&mut status, &state);
    for at in 0..4 {
        seen.push(pinned(key(&mut status, &state, KeyCode::Char('p'))));
        if at < 3 {
            next_card(&mut status, &state);
        }
    }
    use ModName::{PlanUsage, Run, RunLimits, Waiting};
    assert_eq!(seen, [Run, RunLimits, PlanUsage, Waiting]);

    // On a short screen the card ↓ moved to scrolls into view, and ↑ brings the first back.
    let mut status = view(Tab::Status).with_status(sample::status());
    let short = rows(&mut status, &state, 110, 24);
    assert!(
        !short.iter().any(|row| row.contains("◆ needs you")),
        "{short:#?}"
    );
    into_the_cards(&mut status, &state);
    for _ in 0..3 {
        next_card(&mut status, &state);
    }
    let short = rows(&mut status, &state, 110, 24);
    assert!(
        short.iter().any(|row| row.contains("◆ needs you")),
        "{short:#?}"
    );
    for _ in 0..3 {
        key(&mut status, &state, KeyCode::Up);
    }
    let short = rows(&mut status, &state, 110, 24);
    assert!(
        short.iter().any(|row| row.contains("◆ run-03")),
        "{short:#?}"
    );
}

#[test]
fn context_shows_the_main_session_then_the_crew_and_the_arrows_step_through_it() {
    let mut state = state(sample::usage());
    state.conversation.context = Some(sample::counted());
    let mut overview = view(Tab::Context).with_context(sample::context());
    let shown = rows(&mut overview, &state, 120, 80);
    assert!(shown[1].contains("[Context]"), "{shown:#?}");
    assert!(
        shown[2].ends_with("3 sessions · 291k in context"),
        "{shown:#?}"
    );
    assert!(
        shown[4].contains("Context Usage  counted 0s ago"),
        "{shown:#?}"
    );
    assert!(shown[4].contains("Opus 5.5  claude-opus-5-5"), "{shown:#?}");
    assert!(shown[4].ends_with("212k/1M tokens (21%)"), "{shown:#?}");
    let at = |part: &str| {
        shown
            .iter()
            .position(|row| row.contains(part))
            .unwrap_or_else(|| panic!("{part}: {shown:#?}"))
    };
    // Claude Code's sections in its order, then the crew's agents last.
    let order = [
        "Estimated usage by category",
        "Auto-compact window:",
        "MCP tools · /mcp",
        "Custom agents · .claude/agents/",
        "Memory files · /memory",
        "Crew agents · ",
        "build:report-endpo",
        "build:docs",
    ]
    .map(at);
    assert!(order.is_sorted(), "{shown:#?}");
    assert!(
        shown.last().unwrap().contains("↓ into the cards"),
        "{shown:#?}"
    );
    assert!(!shown.last().unwrap().contains(" a "), "{shown:#?}");

    // In the cards ↑↓ step through the blocks, and the pointer says which has the keys.
    into_the_cards(&mut overview, &state);
    let shown = rows(&mut overview, &state, 120, 80);
    assert!(shown[4].contains("❯ Context Usage"), "{shown:#?}");
    assert!(shown.last().unwrap().contains("↑↓ section"), "{shown:#?}");
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Enter),
        ViewOutcome::Consumed
    ));
    // Enter on an agent opens its view.
    key(&mut overview, &state, KeyCode::End);
    let shown = rows(&mut overview, &state, 120, 80);
    assert!(
        shown.iter().any(|row| row.contains("❯ ◐ build:docs")),
        "{shown:#?}"
    );
    assert!(
        shown
            .last()
            .unwrap()
            .contains("↑↓ section · enter agent view"),
        "{shown:#?}"
    );
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Enter),
        ViewOutcome::Request(ViewRequest::Agent { .. })
    ));
    key(&mut overview, &state, KeyCode::Home);
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Up),
        ViewOutcome::Consumed
    ));
    assert_eq!(overview.level(), Level::Tabs, "↑ on the first block leaves");
}

/// Every list shows in full, so the tab is often taller than the panel. It scrolls by rows:
/// a page at a time from the tabs, and block by block in the cards.
#[test]
fn a_context_tab_taller_than_the_panel_scrolls_to_every_row() {
    let mut state = state(sample::usage());
    state.conversation.context = Some(sample::counted());
    let mut overview = view(Tab::Context).with_context(sample::context());
    let top = rows(&mut overview, &state, 120, 30);
    assert!(
        top.iter().any(|row| row.contains("Context Usage")),
        "{top:#?}"
    );
    assert!(
        !top.iter().any(|row| row.contains("Crew agents")),
        "{top:#?}"
    );
    assert!(top.last().unwrap().contains("↓ more"), "{top:#?}");

    let mut seen = top.clone();
    for _ in 0..8 {
        key(&mut overview, &state, KeyCode::PageDown);
        seen.extend(rows(&mut overview, &state, 120, 30));
    }
    for part in [
        "Auto-compact window:",
        "└ search_issues",
        "└ reviewer",
        "└ AGENTS.md",
        "└ release-notes",
        "build:docs",
    ] {
        assert!(
            seen.iter().any(|row| row.contains(part)),
            "{part}: {seen:#?}"
        );
    }
    let bottom = rows(&mut overview, &state, 120, 30);
    assert!(!bottom.last().unwrap().contains("↓ more"), "{bottom:#?}");
    key(&mut overview, &state, KeyCode::PageUp);
    key(&mut overview, &state, KeyCode::PageUp);
    key(&mut overview, &state, KeyCode::PageUp);
    assert_eq!(rows(&mut overview, &state, 120, 30), top);
}

#[test]
fn the_limits_tab_lists_the_limits_and_enter_opens_the_picked_one_to_change() {
    let state = state(sample::usage());
    let mut overview = view(Tab::Limits).with_period(UsagePeriod::Today);
    let shown = rows(&mut overview, &state, 120, 60);
    let at = |rows: &[String], what: &str| {
        rows.iter()
            .position(|row| row.contains(what))
            .unwrap_or_else(|| panic!("{what}: {rows:#?}"))
    };
    // What counts toward the limits, then the limits themselves.
    let list = at(&shown, " Limits · none set, so nothing stops a run");
    assert!(at(&shown, "◆ spend") < at(&shown, "◆ plan usage"));
    assert!(at(&shown, "◆ plan usage") < list, "{shown:#?}");
    assert!(
        shown[list + 1].starts_with(" ❯ Spend for a run"),
        "{shown:#?}"
    );
    into_the_cards(&mut overview, &state);
    let shown = rows(&mut overview, &state, 120, 60);
    assert!(
        shown.last().unwrap().contains("↑↓ limit · enter change"),
        "{shown:#?}"
    );
    // The picker opens under the input, so the panel closes as it sends.
    let opened = |outcome: ViewOutcome| match outcome {
        ViewOutcome::SendToConversation(UiCommand::Slash {
            command:
                SlashCommand::Limits {
                    key,
                    value: None,
                    action: ViewAction::Open,
                },
        }) => key,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        opened(key(&mut overview, &state, KeyCode::Enter)).as_deref(),
        Some("limits.spend_per_run")
    );
    key(&mut overview, &state, KeyCode::Down);
    key(&mut overview, &state, KeyCode::Down);
    let shown = rows(&mut overview, &state, 120, 60);
    assert!(shown[at(&shown, "❯")].contains("Plan usage"), "{shown:#?}");
    assert_eq!(
        opened(key(&mut overview, &state, KeyCode::Enter)).as_deref(),
        Some("limits.plan_usage")
    );

    // On a short screen the list is below the cards, and the arrows bring it into view.
    let mut short = view(Tab::Limits).with_period(UsagePeriod::Today);
    let hidden = rows(&mut short, &state, 120, 20);
    assert!(!hidden.iter().any(|row| row.contains("❯")), "{hidden:#?}");
    into_the_cards(&mut short, &state);
    key(&mut short, &state, KeyCode::Down);
    let found = rows(&mut short, &state, 120, 20);
    assert!(
        found[at(&found, "❯")].contains("Spend for a day"),
        "{found:#?}"
    );

    // A controller that lists no limits: Enter opens them all in their own picker.
    let mut bare = sample::usage();
    bare.limits.clear();
    let state = self::state(bare);
    let mut overview = view(Tab::Limits).with_period(UsagePeriod::Today);
    let shown = rows(&mut overview, &state, 120, 60);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("Enter opens the limits")),
        "{shown:#?}"
    );
    into_the_cards(&mut overview, &state);
    assert_eq!(opened(key(&mut overview, &state, KeyCode::Enter)), None);
    // On the other usage tabs Enter has nothing to open.
    let mut usage = view(Tab::Overview);
    into_the_cards(&mut usage, &state);
    assert!(matches!(
        key(&mut usage, &state, KeyCode::Enter),
        ViewOutcome::Ignored
    ));
}

#[test]
fn the_limits_tab_shows_what_each_task_of_the_followed_run_spent() {
    let mut state = following_the_run();
    let id = argonauts_core::RunId::new("run-03").expect("a run id");
    let run = state.runs.get_mut(&id).expect("the run");
    for (n, title) in [(1, "CSV writer"), (2, "Report endpoint")] {
        let task = crate::state::TaskRow::new(argonauts_core::TaskId::numbered(n), title);
        run.tasks.insert(task.id.clone(), task);
    }
    for session in run.sessions.values_mut() {
        session.cost_usd = Some(0.62);
    }
    state.attached = Some(id);
    let mut overview = view(Tab::Limits).with_period(UsagePeriod::Today);
    let shown = rows(&mut overview, &state, 120, 60);
    let tasks = shown
        .iter()
        .position(|row| row.contains("◆ tasks"))
        .unwrap_or_else(|| panic!("{shown:#?}"));
    assert!(
        shown[tasks].ends_with("$0.62 · 2 tasks · no limit set │"),
        "{shown:#?}"
    );
    assert!(shown[tasks + 2].contains("task-002  Report endpoint"));
    assert!(shown[tasks + 2].ends_with("$0.62 100% │"), "{shown:#?}");

    assert!(
        !shown.iter().any(|row| row.contains("◆ spend · run-03")),
        "the run has reported no spend of its own yet: {shown:#?}"
    );

    // Once it has, the run's spend against its own limit heads the tab.
    let id = argonauts_core::RunId::new("run-03").expect("a run id");
    state.runs.get_mut(&id).expect("the run").spend = sample::status_with_limit().spend;
    let shown = rows(&mut overview, &state, 120, 60);
    let at = |what: &str| {
        shown
            .iter()
            .position(|row| row.contains(what))
            .unwrap_or_else(|| panic!("{what}: {shown:#?}"))
    };
    assert!(
        shown[at("◆ spend · run-03")].contains("$1.20 of $3.00"),
        "{shown:#?}"
    );
    assert!(at("◆ spend · run-03") < at("◆ plan usage"), "{shown:#?}");
    assert!(at("◆ plan usage") < at("◆ tasks"), "{shown:#?}");

    // With no run followed there is no run's spend and there are no tasks to show.
    let idle = self::state(sample::usage());
    let shown = rows(&mut view(Tab::Limits), &idle, 120, 60);
    assert!(!shown.iter().any(|row| row.contains("◆ tasks")));
    assert!(!shown.iter().any(|row| row.contains("run-03")));
}

#[test]
fn an_open_status_tab_is_asked_again_only_while_its_run_works() {
    let idle = state(sample::usage());
    let watch = UsageWatch::default();
    let mut overview = OverviewView::new(Tab::Status, watch.clone())
        .at(sample::now())
        .with_status(sample::status());
    let t0 = Instant::now();
    rows(&mut overview, &idle, 110, 60);
    // The plan usage card keeps the usage numbers coming. The run isn't one this screen
    // holds at work, so its status is never asked for again.
    assert!(matches!(watch.due(t0), Some(ViewRequest::Usage { .. })));
    for seconds in 1..3 {
        let asked = watch.due(t0 + Duration::from_secs(seconds));
        assert!(!matches!(asked, Some(ViewRequest::Status)), "{asked:?}");
    }
}

#[test]
fn stats_asks_for_the_overviews_numbers_until_it_has_every_days() {
    use argonauts_core::overview::OverviewData;
    let mut usage = sample::usage_with_stats();
    usage.period = UsagePeriod::All;
    let state = state(usage);
    let watch = UsageWatch::default();
    let mut stats = OverviewView::new(Tab::Stats, watch.clone())
        .at(sample::now())
        .with_period(UsagePeriod::All);
    let t0 = Instant::now();
    let shown = rows(&mut stats, &state, 120, 60);
    let title = shown
        .iter()
        .find(|row| row.contains("◆ all together"))
        .expect("the card of every CLI together");
    assert!(
        title.contains("the crew's work through Argonauts Code")
            && title.ends_with("5.0M tokens │"),
        "{title}"
    );
    // The Overview's numbers are asked for while the tab waits for them, between the asks
    // for its own.
    let asked: Vec<ViewRequest> = (0..4)
        .flat_map(|seconds| {
            let at = t0 + Duration::from_secs(seconds);
            [watch.due(at), watch.due(at)]
        })
        .flatten()
        .collect();
    let every_day = ViewRequest::Overview {
        period: OverviewPeriod::All,
        import: None,
    };
    assert!(asked.contains(&every_day), "{asked:?}");

    let mut overview = OverviewData::empty(OverviewPeriod::All, sample::now().date());
    overview.total.cache_read = 69_100_000_000;
    stats.accept(&ViewData::Overview {
        overview: Box::new(overview),
    });
    let shown = rows(&mut stats, &state, 120, 60);
    assert!(
        shown
            .iter()
            .any(|row| row.ends_with("5.0M tokens of 69.1b on this computer (Overview) │")),
        "{shown:#?}"
    );
    // A shorter period keeps its own name and totals, which no number here compares with.
    let mut week = OverviewView::new(Tab::Stats, UsageWatch::default())
        .at(sample::now())
        .with_period(UsagePeriod::Week);
    let shown = rows(&mut week, &self::state(sample::usage_with_stats()), 120, 60);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("◆ all together  this week, Sunday to Saturday")),
        "{shown:#?}"
    );
}

#[test]
fn every_tab_holds_at_every_width_and_drawing_usage_keeps_it_coming() {
    let state = state(sample::usage());
    for width in stability::WIDTHS {
        for tab in Tab::ALL {
            let mut overview = view(tab);
            let shown = rows(&mut overview, &state, width, 40);
            assert!(shown.len() >= 5, "{tab:?} at {width}: {shown:#?}");
        }
    }
    let watch = UsageWatch::default();
    let mut usage = OverviewView::new(Tab::Usage, watch.clone()).at(sample::now());
    rows(&mut usage, &state, 110, 40);
    assert!(watch.due(Instant::now()).is_some());

    let quiet = UsageWatch::default();
    let mut context = OverviewView::new(Tab::Context, quiet.clone()).at(sample::now());
    rows(&mut context, &state, 110, 40);
    assert!(
        quiet.due(Instant::now()).is_none(),
        "nothing works, so nothing ages"
    );
}

#[test]
fn mid_answer_the_keys_say_esc_closes_the_panel_and_not_the_answer() {
    let mut state = state(sample::usage());
    state.mode = Some(AppMode::Answering {
        turn: TurnId::numbered(1),
    });
    let mut overview = view(Tab::Usage);
    // The way out shows whole at any width; the keys before it give way.
    for width in [200, 110, 80, 60] {
        let shown = rows(&mut overview, &state, width, 120);
        let keys = shown.last().unwrap();
        assert!(
            keys.ends_with("esc closes this · the answer keeps running"),
            "{width}: {shown:#?}"
        );
        assert!(
            keys.starts_with(" r to cycle dates · ") || keys.starts_with(" esc closes this"),
            "{width}: {shown:#?}"
        );
    }
    let shown = rows(&mut overview, &state, 200, 120);
    assert!(shown.iter().any(|row| row.contains("● Claude Code")));
}

/// Your mods from `mods/` that name a tab sit under that tab's own cards, and nowhere else.
#[test]
fn your_mods_show_under_the_tab_their_file_names() {
    use argonauts_core::settings::mods::view::{ModRow, ModRowKind, ModsView};
    use argonauts_core::settings::mods::{
        ModBody, ModOrigin, ModPlace, ModReading, builtin, check,
    };

    let (name, text) = builtin::STARTERS
        .iter()
        .find(|(starter, _)| *starter == "checks")
        .unwrap();
    let mut file = check::read(name, text).unwrap();
    file.places = vec![ModPlace::Usage];
    let mut state = state(sample::usage());
    state.mods = Some(ModsView {
        rows: vec![ModRow {
            name: (*name).to_owned(),
            origin: ModOrigin::Yours,
            path: format!("~/.argonauts/mods/{name}.toml"),
            kind: ModRowKind::Card {
                file: Box::new(file),
                built_in: None,
                reading: Some(ModReading::sample(ModBody::Stack)),
                note: None,
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }],
        ..ModsView::default()
    });
    let titled = |rows: &[String]| rows.iter().any(|row| row.contains(" checks "));
    let usage = rows(&mut view(Tab::Usage), &state, 110, 80);
    assert!(titled(&usage), "{usage:#?}");
    let overview = rows(&mut view(Tab::Overview), &state, 110, 80);
    assert!(!titled(&overview), "{overview:#?}");
}

/// Stats has sub-tabs: ↓ goes to them, ←→ change them and the cards with them at once, ↓ goes
/// into the cards and ↑ on the first row comes back up, a row at a time. Only fill says which
/// row has the keys, so there are brackets on one row at a time and never a box.
#[test]
fn down_goes_to_the_sub_tabs_then_into_the_cards_and_up_comes_back() {
    let state = state(sample::usage_with_stats());
    let mut stats = view(Tab::Stats).with_period(UsagePeriod::Week);
    let marked = |rows: &[String]| (rows[1].contains("[Stats]"), rows[2].contains("[By"));
    let shown = rows(&mut stats, &state, 120, 80);
    assert_eq!(marked(&shown), (true, false), "{shown:#?}");

    key(&mut stats, &state, KeyCode::Down);
    assert_eq!(stats.level(), Level::SubTabs);
    let shown = rows(&mut stats, &state, 120, 80);
    assert_eq!(marked(&shown), (false, true), "{shown:#?}");
    assert!(shown[2].contains("[By CLI]"), "{shown:#?}");
    assert!(
        shown.last().unwrap().starts_with(
            " r to cycle dates · tab By CLI, By model, By role · ↓ into the cards · ↑ tabs"
        ),
        "{shown:#?}"
    );

    key(&mut stats, &state, KeyCode::Right);
    assert_eq!(
        stats.stats.group,
        StatsGroup::Model,
        "the cards change at once"
    );
    assert_eq!(
        stats.tab(),
        Tab::Stats,
        "→ on the sub-tabs stays on the tab"
    );
    key(&mut stats, &state, KeyCode::Left);
    key(&mut stats, &state, KeyCode::Left);
    assert_eq!(stats.stats.group, StatsGroup::Role, "the sub-tabs wrap too");

    key(&mut stats, &state, KeyCode::Down);
    assert_eq!(stats.level(), Level::Cards);
    let shown = rows(&mut stats, &state, 120, 80);
    assert_eq!(
        marked(&shown),
        (false, true),
        "Tab moves the sub-tabs from the cards, so they stay marked: {shown:#?}"
    );
    assert!(shown[2].contains("[By role]"), "{shown:#?}");
    key(&mut stats, &state, KeyCode::Down);
    key(&mut stats, &state, KeyCode::Up);
    assert_eq!(
        stats.level(),
        Level::Cards,
        "↑ from the second row goes to the first"
    );
    key(&mut stats, &state, KeyCode::Up);
    assert_eq!(stats.level(), Level::SubTabs);
    key(&mut stats, &state, KeyCode::Up);
    assert_eq!(stats.level(), Level::Tabs);
}

/// With vim keys on: h and l along a row, j and k between rows, g and G the first and last,
/// and q closes.
#[test]
fn vim_keys_move_through_the_panel() {
    let mut state = state(sample::usage());
    state.ui = list::keys_state(true).ui;
    let mut overview = view(Tab::Usage);
    let letter = |view: &mut OverviewView, c: char| key(view, &state, KeyCode::Char(c));
    letter(&mut overview, 'l');
    assert_eq!(overview.tab(), Tab::Context);
    letter(&mut overview, 'h');
    letter(&mut overview, 'h');
    letter(&mut overview, 'h');
    assert_eq!(overview.tab(), Tab::Status);
    letter(&mut overview, 'l');
    letter(&mut overview, 'l');
    letter(&mut overview, 'j');
    assert_eq!(overview.level(), Level::SubTabs);
    letter(&mut overview, 'l');
    assert_eq!(overview.cli, 1, "l goes along the CLIs' sub-tabs");
    letter(&mut overview, 'h');
    assert_eq!(overview.cli, 0);
    letter(&mut overview, 'j');
    assert_eq!(overview.level(), Level::Cards);
    letter(&mut overview, 'G');
    let plans = state.usage.as_ref().unwrap().plans.len();
    assert_eq!(
        overview.focus, plans,
        "the conversation's line, then a card per CLI"
    );
    letter(&mut overview, 'g');
    assert_eq!(overview.focus, 0);
    letter(&mut overview, 'k');
    assert_eq!(overview.level(), Level::SubTabs);
    letter(&mut overview, 'k');
    assert_eq!(overview.level(), Level::Tabs);
    assert!(matches!(letter(&mut overview, 'q'), ViewOutcome::Close));

    // Without vim keys the letters do what they always did.
    let state = self::state(sample::usage());
    let mut overview = view(Tab::Usage);
    key(&mut overview, &state, KeyCode::Char('l'));
    assert_eq!(overview.tab(), Tab::Usage);
}

/// The panel drawn in colour on a screen `width` by `rows`, at the height it asks for.
fn colour_buffer(view: &mut OverviewView, state: &AppState, width: u16, rows: u16) -> Buffer {
    let theme = crate::overview::golden::theme();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let height = view
        .dock_height(Size::new(width, rows), &ctx)
        .unwrap_or(rows);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

/// The tabs in plain text, all alike, the open one filled; under them what the tab shows and
/// its sub-tabs flush right, two cells apart, as the Stats picture draws the panel's head. The
/// picture is the panel one column in from each edge of a screen two columns wider.
#[test]
fn the_head_of_the_panel_matches_its_picture() {
    use crate::overview::golden::{Picture, Region, assert_matches, theme};
    let picture = Picture::load("by_cli_after");
    let state = state(sample::usage_with_stats());
    let mut stats = view(Tab::Stats).with_period(UsagePeriod::All);
    let buf = colour_buffer(&mut stats, &state, 120, 80);
    // The tabs; the periods at the right are the Stats tab's own words.
    let tabs = Region {
        rows: 0..1,
        columns: 0..90,
        at: (1, 1),
    };
    assert_matches(&picture, &buf, &tabs, &theme());
    let about = Region {
        rows: 1..2,
        columns: 0..118,
        at: (1, 2),
    };
    assert_matches(&picture, &buf, &about, &theme());
    // The periods as Limits draws them: the one shown in the accent, flush right.
    let mut limits = view(Tab::Limits).with_period(UsagePeriod::All);
    let mut all = sample::usage();
    all.period = UsagePeriod::All;
    let state = self::state(all);
    let buf = colour_buffer(&mut limits, &state, 120, 80);
    let periods = Region {
        rows: 0..1,
        columns: 90..118,
        at: (1, 1),
    };
    assert_matches(&picture, &buf, &periods, &theme());
}

/// Tab follows the row with the keys; ←→ do nothing in the cards; Enter on the tabs does
/// nothing and on the sub-tabs goes to the first row; the key line says what Tab does.
#[test]
fn tab_goes_along_the_row_with_the_keys() {
    let state = state(sample::usage_with_stats());
    let mut stats = view(Tab::Stats).with_period(UsagePeriod::Week);
    assert!(matches!(
        key(&mut stats, &state, KeyCode::Enter),
        ViewOutcome::Consumed
    ));
    assert_eq!(stats.level(), Level::Tabs, "enter on the tabs does nothing");
    key(&mut stats, &state, KeyCode::Tab);
    assert_eq!(stats.tab(), Tab::Overview, "on the tabs Tab moves the tabs");
    key(&mut stats, &state, KeyCode::BackTab);
    assert_eq!(stats.tab(), Tab::Stats);

    key(&mut stats, &state, KeyCode::Down);
    assert_eq!(stats.level(), Level::SubTabs);
    key(&mut stats, &state, KeyCode::Tab);
    assert_eq!(
        stats.tab(),
        Tab::Stats,
        "on the sub-tabs Tab moves the sub-tabs"
    );
    assert_eq!(stats.stats.group, StatsGroup::Model);
    key(&mut stats, &state, KeyCode::BackTab);
    key(&mut stats, &state, KeyCode::BackTab);
    assert_eq!(stats.stats.group, StatsGroup::Role, "and wraps");

    key(&mut stats, &state, KeyCode::Enter);
    assert_eq!(
        stats.level(),
        Level::Cards,
        "enter on the sub-tabs goes to the first row"
    );
    let shown = rows(&mut stats, &state, 160, 80);
    assert!(
        shown
            .last()
            .unwrap()
            .contains("tab By CLI, By model, By role"),
        "{shown:#?}"
    );
    key(&mut stats, &state, KeyCode::Tab);
    assert_eq!(
        stats.stats.group,
        StatsGroup::Cli,
        "in the cards Tab moves the sub-tabs"
    );
    assert_eq!(
        stats.level(),
        Level::Cards,
        "and the keys stay in the cards"
    );
    assert!(matches!(
        key(&mut stats, &state, KeyCode::Right),
        ViewOutcome::Consumed
    ));
    assert_eq!(stats.tab(), Tab::Stats, "→ in the cards does nothing");

    // A tab without sub-tabs: Tab moves the tabs from any row, and the line says so.
    let state = self::state(sample::usage());
    let mut limits = view(Tab::Limits);
    into_the_cards(&mut limits, &state);
    let shown = rows(&mut limits, &state, 160, 80);
    assert!(shown.last().unwrap().contains("tab tabs"), "{shown:#?}");
    key(&mut limits, &state, KeyCode::Tab);
    assert_eq!(limits.tab(), Tab::Stats);
}

/// Esc closes the panel at once from every row, with a CLI's page open too, and so does q
/// with vim keys on.
#[test]
fn esc_closes_the_panel_from_any_row() {
    let state = state(sample::usage());
    for tab in Tab::ALL {
        for downs in 0..4 {
            let mut panel = view(tab);
            for _ in 0..downs {
                key(&mut panel, &state, KeyCode::Down);
            }
            assert!(
                matches!(key(&mut panel, &state, KeyCode::Esc), ViewOutcome::Close),
                "{tab:?} after {downs} ↓"
            );
        }
    }
    let mut usage = view(Tab::Usage);
    into_the_cards(&mut usage, &state);
    key(&mut usage, &state, KeyCode::Down);
    key(&mut usage, &state, KeyCode::Enter);
    assert_eq!(usage.cli, 1, "enter on the first card opens its CLI's page");
    assert!(matches!(
        key(&mut usage, &state, KeyCode::Esc),
        ViewOutcome::Close
    ));
    let mut vim = state.clone();
    vim.ui = list::keys_state(true).ui;
    let mut usage = view(Tab::Usage);
    into_the_cards(&mut usage, &vim);
    assert!(matches!(
        key(&mut usage, &vim, KeyCode::Char('q')),
        ViewOutcome::Close
    ));
}

/// /usage opens with the keys on the tabs; in the cards the card with the keys has its title
/// filled, Enter opens its CLI's page and Enter there comes back to the same card.
#[test]
fn usage_shows_the_card_with_the_keys_and_enter_opens_and_closes_its_page() {
    let state = state(sample::usage());
    let mut usage = view(Tab::Usage);
    assert_eq!(usage.level(), Level::Tabs);
    into_the_cards(&mut usage, &state);
    key(&mut usage, &state, KeyCode::Down);
    key(&mut usage, &state, KeyCode::Down);
    assert_eq!(usage.focus, 2);
    let theme = crate::overview::golden::theme();
    let buf = colour_buffer(&mut usage, &state, 110, 80);
    let filled = theme.color(Role::Question);
    let lines = crate::views::dock::lines(&buf);
    let codex = lines
        .iter()
        .position(|line| line.to_string().contains("● Codex"))
        .expect("Codex's card");
    let column = lines[codex].to_string().find("Codex").unwrap();
    let column = lines[codex].to_string()[..column].chars().count() as u16;
    assert_eq!(
        buf.cell((column, codex as u16)).map(|cell| cell.bg),
        filled,
        "the card with the keys has its title filled"
    );
    let shown = rows(&mut usage, &state, 110, 80);
    assert!(
        shown
            .last()
            .unwrap()
            .contains("↑↓ card · enter opens its page · tab CLIs"),
        "{shown:#?}"
    );
    key(&mut usage, &state, KeyCode::Enter);
    assert_eq!(usage.cli, 2, "Codex's page");
    assert_eq!(usage.level(), Level::Cards);
    key(&mut usage, &state, KeyCode::Enter);
    assert_eq!((usage.cli, usage.focus), (0, 2), "back on Codex's card");
}

/// The Overview tab's sub-tabs go by the panel's keys like every tab's: ↓ reaches them, Tab
/// and Shift+Tab move between Overview and Models, ↑ goes back to the tabs, and Esc closes
/// from the sub-tabs as from anywhere.
#[test]
fn tab_moves_the_overview_sub_tabs_as_it_does_every_tabs() {
    let state = state(sample::usage());
    let mut overview = view(Tab::Overview);
    overview.accept(&ViewData::Overview {
        overview: Box::new(crate::overview::home::sample::overview()),
    });
    let marked = |overview: &mut OverviewView| {
        let shown = rows(overview, &state, 160, 80);
        let row = shown
            .iter()
            .find(|row| row.contains("Overview") && row.contains("Models"))
            .cloned()
            .unwrap_or_default();
        (row.contains("[Overview]"), row.contains("[Models]"))
    };
    key(&mut overview, &state, KeyCode::Down);
    assert_eq!(overview.level(), Level::SubTabs);
    assert_eq!(marked(&mut overview), (true, false));

    key(&mut overview, &state, KeyCode::Tab);
    assert_eq!(overview.tab(), Tab::Overview, "Tab moves the sub-tabs here");
    assert_eq!(marked(&mut overview), (false, true));
    key(&mut overview, &state, KeyCode::BackTab);
    assert_eq!(marked(&mut overview), (true, false));

    key(&mut overview, &state, KeyCode::Up);
    assert_eq!(overview.level(), Level::Tabs);
    let shown = rows(&mut overview, &state, 160, 80);
    let keys = shown
        .iter()
        .rev()
        .find(|row| !row.trim().is_empty())
        .unwrap();
    assert!(
        keys.trim_start()
            .starts_with("r to cycle dates · tab · ←→ tabs · ↓ Overview or Models"),
        "on the tabs the line says what r and Tab do: {keys}"
    );
    key(&mut overview, &state, KeyCode::Down);
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Esc),
        ViewOutcome::Close
    ));
}

/// Where the keys are among the open tab's cards, or its rows where it lists rows.
fn card_at(view: &OverviewView) -> usize {
    match view.tab {
        Tab::Overview => view.home.picked,
        Tab::Status | Tab::Usage => view.focus,
        Tab::Context => view.pane.focused(),
        Tab::Limits => view.limit,
        Tab::Stats => view.stats.picked,
        // Config's search box takes letters, so its own tests move through its rows.
        Tab::Config => 0,
    }
}

/// Every tab with what it lists in hand, the keys in its cards.
fn in_the_cards_of(tab: Tab, state: &AppState) -> OverviewView {
    let mut view = view(tab)
        .with_period(UsagePeriod::Week)
        .with_status(sample::status())
        .with_context(sample::context());
    if tab == Tab::Overview {
        view.accept(&ViewData::Overview {
            overview: Box::new(crate::overview::home::sample::models()),
        });
        // Its models are the sub-tab with cards to move through.
        key(&mut view, state, KeyCode::Down);
        key(&mut view, state, KeyCode::Right);
    }
    into_the_cards(&mut view, state);
    // The Context tab steps through the blocks it last laid out, so it is drawn once first.
    if tab == Tab::Context {
        rows(&mut view, state, 160, 80);
    }
    view
}

/// Home and End go to the first and the last card on every tab, and so do g and G, with vim
/// keys or without. On Status the run's card takes the first press for its own tasks.
#[test]
fn home_end_g_and_shift_g_reach_the_first_and_last_card_on_every_tab() {
    let mut state = state(sample::usage_with_stats());
    if let Some(usage) = state.usage.as_mut() {
        Arc::make_mut(usage).limits = sample::limits();
    }
    for vim in [false, true] {
        state.ui = list::keys_state(vim).ui;
        for tab in Tab::ALL.into_iter().filter(|tab| *tab != Tab::Config) {
            let pairs = [
                (KeyCode::End, KeyCode::Home),
                (KeyCode::Char('G'), KeyCode::Char('g')),
            ];
            for (to_last, to_first) in pairs {
                let mut view = in_the_cards_of(tab, &state);
                assert_eq!(view.level(), Level::Cards, "{tab:?}");
                assert_eq!(card_at(&view), 0, "{tab:?} starts on its first");
                key(&mut view, &state, to_last);
                key(&mut view, &state, to_last);
                let last = card_at(&view);
                assert!(
                    last > 0,
                    "{tab:?}, vim {vim}: {to_last:?} moved to the last"
                );
                key(&mut view, &state, KeyCode::Down);
                assert_eq!(card_at(&view), last, "{tab:?}: nothing is past the last");
                key(&mut view, &state, to_first);
                key(&mut view, &state, to_first);
                assert_eq!(card_at(&view), 0, "{tab:?}, vim {vim}: {to_first:?}");
                assert_eq!(
                    view.level(),
                    Level::Cards,
                    "{tab:?}: the keys stay in the cards"
                );
            }
        }
    }
}

/// `r` cycles the period forward and Shift+R back on every tab that has periods, from any
/// row, and the tab's key line starts by saying so.
#[test]
fn r_cycles_the_period_on_every_tab_that_has_one_and_shift_r_goes_back() {
    let state = state(sample::usage_with_stats());
    let press = |view: &mut OverviewView, code: KeyCode, modifiers: KeyModifiers| {
        let theme = plain();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.handle_key(KeyEvent::new(code, modifiers), &ctx)
    };
    let asks = |outcome: ViewOutcome| match outcome {
        ViewOutcome::Request(ViewRequest::Usage { period }) => Some(period),
        _ => None,
    };
    for tab in [Tab::Usage, Tab::Limits, Tab::Stats] {
        let mut view = view(tab).with_period(UsagePeriod::Week);
        for level in [Level::Tabs, Level::SubTabs, Level::Cards] {
            while view.level() != level && tab != Tab::Limits {
                key(&mut view, &state, KeyCode::Down);
            }
            let on = press(&mut view, KeyCode::Char('r'), KeyModifiers::NONE);
            assert_eq!(asks(on), Some(UsagePeriod::Month), "{tab:?} {level:?}");
            let back = press(&mut view, KeyCode::Char('R'), KeyModifiers::SHIFT);
            assert_eq!(asks(back), Some(UsagePeriod::Week), "{tab:?} {level:?}");
        }
        // From the last it goes round to the first, and back.
        let mut view = self::view(tab).with_period(UsagePeriod::All);
        let round = press(&mut view, KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(asks(round), Some(UsagePeriod::Today), "{tab:?}");
        let back = press(&mut view, KeyCode::Char('R'), KeyModifiers::SHIFT);
        assert_eq!(asks(back), Some(UsagePeriod::All), "{tab:?}");
        let shown = rows(&mut view, &state, 140, 60);
        let keys = shown.iter().rev().find(|row| !row.trim().is_empty());
        assert!(
            keys.is_some_and(|keys| keys.trim_start().starts_with("r to cycle dates · ")),
            "{tab:?}: {shown:#?}"
        );
    }
    // On Stats `r` no longer changes what the cards are grouped by.
    let mut stats = view(Tab::Stats).with_period(UsagePeriod::Week);
    press(&mut stats, KeyCode::Char('r'), KeyModifiers::NONE);
    let shown = rows(&mut stats, &state, 120, 60);
    assert!(shown[2].contains("What the crew did"), "{shown:#?}");
    // The tabs with no periods leave the keys alone and say nothing of them.
    for tab in [Tab::Status, Tab::Context] {
        let mut view = view(tab);
        let on = press(&mut view, KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(asks(on).is_none(), "{tab:?}");
        let shown = rows(&mut view, &state, 140, 60);
        assert!(
            !shown.iter().any(|row| row.contains("r to cycle dates")),
            "{tab:?}: {shown:#?}"
        );
    }
}

/// The Overview has three periods of its own, which the same two keys go round.
#[test]
fn r_and_shift_r_go_round_the_overviews_own_periods() {
    let state = state(sample::usage());
    let mut overview = view(Tab::Overview);
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let asked = |outcome: ViewOutcome| match outcome {
        ViewOutcome::Request(ViewRequest::Overview { period, .. }) => Some(period),
        _ => None,
    };
    let r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
    let shift_r = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT);
    assert_eq!(
        asked(overview.handle_key(r, &ctx)),
        Some(OverviewPeriod::Week)
    );
    assert_eq!(
        asked(overview.handle_key(shift_r, &ctx)),
        Some(OverviewPeriod::All)
    );
    assert_eq!(
        asked(overview.handle_key(shift_r, &ctx)),
        Some(OverviewPeriod::Month)
    );
}

/// Usage takes a period with 1 to 4 from any row, as Stats and Limits do, and shows the four on
/// its tab row. Its key line names `r` alone, which cycles them.
#[test]
fn usage_takes_a_period_from_any_row() {
    let state = state(sample::usage());
    let mut usage = view(Tab::Usage);
    for (row, keys) in [("tabs", vec![]), ("CLIs", vec![KeyCode::Down])] {
        for code in &keys {
            key(&mut usage, &state, *code);
        }
        let asked = key(&mut usage, &state, KeyCode::Char('1'));
        assert!(
            matches!(
                asked,
                ViewOutcome::Request(ViewRequest::Usage {
                    period: UsagePeriod::Today
                })
            ),
            "{row}: {asked:?}"
        );
    }
    key(&mut usage, &state, KeyCode::Down);
    let asked = key(&mut usage, &state, KeyCode::Char('4'));
    assert!(
        matches!(
            asked,
            ViewOutcome::Request(ViewRequest::Usage {
                period: UsagePeriod::All
            })
        ),
        "the cards: {asked:?}"
    );
    let shown = rows(&mut usage, &state, 140, 60);
    assert!(shown[1].ends_with("Today  Week  Month  All"), "{shown:#?}");
    // Its cards leave the last column blank, as every other tab's do.
    for width in [100u16, 118, 120, 140] {
        let shown = rows(&mut usage, &state, width, 60);
        for row in shown.iter().skip(1) {
            assert!(
                row.chars().count() < usize::from(width),
                "{width}: {row:?}\n{shown:#?}"
            );
        }
    }
    let keys = shown.iter().rev().find(|row| !row.trim().is_empty());
    assert!(
        keys.is_some_and(|keys| {
            keys.trim_start().starts_with("r to cycle dates · ") && !keys.contains("1-4")
        }),
        "{shown:#?}"
    );
}

/// On Usage too the chosen CLI stays filled in the cards, where Tab still moves it.
#[test]
fn in_the_cards_of_usage_the_chosen_cli_stays_filled() {
    let state = state(sample::usage());
    let mut usage = view(Tab::Usage);
    into_the_cards(&mut usage, &state);
    let shown = rows(&mut usage, &state, 120, 80);
    assert!(shown[2].contains("[All CLIs]"), "{shown:#?}");
    key(&mut usage, &state, KeyCode::Tab);
    assert_eq!((usage.cli, usage.level()), (1, Level::Cards));
    let shown = rows(&mut usage, &state, 120, 80);
    assert!(!shown[2].contains("[All CLIs]"), "{shown:#?}");
    assert_eq!(shown[2].matches('[').count(), 1, "{shown:#?}");

    let theme = crate::overview::golden::theme();
    let buf = colour_buffer(&mut usage, &state, 120, 80);
    let orange = (0..120)
        .filter(|x| buf[(*x, 2)].bg == theme.color(Role::AgentOrange).unwrap())
        .count();
    assert!(orange > 0, "the chosen CLI is filled in orange");
}

/// Every tab opens on All, and the Overview on All time, whichever tab the panel opens on and
/// whichever the tabs were before; the periods at the right of the tabs say so.
#[test]
fn every_tab_opens_on_all() {
    for tab in Tab::ALL {
        let view = OverviewView::new(tab, UsageWatch::default());
        assert_eq!(
            (view.period, view.home.period),
            (UsagePeriod::All, OverviewPeriod::All),
            "{tab:?}"
        );
    }
    let mut all = sample::usage();
    all.period = UsagePeriod::All;
    let state = state(all);
    let mut limits = view(Tab::Limits);
    let buf = colour_buffer(&mut limits, &state, 120, 80);
    let theme = crate::overview::golden::theme();
    let row: Vec<&str> = (0..120).map(|x| buf[(x, 1)].symbol()).collect();
    let at = (0..118)
        .rev()
        .find(|x| row[*x..*x + 3].concat() == "All")
        .expect("the periods") as u16;
    assert_eq!(buf[(at, 1)].fg, theme.color(Role::Question).unwrap());
    assert!(
        buf[(at, 1)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
}
