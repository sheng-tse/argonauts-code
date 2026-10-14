use std::sync::Arc;
use std::time::Instant;

use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::app::{ConfigEntry, ViewData, ViewRequest};
use argonauts_core::events::run::RunMode;
use argonauts_core::names::WorkerRef;
use argonauts_core::status_tab::{
    CliStatus, Conversation, Folder, MainSession, SettingsFile, SettingsInEffect, StatusTabView,
    WorkflowSetup,
};
use argonauts_core::{RunId, Spend};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use time::macros::datetime;

use super::NO_RUNS;
use crate::overview::{OverviewView, Tab, UsageWatch, sample};
use crate::state::AppState;
use crate::theme::{ColorLevel, Glyphs, Theme};
use crate::views::{View, ViewCtx, ViewOutcome};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn cli(id: &str, name: &str, version: Option<&str>, plan: &str, note: &str) -> CliStatus {
    CliStatus {
        id: id.to_owned(),
        name: name.to_owned(),
        version: version.map(str::to_owned),
        installed: version.is_some(),
        main: id == "claude-code",
        account: version.map(|_| "you@example.com".to_owned()),
        signed_out: false,
        plan: (!plan.is_empty()).then(|| plan.to_owned()),
        note: note.to_owned(),
    }
}

fn entry(key: &str, value: &str, layer: &str) -> ConfigEntry {
    ConfigEntry {
        key: key.to_owned(),
        value: value.to_owned(),
        layer: layer.to_owned(),
        safety: false,
    }
}

/// The tab for a folder without git and no run yet.
fn picture() -> StatusTabView {
    StatusTabView {
        version: "0.0.1".to_owned(),
        conversation: Some(Conversation {
            id: "chat-014".to_owned(),
            started: datetime!(2026-10-08 19:12 UTC),
        }),
        folder: Folder {
            shown: "~/workspace/rate-limiter-app".to_owned(),
            git: false,
        },
        main: Some(MainSession {
            worker: WorkerRef::new("claude-code", Some("claude-opus-5-5".to_owned())),
            effort: Some("xhigh".to_owned()),
            context_used: Some(212_000),
            context_window: Some(1_000_000),
        }),
        workflow: WorkflowSetup {
            name: "solo".to_owned(),
            ultracode: false,
            size: "medium".to_owned(),
        },
        mode: "supervised".to_owned(),
        clis: vec![
            cli(
                "claude-code",
                "Claude Code",
                Some("2.1.295"),
                "Max",
                "newer than checked, works",
            ),
            cli("codex", "Codex", Some("0.160.0"), "Plus", "checked"),
            cli(
                "antigravity-cli",
                "Antigravity CLI",
                Some("1.3.0"),
                "Pro",
                "checked",
            ),
            cli("opencode", "OpenCode", None, "", "not installed"),
        ],
        settings: SettingsInEffect {
            files: vec![
                SettingsFile {
                    path: "~/.argonauts/config.toml".to_owned(),
                    layer: "global".to_owned(),
                    keys: 1,
                },
                SettingsFile {
                    path: ".argonauts/config.local.toml".to_owned(),
                    layer: "project-local".to_owned(),
                    keys: 1,
                },
            ],
            mods_built_in: 6,
            mods_yours: vec!["checks".to_owned()],
            tools: ["start a workflow", "set a goal", "setup", "records"]
                .map(str::to_owned)
                .to_vec(),
            mcp_servers: Some(vec!["github".to_owned(), "linear".to_owned()]),
            keys: vec![
                entry("main_agent", "\"claude-code\"", "global"),
                entry("effort", "\"xhigh\"", "project-local"),
            ],
        },
        runs: Vec::new(),
        run: None,
    }
}

fn state() -> AppState {
    let mut state = AppState::default();
    state.usage = Some(Arc::new(sample::usage()));
    state.names = sample::names();
    state
}

fn tab(view: StatusTabView) -> OverviewView {
    OverviewView::new(Tab::Status, UsageWatch::default())
        .at(datetime!(2026-10-08 20:00 UTC))
        .with_status_tab(view)
}

fn rows(view: &mut OverviewView, state: &AppState, width: u16) -> Vec<String> {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let height = view.dock_height(Size::new(width, 80), &ctx).unwrap();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
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

fn row<'a>(rows: &'a [String], text: &str) -> &'a str {
    rows.iter()
        .find(|row| row.contains(text))
        .unwrap_or_else(|| panic!("{text}: {rows:#?}"))
}

