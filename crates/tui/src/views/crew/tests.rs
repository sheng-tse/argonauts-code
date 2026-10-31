use argonauts_core::app::agent_panel::{CrewDetails, CrewRun};
use argonauts_core::base::names::ConversationId;
use argonauts_core::models::limits::{PlanUsageReading, PlanUsageStatus};
use serde_json::json;
use time::OffsetDateTime;

use super::*;
use crate::base::theme::Theme;

fn at(minutes_ago: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_000 - minutes_ago * 60).unwrap()
}

fn now() -> OffsetDateTime {
    at(0)
}

fn row(title: &str, state: CrewState, latest: &str) -> CrewRow {
    CrewRow {
        target: CrewTarget::Conversation {
            conversation: ConversationId::numbered(1),
        },
        state,
        title: title.into(),
        label: None,
        tries: 0,
        repo: "~/app".into(),
        run: None,
        worker: Some("antigravity-cli/gemini-3.6-pro".parse().unwrap()),
        latest: latest.into(),
        updated: at(12),
        details: CrewDetails::default(),
    }
}

fn press(view: &mut AgentPanelView, key: KeyEvent, state: &AppState) -> ViewOutcome {
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
    let mut view = AgentPanelView::new(&state);
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
    let state = crate::bridge::mentions::fixture::working_state();
    let mut view = AgentPanelView::opened_left(&state);
    assert!(!matches!(
        press(&mut view, KeyEvent::from(KeyCode::Enter), &state),
        ViewOutcome::Ignored | ViewOutcome::Close
    ));
    let mut view = AgentPanelView::opened_left(&state);
    assert!(matches!(
        press(&mut view, KeyEvent::from(KeyCode::Right), &state),
        ViewOutcome::Close
    ));
    // Opened any other way, → opens the row.
    let mut view = AgentPanelView::new(&state);
    assert!(!matches!(
        press(&mut view, KeyEvent::from(KeyCode::Right), &state),
        ViewOutcome::Ignored | ViewOutcome::Close
    ));
}

