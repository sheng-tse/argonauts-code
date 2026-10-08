use std::sync::Arc;

use argonauts_core::api::{StatusSummary, UsageSummary};
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

#[test]
fn the_usage_tab_stacks_its_cards_under_the_tabs_and_takes_only_their_rows() {
    let state = state(sample::usage());
    let mut usage = view(Tab::Usage);
    let rows = rows(&mut usage, &state, 110, 60);
    assert!(rows[0].chars().all(|c| c == '─'), "{rows:#?}");
    assert!(
        rows[1].starts_with("  Overview   Status  [Usage]  Context   Limits   Stats"),
        "{rows:#?}"
    );
    assert!(rows[1].ends_with("Today  Week  Month  All"), "{rows:#?}");
    assert!(
        rows[2].starts_with(" Plan usage, spend and tokens across the crew"),
        "{rows:#?}"
    );
    assert!(rows[2].ends_with("~$1.86 today · 1.4M tokens"), "{rows:#?}");
    let at = |text: &str| {
        rows.iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text}: {rows:#?}"))
    };
    let order = [
        at("◆ plan usage"),
        at("● Claude Code Max · in use · read 1m ago"),
        at("◆ spend"),
        at("◆ tokens"),
        at("◆ workflows"),
    ];
    assert!(order.is_sorted(), "{order:?}: {rows:#?}");
    assert_eq!(at("◆ tokens"), at("◆ history"), "side by side: {rows:#?}");
    assert!(
        rows.last().unwrap().starts_with(
            " ←→ tab · d w m a period · tab next card · p pin above the input · u used or left"
        ),
        "{rows:#?}"
    );
    assert!(
        rows.len() < 40,
        "it takes the rows it needs, not the screen"
    );
}

#[test]
fn a_narrow_panel_stacks_tokens_over_history_and_scrolls_what_does_not_fit() {
    let state = state(sample::usage());
    let mut usage = view(Tab::Usage);
    let shown = rows(&mut usage, &state, 80, 24);
    assert!(shown.len() <= 18, "{shown:#?}");
    assert!(
        shown.last().unwrap().starts_with(" ↑↓ scroll · "),
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|row| row.contains("◆ workflows")));
    // ↓ moves a card at a time, and stops at the last.
    for _ in 0..9 {
        key(&mut usage, &state, KeyCode::Down);
    }
    let scrolled = rows(&mut usage, &state, 80, 24);
    assert!(
        scrolled.iter().any(|row| row.contains("◆ workflows")),
        "{scrolled:#?}"
    );
}