#[test]
fn status_draws_every_part_of_the_picture() {
    let state = state();
    let mut status = tab(picture());
    let rows = rows(&mut status, &state, 110);
    assert!(rows[1].contains("[Status]"), "{rows:#?}");
    for fact in [
        " Version         Argonauts Code 0.0.1",
        " Conversation    chat-014 · started 7:12pm",
        " Folder          ~/workspace/rate-limiter-app · no git, workflows keep a hidden history",
        " Main session    Opus 5.5 on Claude Code · xhigh effort · 212k of 1M context",
        " Workflow        solo · ultracode off · workflow size medium",
        " Mode            supervised",
    ] {
        assert!(rows.iter().any(|row| row == fact), "{fact}: {rows:#?}");
    }
    assert!(row(&rows, "◆ your CLIs").ends_with("who you're logged in as on each │"));
    assert!(
        row(&rows, "● Claude Code").contains(
            "● Claude Code     2.1.295     you@example.com · Max · newer than checked, works"
        ),
        "{rows:#?}"
    );
    assert!(
        row(&rows, "○ Codex")
            .contains("○ Codex           0.160.0     you@example.com · Plus · checked")
    );
    assert!(row(&rows, "○ OpenCode").contains("—           not installed · /update can't add it"));
    assert!(
        row(&rows, "◆ settings in effect")
            .ends_with("enter shows each key and where it came from │")
    );
    assert!(row(&rows, "~/.argonauts/config.toml").contains("1 key · your settings"));
    assert!(row(&rows, ".argonauts/config.local.toml").contains("1 key · this folder, only you"));
    assert!(row(&rows, "│ mods").contains("6 built in · 1 of yours (checks)"));
    assert!(
        row(&rows, "tools for the main session")
            .contains("start a workflow · set a goal · setup · records")
    );
    assert!(
        row(&rows, "MCP servers").contains("2 · from your Claude Code settings: github, linear")
    );
    assert!(row(&rows, "◆ runs").ends_with("in this folder │"));
    assert!(row(&rows, NO_RUNS).contains(NO_RUNS));
    let at = |text: &str| rows.iter().position(|row| row.contains(text)).unwrap();
    assert!(at(" Version ") < at("◆ your CLIs"));
    assert!(at("◆ your CLIs") < at("◆ settings in effect"));
    assert!(at("◆ settings in effect") < at("◆ runs"));
    assert_eq!(
        rows.last().unwrap(),
        " tab · ←→ tabs · ↓ into the cards · enter open · esc back"
    );
}

#[test]
fn enter_on_the_settings_lists_each_key_with_where_it_came_from_and_esc_closes_it() {
    let state = state();
    let mut status = tab(picture());
    key(&mut status, &state, KeyCode::Down);
    key(&mut status, &state, KeyCode::Down);
    let shown = rows(&mut status, &state, 110);
    assert!(
        shown.last().unwrap().contains("enter each key"),
        "{shown:#?}"
    );
    key(&mut status, &state, KeyCode::Enter);
    let opened = rows(&mut status, &state, 110);
    assert!(
        row(&opened, "main_agent").contains("claude-code · ~/.argonauts/config.toml"),
        "{opened:#?}"
    );
    assert!(row(&opened, "│ effort").contains("xhigh · .argonauts/config.local.toml"));
    assert!(matches!(
        key(&mut status, &state, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    let closed = rows(&mut status, &state, 110);
    assert!(!closed.iter().any(|row| row.contains("main_agent")));
    assert!(matches!(
        key(&mut status, &state, KeyCode::Esc),
        ViewOutcome::Close
    ));
}

#[test]
fn the_runs_card_lists_this_folders_runs_and_enter_opens_the_newest() {
    let state = state();
    let mut view = picture();
    view.runs = vec![RunSummary {
        run: RunId::new("run-03").unwrap(),
        title: "Export reports as CSV".to_owned(),
        name: Some("export-reports-csv".to_owned()),
        mode: RunMode::Crew,
        workflow: "council".to_owned(),
        state: RunState::Running,
        stopped_at: "plan checkpoint, waiting for you".to_owned(),
        updated: datetime!(2026-10-08 19:30 UTC),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: true,
        remind: false,
    }];
    let mut status = tab(view);
    let shown = rows(&mut status, &state, 110);
    assert!(
        row(&shown, "export-reports-csv")
            .contains("● export-reports-csv · run-03 · plan checkpoint, waiting for you"),
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|row| row.contains("None yet")));
    for _ in 0..3 {
        key(&mut status, &state, KeyCode::Down);
    }
    assert!(matches!(
        key(&mut status, &state, KeyCode::Enter),
        ViewOutcome::Request(ViewRequest::Workflow { .. })
    ));
}

#[test]
fn an_answer_with_nothing_yet_still_draws_the_whole_tab() {
    let state = state();
    let mut status = OverviewView::new(Tab::Status, UsageWatch::default());
    assert!(status.accept(&ViewData::StatusTab {
        view: Box::new(StatusTabView {
            version: "0.0.1".to_owned(),
            ..StatusTabView::default()
        }),
    }));
    let shown = rows(&mut status, &state, 90);
    assert!(row(&shown, "◆ your CLIs").contains("◆ your CLIs"));
    assert!(row(&shown, "Doctor hasn't checked").contains("/doctor"));
    assert!(row(&shown, "settings files").contains("none · every key is built in"));
    assert!(row(&shown, "MCP servers").contains("listed once the main session starts"));
    assert!(row(&shown, "None yet.").contains("None yet."));
}

/// A CLI that isn't signed in says so, in the warning colour, and one whose account isn't
/// known never claims to be signed in.
#[test]
fn a_cli_is_signed_in_signed_out_or_not_known() {
    let mut view = picture();
    view.clis[1].account = None;
    view.clis[1].signed_out = true;
    view.clis[2].account = None;
    let state = state();
    let mut tab = tab(view);
    let rows = rows(&mut tab, &state, 140);
    let out = row(&rows, "not signed in · /login signs in");
    assert!(!out.contains("signed in as"), "{out}");
    let unknown = row(&rows, "account not known");
    assert!(!unknown.contains("signed in"), "{unknown}");
    assert!(
        !rows.iter().any(|row| row.contains("signed in as it says")),
        "{rows:#?}"
    );
}

#[test]
fn a_screen_reader_hears_the_same_facts() {
    let lines = super::spoken(&picture(), &sample::names());
    for text in [
        "version: Argonauts Code 0.0.1",
        "folder: ~/workspace/rate-limiter-app, no git",
        "Claude Code: 2.1.295, you@example.com, Max, newer than checked, works",
        "OpenCode: not installed",
    ] {
        assert!(lines.iter().any(|line| line == text), "{text}: {lines:#?}");
    }
}