#[test]
fn letter_keys_move_open_and_go_back() {
    let mut state = crate::bridge::mentions::fixture::working_state();
    state.ui = list::keys_state(true).ui;
    let mut view = AgentPanelView::new(&state);
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
    let mut view = AgentPanelView::new(&state);
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
    let mut state = crate::bridge::mentions::fixture::working_state();
    state.welcome = Some(crate::frame::state::Welcome {
        version: "0.1.0".into(),
        repo: "~/app".into(),
        workflow: "captain".into(),
        main_agent: None,
        agents: Vec::new(),
        runs_going: 1,
        runs_need_you: 0,
        example_prompt: None,
    });
    let held = state.runs.keys().next().unwrap().clone();
    let run_row = |id: &RunId, repo: &str| CrewRow {
        target: CrewTarget::Run { run: id.clone() },
        run: Some(CrewRun::new(id.clone(), "A run")),
        repo: repo.into(),
        ..row("A run", CrewState::Working, "running")
    };
    let other = RunId::new("run-09").unwrap();
    state.agent_panel = Some(AgentPanelRows {
        repo: "/home/me/app".into(),
        rows: vec![
            run_row(&held, "/home/me/app"),
            run_row(&other, "/home/me/app"),
            CrewRow {
                repo: "/home/me/app".into(),
                ..row("Conversation", CrewState::Ready, "hi")
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
    assert!(panel.rows.iter().any(|row| row.title == "Conversation"));
}

fn agent_row(label: &str, state: CrewState, latest: &str) -> CrewRow {
    CrewRow {
        target: CrewTarget::Agent {
            run: RunId::new("run-01").unwrap(),
            session: SessionKey::numbered(1),
        },
        label: Some(label.into()),
        run: Some(CrewRun::new(RunId::new("run-01").unwrap(), "Add due dates")),
        ..row("Add due dates", state, latest)
    }
}

#[test]
fn who_and_when_show_only_in_room_the_name_and_the_last_line_leave() {
    let rows = [
        agent_row(
            "build:add-due",
            CrewState::Done,
            "Implemented optional due dates",
        ),
        agent_row("signoff:judge", CrewState::Done, "All 5 criteria met"),
    ];
    let refs: Vec<&CrewRow> = rows.iter().collect();
    let columns = |room| {
        let columns = draw::row_columns(&refs, room, now(), &Default::default());
        let names: Vec<&str> = columns.iter().map(|c| c.heading).collect();
        (names.join(" "), list::total_width(&columns, draw::GAP))
    };
    assert_eq!(columns(120), ("name latest worker ago".to_owned(), 120));
    assert_eq!(columns(60), ("name latest ago".to_owned(), 60));
    assert_eq!(columns(46), ("name latest".to_owned(), 46));
    assert_eq!(columns(30), ("name latest".to_owned(), 30));
}

fn text_of(lines: &[draw::BodyLine]) -> Vec<String> {
    lines
        .iter()
        .map(|body| {
            let text: String = body
                .line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            text.trim_end().to_owned()
        })
        .collect()
}

/// The list: the run's name once with where it stands, each
/// agent by its label and last line, retries folded, and the conversations under Sessions.
#[test]
fn the_list_reads_like_the_picture() {
    let mut run = CrewRun::new(RunId::new("run-01").unwrap(), "Add due dates to the tasks");
    run.name = Some("add-due-dates".into());
    run.stands = Some(argonauts_core::app::agent_panel::RunStands {
        state: CrewState::Done,
        word: "merged".into(),
        since: at(13),
        spent: Some(argonauts_core::Spend {
            cost_usd: 0.23,
            cost_estimated: true,
            ..Default::default()
        }),
    });
    let agent = |label: &str, latest: &str, minutes_ago: i64, tries: u32| CrewRow {
        run: Some(run.clone()),
        updated: at(minutes_ago),
        tries,
        ..agent_row(label, CrewState::Done, latest)
    };
    let panel = AgentPanelRows {
        repo: "~/app".into(),
        rows: vec![
            agent("plan:orchestrator", "accepted on its 6th try", 20, 6),
            agent("build:add-due", "Implemented optional due dates", 16, 0),
            agent(
                "review:add-due",
                "The implementation cleanly adds it",
                15,
                0,
            ),
            agent("signoff:judge", "All 5 criteria met", 14, 0),
            CrewRow {
                worker: Some("claude-code/opus".parse().unwrap()),
                ..row("Add due dates to the tasks", CrewState::Ready, "Done.")
            },
        ],
    };
    let lines = draw::list_lines(
        &panel.groups(CrewTab::All),
        &panel.repo,
        &[],
        2,
        51,
        now(),
        &Default::default(),
        &Theme::dark(),
    );
    assert_eq!(
        text_of(&lines),
        [
            "✓ add-due-dates · merged 13m ago · ~$0.23",
            "    ✓ signoff:judge         All 5 criteria met",
            "    ✓ review:add-due        The implementation cle…",
            "  ❯ ✓ build:add-due         Implemented optional d…",
            "    ✓ plan:orchestrator ×6  accepted on its 6th try",
            "Sessions",
            "    ◎ Add due dates to th…  Claude Code · enter co…",
        ]
    );
    let rows: Vec<Option<usize>> = lines.iter().map(|line| line.row).collect();
    assert_eq!(
        rows,
        [None, Some(0), Some(1), Some(2), Some(3), None, Some(4)]
    );
    // And its colors: the run's mark green and its name bold white, where it stands dim, the
    // chosen row lavender from its pointer to its last line, and a conversation's ring
    // lavender too.
    let theme = Theme::dark();
    let fg = |role| theme.style(role).fg;
    let fg_of = |line: usize, part: &str| {
        lines[line]
            .line
            .spans
            .iter()
            .find(|span| span.content.contains(part))
            .and_then(|span| span.style.fg)
    };
    assert_eq!(fg_of(0, "✓"), fg(Role::Success));
    assert_eq!(fg_of(0, "add-due-dates"), fg(Role::Bright));
    assert_eq!(fg_of(0, "merged"), fg(Role::Dim));
    assert_eq!(fg_of(1, "✓"), fg(Role::Success));
    for part in ["❯", "✓", "build:add-due", "Implemented"] {
        assert_eq!(fg_of(3, part), fg(Role::Question), "{part}");
    }
    assert_eq!(fg_of(4, "×6"), fg(Role::Dim));
    assert_eq!(fg_of(5, "Sessions"), fg(Role::Dim));
    assert_eq!(fg_of(6, "◎"), fg(Role::Question));
}

/// A run with none of its agents listed, such as another repo's, is one row: its heading.
#[test]
fn a_run_listed_whole_is_its_own_heading_and_another_repo_is_named() {
    let whole = |id: &str, repo: &str, word: &str, state: CrewState| {
        let mut run = CrewRun::new(RunId::new(id).unwrap(), "Tidy the README");
        run.stands = Some(argonauts_core::app::agent_panel::RunStands {
            state,
            word: word.into(),
            since: at(120),
            spent: None,
        });
        CrewRow {
            target: CrewTarget::Run {
                run: RunId::new(id).unwrap(),
            },
            run: Some(run),
            repo: repo.into(),
            ..row("Tidy the README", state, word)
        }
    };
    let panel = AgentPanelRows {
        repo: "~/app".into(),
        rows: vec![
            whole("run-02", "~/app", "merged", CrewState::Done),
            whole("run-07", "~/api", "running", CrewState::Working),
        ],
    };
    let draw = |selected| {
        text_of(&draw::list_lines(
            &panel.groups(CrewTab::All),
            &panel.repo,
            &[],
            selected,
            60,
            now(),
            &Default::default(),
            &Theme::dark(),
        ))
    };
    assert_eq!(
        draw(1),
        [
            "~/app",
            "✓ Tidy the README · merged 2h ago",
            "",
            "~/api",
            "❯ Tidy the README · running",
        ]
    );
    assert_eq!(draw(0)[1], "❯ Tidy the README · merged 2h ago");
}

#[test]
fn long_paths_in_the_details_are_cut_in_the_middle() {
    let mut row = row("Fix it", CrewState::Working, "Editing");
    row.repo = "/Users/you/projects/github/argonauts/playground/demo/repo-c100".into();
    row.details.branch = Some("argonauts/run-20261002-101116-dd4b/task-002".into());
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
        text.contains(&"Branch   argonauts/…/task-002".to_owned()),
        "{text:#?}"
    );
    assert!(
        text.iter()
            .all(|line| crate::base::text::str_width(line) <= 40),
        "{text:#?}"
    );
}

fn reading(status: PlanUsageStatus, used: Option<f64>) -> PlanUsageReading {
    serde_json::from_value(json!({"agent_id": "codex", "status": status, "used": used,
        "resets_at": null, "window": "five_hour", "source": "rate_limit_event",
        "observed_at": "2026-09-30T11:59:00Z", "detail": null}))
    .unwrap()
}

#[test]
fn plan_usages_read_as_one_short_part_each() {
    let part = |status, used| draw::plan_usage_part("Codex", &reading(status, used));
    assert_eq!(
        part(PlanUsageStatus::Allowed, Some(0.12)),
        ("Codex 12%".to_owned(), Role::Dim)
    );
    assert_eq!(
        part(PlanUsageStatus::Allowed, Some(0.93)),
        ("Codex 93%".to_owned(), Role::Warn)
    );
    assert_eq!(
        part(PlanUsageStatus::Rejected, Some(1.0)),
        ("Codex out".to_owned(), Role::Danger)
    );
    let finishing = PlanUsageReading {
        wrap_up: true,
        ..reading(PlanUsageStatus::Rejected, Some(1.0))
    };
    assert_eq!(
        draw::plan_usage_part("Claude", &finishing),
        ("Claude wrapping up".to_owned(), Role::Warn)
    );
    assert_eq!(
        part(PlanUsageStatus::Unknown, None),
        ("Codex ?".to_owned(), Role::Dim)
    );

    let mut state = AppState::default();
    state.plan_windows.insert(
        "codex".into(),
        vec![reading(PlanUsageStatus::Allowed, Some(0.4))],
    );
    let line = draw::title_line(&state, 80, &Theme::dark());
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.starts_with("argonauts › crew") && text.ends_with("Codex 40%"));
    assert_eq!(crate::base::text::str_width(&text), 80);
    // With no room the plan usage gives way and the name stays.
    let line = draw::title_line(&state, 20, &Theme::dark());
    assert_eq!(line.spans.len(), 1);
}

#[test]
fn every_state_has_its_own_glyph() {
    for theme in [Theme::dark(), {
        let mut ascii = Theme::dark();
        ascii.glyphs = crate::base::theme::Glyphs::ASCII;
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
    let panel = AgentPanelRows {
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
        ViewOutcome::SendToConversation(UiCommand::Slash {
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
        .insert(run.clone(), crate::frame::state::RunState::new(run));
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

fn hints(footer: &Footer) -> Vec<String> {
    match footer {
        Footer::Hints(hints) => hints.iter().map(|(hint, _)| hint.clone()).collect(),
        Footer::Line(_) => Vec::new(),
    }
}

/// Enter on any agent opens its transcript, finished or running, even once its run merged,
/// and only a working agent offers `x stop`.
#[test]
fn enter_opens_any_agents_transcript_finished_or_running() {
    let mut state = crate::bridge::mentions::fixture::working_state();
    let run = state.attached.clone().unwrap();
    let held = |state: &AppState| {
        let panel = rows::panel(state, now());
        let agents: Vec<(CrewRow, bool)> = ordered(&panel, CrewTab::All)
            .into_iter()
            .filter_map(|row| match &row.target {
                CrewTarget::Agent { run, session } => Some((
                    row.clone(),
                    state.session(run, session).unwrap().is_running(),
                )),
                _ => None,
            })
            .collect();
        (panel, agents)
    };
    let (panel, agents) = held(&state);
    assert!(
        agents.iter().any(|(_, running)| !running),
        "one has finished"
    );
    let view = AgentPanelView::new(&state);
    for (row, running) in &agents {
        assert!(
            matches!(view.open(&state, &panel, row), ViewOutcome::Open(_)),
            "{}",
            row.title
        );
        let offered = hints(&view.footer(&state, Some(row)));
        assert_eq!(offered.iter().any(|hint| hint == "x stop"), *running);
    }

    state.runs.get_mut(&run).unwrap().finished =
        Some(argonauts_core::runs::events::run::RunOutcome::Merged);
    for session in state.runs.get_mut(&run).unwrap().sessions.values_mut() {
        session.status = SessionStatus::Done;
    }
    let (panel, agents) = held(&state);
    for (row, _) in &agents {
        assert!(matches!(
            view.open(&state, &panel, row),
            ViewOutcome::Open(_)
        ));
        assert!(!hints(&view.footer(&state, Some(row))).contains(&"x stop".to_owned()));
    }
}

/// An agent the screen doesn't hold opens from its run's log, and a run that finished shows
/// its workflow, whose agents open the same way, rather than trying to resume.
#[test]
fn an_agent_or_run_the_screen_doesnt_hold_opens_from_its_log() {
    let state = AppState::default();
    let view = AgentPanelView::new(&state);
    let panel = rows::panel(&state, now());
    let run = RunId::new("run-20261004-043029-d8bb").unwrap();
    let agent = CrewRow {
        target: CrewTarget::Agent {
            run: run.clone(),
            session: SessionKey::numbered(1),
        },
        repo: panel.repo.clone(),
        ..row("Wire the flag", CrewState::Done, "done")
    };
    assert!(matches!(
        view.open(&state, &panel, &agent),
        ViewOutcome::Request(ViewRequest::Agent { run: asked, session })
            if asked == run && session == SessionKey::numbered(1)
    ));
    // A stopped or waiting run can go on, so Enter resumes it. One that finished, was
    // abandoned (which reads as done), or failed can't, so Enter opens its workflow.
    for (done, opens_workflow) in [
        (CrewState::Done, true),
        (CrewState::Failed, true),
        (CrewState::Stopped, false),
        (CrewState::Ready, false),
    ] {
        let whole = CrewRow {
            target: CrewTarget::Run { run: run.clone() },
            repo: panel.repo.clone(),
            ..row("A run", done, "")
        };
        let outcome = view.open(&state, &panel, &whole);
        assert_eq!(
            matches!(
                outcome,
                ViewOutcome::Request(ViewRequest::Workflow { workflow: None, .. })
            ),
            opens_workflow,
            "{done:?}"
        );
    }
}

/// An agent its run no longer needed, such as the commit-message writer when the run merged
/// at once, is marked as the workflow view marks a skipped row: dim, never stopped.
#[test]
fn an_agent_the_run_no_longer_needed_reads_as_skipped() {
    let theme = Theme::default();
    let (glyph, role) = draw::glyph(CrewState::Skipped, false, &theme);
    assert_eq!((glyph, role), ("–", Role::Dim));
    assert_eq!(draw::state_word(CrewState::Skipped), "Skipped");
    assert_ne!(glyph, draw::glyph(CrewState::Stopped, false, &theme).0);
}