#[test]
fn the_keys_change_tab_period_units_and_pin_the_card_in_focus() {
    let state = state(sample::usage());
    let mut overview = view(Tab::Usage);
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Char('w')),
        ViewOutcome::Request(ViewRequest::Usage {
            period: UsagePeriod::Week
        })
    ));
    // The week's numbers aren't in yet, and yesterday's totals never stand in for them.
    let waiting = rows(&mut overview, &state, 110, 60);
    assert!(waiting.iter().any(|row| row.contains("Reading usage…")));
    key(&mut overview, &state, KeyCode::Char('d'));

    key(&mut overview, &state, KeyCode::Char('u'));
    let left = rows(&mut overview, &state, 110, 60);
    assert!(left.iter().any(|row| row.contains(" 29%")), "{left:#?}");

    key(&mut overview, &state, KeyCode::Tab);
    let pinned = key(&mut overview, &state, KeyCode::Char('p'));
    let ViewOutcome::Send(UiCommand::Slash {
        command:
            SlashCommand::Widgets {
                action: WidgetsAction::Change { widget, change },
                ..
            },
    }) = pinned
    else {
        panic!("{pinned:?}");
    };
    assert_eq!(widget, UsageCard::Spend.name());
    assert_eq!(
        change,
        WidgetChange::Toggle {
            place: Place::AboveInput
        }
    );

    assert!(matches!(
        key(&mut overview, &state, KeyCode::Right),
        ViewOutcome::Request(ViewRequest::Context)
    ));
    assert_eq!(overview.tab(), Tab::Context);
    for _ in 0..6 {
        key(&mut overview, &state, KeyCode::Left);
    }
    assert_eq!(overview.tab(), Tab::Overview, "← stops at the first tab");
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
        ViewOutcome::Request(ViewRequest::Status)
    ));
    let waiting = rows(&mut overview, &state, 110, 60);
    assert!(waiting[1].contains("[Status]"), "{waiting:#?}");
    assert!(waiting.iter().any(|row| row.contains("Reading status…")));

    assert!(overview.accept(&ViewData::Status {
        summary: StatusSummary::default(),
    }));
    let idle = rows(&mut overview, &state, 110, 60);
    assert!(
        idle.iter().any(|row| row.contains("Nothing running")),
        "{idle:#?}"
    );

    assert!(overview.accept(&ViewData::Status {
        summary: sample::status(),
    }));
    let shown = rows(&mut overview, &state, 110, 60);
    assert!(shown[2].ends_with("run-03 · $1.20"), "{shown:#?}");
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
        keys.contains("↑↓ task · enter agent · tab next card · p pin above the input"),
        "{keys}"
    );

    // Narrow, the two stack.
    let narrow = rows(&mut overview, &state, 80, 60);
    let at = |what: &str| narrow.iter().position(|row| row.contains(what)).unwrap();
    assert!(at("◆ spend") < at("◆ plan usage"), "{narrow:#?}");

    assert!(matches!(
        key(&mut overview, &state, KeyCode::Char('s')),
        ViewOutcome::SendToChat(UiCommand::Summary)
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
    use argonauts_core::{CardId, RunId};

    use crate::state::{RunState, SessionState, SessionStatus};

    let mut state = state(sample::usage());
    let id = RunId::new("run-03").expect("a run id");
    let mut run = RunState::new(id.clone());
    let key = SessionKey::numbered(7);
    let worker = "claude-code/claude-opus-5-5".parse().expect("a worker");
    let mut session = SessionState::new(
        key.clone(),
        worker,
        String::new(),
        RoleName::Worker,
        sample::now(),
    );
    session.card = Some(CardId::numbered(2));
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
    // The arrows stop at the last task.
    for _ in 0..9 {
        key(&mut status, &state, KeyCode::Down);
    }
    let shown = rows(&mut status, &state, 110, 60);
    assert!(picked(&shown).contains("task-004  Review"), "{shown:#?}");

    // On another card no task is picked, the rows stay where they were, and Enter opens
    // the workflow.
    let before = shown
        .iter()
        .position(|row| row.contains("task-002"))
        .unwrap();
    key(&mut status, &state, KeyCode::Tab);
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
                SlashCommand::Widgets {
                    action: WidgetsAction::Change { widget, change },
                    ..
                },
        }) => {
            assert_eq!(
                change,
                WidgetChange::Toggle {
                    place: Place::AboveInput
                }
            );
            widget
        }
        other => panic!("{other:?}"),
    };
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(pinned(key(&mut status, &state, KeyCode::Char('p'))));
        key(&mut status, &state, KeyCode::Tab);
    }
    use WidgetName::{Budget, Quota, Run, Waiting};
    assert_eq!(seen, [Run, Budget, Quota, Waiting]);

    // On a short screen the card Tab moved to scrolls into view.
    let mut status = view(Tab::Status).with_status(sample::status());
    let short = rows(&mut status, &state, 110, 24);
    assert!(
        !short.iter().any(|row| row.contains("◆ needs you")),
        "{short:#?}"
    );
    for _ in 0..3 {
        key(&mut status, &state, KeyCode::Tab);
    }
    let short = rows(&mut status, &state, 110, 24);
    assert!(
        short.iter().any(|row| row.contains("◆ needs you")),
        "{short:#?}"
    );
    key(&mut status, &state, KeyCode::Tab);
    let short = rows(&mut status, &state, 110, 24);
    assert!(
        short.iter().any(|row| row.contains("◆ run-03")),
        "{short:#?}"
    );
}

#[test]
fn context_shows_the_picked_session_over_the_list_and_the_arrows_pick() {
    let state = state(sample::usage());
    let mut overview = view(Tab::Context).with_context(sample::context());
    let shown = rows(&mut overview, &state, 120, 60);
    assert!(shown[1].contains("[Context]"), "{shown:#?}");
    assert!(
        shown[2].ends_with("3 sessions · 291k in context"),
        "{shown:#?}"
    );
    assert!(
        shown
            .iter()
            .any(|row| row.contains("◆ context · main · chat")),
        "{shown:#?}"
    );
    assert!(shown.iter().any(|row| row.contains("❯ main · chat")));
    assert!(
        shown
            .last()
            .unwrap()
            .contains("↑↓ session · enter agent view")
    );

    assert!(matches!(
        key(&mut overview, &state, KeyCode::Enter),
        ViewOutcome::Notice(_)
    ));
    key(&mut overview, &state, KeyCode::Down);
    let moved = rows(&mut overview, &state, 120, 60);
    assert!(
        moved
            .iter()
            .any(|row| row.contains("◆ context · build:report-endpoint")),
        "{moved:#?}"
    );
    assert!(matches!(
        key(&mut overview, &state, KeyCode::Enter),
        ViewOutcome::Request(ViewRequest::Agent { .. })
    ));
    for _ in 0..5 {
        key(&mut overview, &state, KeyCode::Down);
    }
    let last = rows(&mut overview, &state, 120, 60);
    assert!(
        last.iter().any(|row| row.contains("❯ build:docs")),
        "{last:#?}"
    );

    // On a short screen the list stays, since the keys work on it.
    let short = rows(&mut overview, &state, 120, 20);
    assert!(
        short.iter().any(|row| row.contains("◆ sessions")),
        "{short:#?}"
    );
    assert!(
        !short.iter().any(|row| row.contains("◆ context ·")),
        "{short:#?}"
    );
}

