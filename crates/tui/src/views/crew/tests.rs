use argonauts_core::crew_panel::{CrewDetails, CrewRun};
use argonauts_core::limits::{QuotaReading, QuotaStatus};
use argonauts_core::names::ChatId;
use serde_json::json;
use time::OffsetDateTime;

use super::*;
use crate::theme::Theme;

fn at(minutes_ago: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_000 - minutes_ago * 60).unwrap()
}

fn now() -> OffsetDateTime {
    at(0)
}

fn row(title: &str, state: CrewState, latest: &str) -> CrewRow {
    CrewRow {
        target: CrewTarget::Conversation {
            chat: ChatId::numbered(1),
        },
        state,
        title: title.into(),
        repo: "~/app".into(),
        run: None,
        worker: Some("antigravity-cli/gemini-3.6-pro".parse().unwrap()),
        latest: latest.into(),
        updated: at(12),
        details: CrewDetails::default(),
    }
}

fn press(view: &mut CrewView, key: KeyEvent, state: &AppState) -> ViewOutcome {
    let theme = Theme::dark();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.handle_key(key, &ctx)
}

fn char_key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn ctrl_letters_are_left_to_the_global_keys() {
    let state = AppState::default();
    let mut view = CrewView::new(&state);
    for c in ['x', ' ', '?', 'j'] {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert!(
            matches!(press(&mut view, key, &state), ViewOutcome::Ignored),
            "Ctrl+{c}"
        );
    }
    assert!(!view.details && !view.keys && view.confirm_stop.is_none());
}

#[test]
fn opened_with_left_it_closes_with_right_and_opens_with_enter() {
    let state = crate::mentions::fixture::working_state();
    let mut view = CrewView::opened_left(&state);
    assert!(!matches!(
        press(&mut view, KeyEvent::from(KeyCode::Enter), &state),
        ViewOutcome::Ignored | ViewOutcome::Close
    ));
    let mut view = CrewView::opened_left(&state);
    assert!(matches!(
        press(&mut view, KeyEvent::from(KeyCode::Right), &state),
        ViewOutcome::Close
    ));
    // Opened any other way, → opens the row.
    let mut view = CrewView::new(&state);
    assert!(!matches!(
        press(&mut view, KeyEvent::from(KeyCode::Right), &state),
        ViewOutcome::Ignored | ViewOutcome::Close
    ));
}

#[test]
fn letter_keys_move_open_and_go_back() {
    let mut state = crate::mentions::fixture::working_state();
    state.ui = list::keys_state(true).ui;
    let mut view = CrewView::new(&state);
    let count = ordered(&view.panel(&state), view.tab).len();
    assert!(count >= 2);
    press(&mut view, char_key('j'), &state);
    assert_eq!(view.list.selected, 1);
    press(&mut view, char_key('G'), &state);
    assert_eq!(view.list.selected, count - 1);
    press(&mut view, char_key('g'), &state);
    assert_eq!(view.list.selected, 0);
    assert!(!matches!(
        press(&mut view, char_key('l'), &state),
        ViewOutcome::Ignored | ViewOutcome::Close
    ));
    assert!(matches!(
        press(&mut view, char_key('h'), &state),
        ViewOutcome::Close
    ));

    // Without vim keys j and k still move, the other letters do nothing, and ← goes back.
    state.ui = list::keys_state(false).ui;
    let mut view = CrewView::new(&state);
    press(&mut view, char_key('j'), &state);
    assert_eq!(view.list.selected, 1);
    press(&mut view, char_key('k'), &state);
    assert_eq!(view.list.selected, 0);
    assert!(matches!(
        press(&mut view, char_key('l'), &state),
        ViewOutcome::Ignored
    ));
    assert!(matches!(
        press(&mut view, KeyEvent::from(KeyCode::Left), &state),
        ViewOutcome::Close
    ));
}

#[test]
fn the_controller_row_for_a_run_on_screen_gives_way_to_its_agents() {
    let mut state = crate::mentions::fixture::working_state();
    state.welcome = Some(crate::state::Welcome {
        version: "0.1.0".into(),
        repo: "~/app".into(),
        preset: "solo".into(),
        default_agent: None,
        agents: Vec::new(),
        runs_going: 1,
        example_prompt: None,
    });
    let held = state.runs.keys().next().unwrap().clone();
    let run_row = |id: &RunId, repo: &str| CrewRow {
        target: CrewTarget::Run { run: id.clone() },
        run: Some(CrewRun {
            id: id.clone(),
            title: "A run".into(),
        }),
        repo: repo.into(),
        ..row("A run", CrewState::Working, "running")
    };
    let other = RunId::new("run-09").unwrap();
    state.crew_panel = Some(CrewPanel {
        repo: "/home/me/app".into(),
        rows: vec![
            run_row(&held, "/home/me/app"),
            run_row(&other, "/home/me/app"),
            CrewRow {
                repo: "/home/me/app".into(),
                ..row("Chat", CrewState::Ready, "hi")
            },
        ],
    });
    let panel = rows::panel(&state, now());
    let runs: Vec<&RunId> = panel
        .rows
        .iter()
        .filter_map(|row| match &row.target {
            CrewTarget::Run { run } => Some(run),
            _ => None,
        })
        .collect();
    assert_eq!(runs, [&other]);
    // The controller's name for this repo becomes the screen's, so they share a group.
    assert_eq!(panel.repo, "~/app");
    assert!(panel.rows.iter().all(|row| row.repo == "~/app"));
    assert!(panel.rows.iter().any(|row| row.title == "Chat"));
}

#[test]
fn the_agent_gives_way_first_and_times_line_up_at_the_right() {
    let rows = [
        row(
            "Explain the input handling",
            CrewState::Ready,
            "It reads keys",
        ),
        row("Fix it", CrewState::Done, "merged"),
    ];
    let refs: Vec<&CrewRow> = rows.iter().collect();
    let columns = |room| {
        let columns = draw::row_columns(&refs, room, now(), &Default::default());
        let names: Vec<&str> = columns.iter().map(|c| c.heading).collect();
        (names.join(" "), list::total_width(&columns, draw::GAP))
    };
    assert_eq!(columns(120), ("title worker latest ago".to_owned(), 120));
    assert_eq!(columns(60), ("title worker latest ago".to_owned(), 60));
    assert_eq!(columns(46), ("title latest ago".to_owned(), 46));
    assert_eq!(columns(34), ("title latest".to_owned(), 34));
}

#[test]
fn long_paths_in_the_details_are_cut_in_the_middle() {
    let mut row = row("Fix it", CrewState::Working, "Editing");
    row.repo = "/Users/you/projects/github/argonauts/playground/demo/repo-c100".into();
    row.details.branch = Some("argonauts/run-20261002-101116-dd4b/card-002".into());
    let lines = draw::details_lines(&row, false, 40, &Default::default(), &Theme::dark());
    let text: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    // A path broken across lines mid-name reads as two; its ends are what tell it apart.
    assert!(
        text.contains(&"Repo     /…/playground/demo/repo-c100".to_owned()),
        "{text:#?}"
    );
    assert!(
        text.contains(&"Branch   argonauts/…/card-002".to_owned()),
        "{text:#?}"
    );
    assert!(
        text.iter().all(|line| crate::text::str_width(line) <= 40),
        "{text:#?}"
    );
}

fn reading(status: QuotaStatus, used: Option<f64>) -> QuotaReading {
    serde_json::from_value(json!({"vendor": "codex", "status": status, "used": used,
        "resets_at": null, "window": "five_hour", "source": "rate_limit_event",
        "observed_at": "2026-09-30T11:59:00Z", "detail": null}))
    .unwrap()
}