#[test]
fn on_context_a_lists_every_item_of_every_category_and_a_again_the_largest() {
    let mut state = state(sample::usage());
    state.chat.context = Some(sample::breakdown());
    let mut context = view(Tab::Context).with_context(sample::context());
    let few = rows(&mut context, &state, 110, 80);
    assert!(
        few.iter().any(|row| row.contains("Skills · 3 ")),
        "{few:#?}"
    );
    assert!(few.last().unwrap().contains("a every item"), "{few:#?}");

    assert!(matches!(
        key(&mut context, &state, KeyCode::Char('a')),
        ViewOutcome::Consumed
    ));
    let all = rows(&mut context, &state, 110, 80);
    assert!(
        all.iter().any(|row| row.contains("Skills · 3 · 2.2k")),
        "{all:#?}"
    );
    assert!(all.len() > few.len(), "a row an item: {all:#?}");
    assert!(
        all.last().unwrap().contains("a the largest items"),
        "{all:#?}"
    );

    key(&mut context, &state, KeyCode::Char('a'));
    assert_eq!(rows(&mut context, &state, 110, 80), few);
    // A refresh of the open tab keeps what `a` chose.
    key(&mut context, &state, KeyCode::Char('a'));
    assert!(context.accept(&ViewData::Context {
        view: sample::context()
    }));
    assert_eq!(rows(&mut context, &state, 110, 80), all);
}

#[test]
fn the_limits_tab_lists_the_limits_and_enter_opens_the_picked_one_to_change() {
    let state = state(sample::usage());
    let mut overview = view(Tab::Limits);
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
    assert!(
        shown.last().unwrap().contains("↑↓ limit · enter change"),
        "{shown:#?}"
    );
    // The picker opens under the input, so the panel closes as it sends.
    let opened = |outcome: ViewOutcome| match outcome {
        ViewOutcome::SendToChat(UiCommand::Slash {
            command:
                SlashCommand::Budget {
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
    let mut short = view(Tab::Limits);
    let hidden = rows(&mut short, &state, 120, 20);
    assert!(!hidden.iter().any(|row| row.contains("❯")), "{hidden:#?}");
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
    let mut overview = view(Tab::Limits);
    let shown = rows(&mut overview, &state, 120, 60);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("Enter opens the limits")),
        "{shown:#?}"
    );
    assert_eq!(opened(key(&mut overview, &state, KeyCode::Enter)), None);
    // On the other usage tabs Enter has nothing to open.
    let mut usage = view(Tab::Usage);
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
        let task = crate::state::CardRow::new(argonauts_core::CardId::numbered(n), title);
        run.cards.insert(task.id.clone(), task);
    }
    for session in run.sessions.values_mut() {
        session.cost_usd = Some(0.62);
    }
    state.attached = Some(id);
    let mut overview = view(Tab::Limits);
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

    // Once it has, the run's spend against its own limit leads the tab.
    let id = argonauts_core::RunId::new("run-03").expect("a run id");
    state.runs.get_mut(&id).expect("the run").budget = sample::status_with_limit().budget;
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
    let mut overview = view(Tab::Overview);
    // The way out shows whole at any width; the keys before it give way.
    for width in [200, 110, 80, 60] {
        let shown = rows(&mut overview, &state, width, 40);
        let keys = shown.last().unwrap();
        assert!(
            keys.ends_with("esc closes this · the answer keeps running"),
            "{width}: {shown:#?}"
        );
        assert!(keys.starts_with(" ←→ tab · "), "{width}: {shown:#?}");
    }
    let shown = rows(&mut overview, &state, 200, 40);
    assert!(shown.iter().any(|row| row.contains("◆ workflows")));
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