#[test]
fn quotas_read_as_one_short_part_each() {
    let part = |status, used| draw::quota_part("Codex", &reading(status, used));
    assert_eq!(
        part(QuotaStatus::Allowed, Some(0.12)),
        ("Codex 12%".to_owned(), Role::Dim)
    );
    assert_eq!(
        part(QuotaStatus::Allowed, Some(0.93)),
        ("Codex 93%".to_owned(), Role::Warning)
    );
    assert_eq!(
        part(QuotaStatus::Rejected, Some(1.0)),
        ("Codex out".to_owned(), Role::Error)
    );
    assert_eq!(
        part(QuotaStatus::Unknown, None),
        ("Codex ?".to_owned(), Role::Dim)
    );

    let mut state = AppState::default();
    state.quotas.insert(
        "codex".into(),
        vec![reading(QuotaStatus::Allowed, Some(0.4))],
    );
    let line = draw::title_line(&state, 80, &Theme::dark());
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.starts_with("argonauts › crew") && text.ends_with("Codex 40%"));
    assert_eq!(crate::text::str_width(&text), 80);
    // With no room the quotas give way and the name stays.
    let line = draw::title_line(&state, 20, &Theme::dark());
    assert_eq!(line.spans.len(), 1);
}

#[test]
fn every_state_has_its_own_glyph() {
    for theme in [Theme::dark(), {
        let mut ascii = Theme::dark();
        ascii.glyphs = crate::theme::Glyphs::ASCII;
        ascii
    }] {
        let states = [
            CrewState::NeedsYou,
            CrewState::Working,
            CrewState::Waiting,
            CrewState::Ready,
            CrewState::Done,
            CrewState::Stopped,
            CrewState::Failed,
        ];
        let glyphs: std::collections::BTreeSet<&str> = states
            .iter()
            .map(|&state| draw::glyph(state, false, &theme).0)
            .collect();
        assert_eq!(glyphs.len(), states.len(), "{glyphs:?}");
    }
}

#[test]
fn c_carries_a_finished_agent_on_and_leaves_a_working_one_to_take_over() {
    let state = AppState::default();
    let panel = CrewPanel {
        repo: "~/app".into(),
        rows: Vec::new(),
    };
    let agent = |state: CrewState| CrewRow {
        target: CrewTarget::Agent {
            run: RunId::new("run-01").unwrap(),
            session: SessionKey::numbered(1),
        },
        ..row("Fix the parser", state, "Fixed both functions.")
    };
    // This screen hasn't loaded run-01, so the app says whether its run ended.
    match carry_on(&state, &panel, &agent(CrewState::Done)) {
        ViewOutcome::SendToChat(UiCommand::Slash {
            command:
                SlashCommand::Resume {
                    target: Some(ResumeTarget::Agent(agent)),
                },
        }) => assert_eq!(agent.to_string(), "run-01/s-001"),
        other => panic!("{other:?}"),
    }
    match carry_on(&state, &panel, &agent(CrewState::Working)) {
        ViewOutcome::Notice(text) => {
            assert_eq!(
                text,
                "s-001 is still working. t in its agent view takes it over"
            );
        }
        other => panic!("{other:?}"),
    }
    let conversation = row("hi", CrewState::Ready, "");
    assert!(matches!(
        carry_on(&state, &panel, &conversation),
        ViewOutcome::Notice(_)
    ));

    // A run on screen that hasn't ended still owns its finished agents' sessions.
    let mut going = AppState::default();
    let run = RunId::new("run-01").unwrap();
    going
        .runs
        .insert(run.clone(), crate::state::RunState::new(run));
    match carry_on(&going, &panel, &agent(CrewState::Done)) {
        ViewOutcome::Notice(text) => assert_eq!(
            text,
            "run-01 hasn't ended, so it still owns s-001. t takes it over, or it carries on \
             once the run ends"
        ),
        other => panic!("{other:?}"),
    }

    // Another repo's run carries on from an app open there, never by this repo's run-01.
    let elsewhere = CrewRow {
        repo: "~/web".into(),
        ..agent(CrewState::Done)
    };
    match carry_on(&state, &panel, &elsewhere) {
        ViewOutcome::Notice(text) => {
            assert_eq!(
                text,
                "run-01 runs in ~/web. Open argonauts there to carry it on"
            );
        }
        other => panic!("{other:?}"),
    }
}
