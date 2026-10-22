use std::sync::Arc;
use std::time::Instant;

use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::app::{ConfigEntry, UiCommand, ViewData, ViewRequest};
use argonauts_core::events::run::RunMode;
use argonauts_core::input::SlashCommand;
use argonauts_core::names::ModelRef;
use argonauts_core::status_tab::{
    CliProvider, CliStatus, CliVersionCheck, Conversation, Folder, MainSession, ProviderBilling,
    SettingsFile, SettingsInEffect, StatusTabView, VersionStanding, WorkflowSetup,
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

/// A CLI signed in as `you@example.com`, its version standing as `state` says.
fn cli(id: &str, name: &str, version: Option<&str>, plan: &str, state: &str) -> CliStatus {
    let (standing, works) = match state {
        "checked" => (VersionStanding::Checked, false),
        "checked · works" => (VersionStanding::Checked, true),
        "newer than checked · works" => (VersionStanding::Newer, true),
        "new major version" => (VersionStanding::NewMajor, false),
        _ => (VersionStanding::Unknown, false),
    };
    CliStatus {
        id: id.to_owned(),
        name: name.to_owned(),
        command: id.to_owned(),
        version: version.map(str::to_owned),
        installed: version.is_some(),
        main: id == "claude-code",
        account: version.map(|_| "you@example.com".to_owned()),
        plan: (!plan.is_empty()).then(|| plan.to_owned()),
        check: CliVersionCheck {
            standing,
            works,
            ..CliVersionCheck::default()
        },
        ..CliStatus::default()
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
            id: "conversation-014".to_owned(),
            started: datetime!(2026-10-08 19:12 UTC),
        }),
        folder: Folder {
            shown: "~/workspace/rate-limiter-app".to_owned(),
            git: false,
            history_size: None,
        },
        main: Some(MainSession {
            worker: ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned())),
            effort: Some("xhigh".to_owned()),
            context_used: Some(212_000),
            context_window: Some(1_000_000),
        }),
        workflow: WorkflowSetup {
            name: "captain".to_owned(),
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
                "newer than checked · works",
            ),
            cli("codex", "Codex", Some("0.160.0"), "Plus", "checked"),
            cli("antigravity-cli", "agy", Some("1.3.0"), "Pro", "checked"),
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
            tools_off: false,
            mcp_servers: Some(vec!["github".to_owned(), "linear".to_owned()]),
            keys: vec![
                entry("crew.main", "\"claude-code\"", "global"),
                entry("effort", "\"xhigh\"", "project-local"),
            ],
        },
        runs: Vec::new(),
        run: None,
        health: Default::default(),
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

/// The whole tab as a screen `width` cells across draws it in `theme`.
fn screen(view: &mut OverviewView, state: &AppState, theme: &Theme, width: u16) -> Buffer {
    let ctx = ViewCtx {
        theme,
        state,
        now: Instant::now(),
    };
    let height = view.dock_height(Size::new(width, 80), &ctx).unwrap();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

fn rows(view: &mut OverviewView, state: &AppState, width: u16) -> Vec<String> {
    let buf = screen(view, state, &plain(), width);
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

/// The row of the old-runs picture that follows `label`.
fn old_runs_row(label: &str) -> &'static str {
    include_str!("../../tests/golden/old_runs.txt")
        .lines()
        .find_map(|row| row.trim_start().strip_prefix(label))
        .expect("the picture has the row")
        .trim()
}

/// A run another build wrote, as the old-runs picture lists it: its name, its day, the one
/// word for its state, and the key that archives it.
#[test]
fn a_run_from_an_older_build_shows_its_day_and_the_key_that_archives_it() {
    let state = state();
    let mut view = picture();
    let run = |title: &str, state: RunState, stopped_at: &str| RunSummary {
        run: RunId::new("run-20261004-072329-63c3").unwrap(),
        title: title.to_owned(),
        name: None,
        mode: RunMode::Crew,
        workflow: "council".to_owned(),
        state,
        stopped_at: stopped_at.to_owned(),
        updated: datetime!(2026-10-04 07:23:29 UTC),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: false,
        remind: false,
        shape_known: true,
    };
    view.runs = vec![run("Add due dates", RunState::Crashed, "stopped")];
    let mut status = tab(view.clone());
    // Nothing here to archive, so the key does nothing.
    assert!(matches!(
        key(&mut status, &state, KeyCode::Char('a')),
        ViewOutcome::Ignored
    ));
    let shown = rows(&mut status, &state, 118);
    assert!(
        row(&shown, "Add due dates").contains("○ Add due dates · Oct 4 · stopped"),
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|row| row.contains("archives")));

    view.runs = vec![run(
        "hi",
        RunState::FromAnotherBuild,
        "from an older Argonauts, can't be resumed",
    )];
    let mut status = tab(view);
    let shown = rows(&mut status, &state, 118);
    let listed = old_runs_row("Status →  runs:");
    assert!(
        shown.iter().any(|row| row.contains(listed)),
        "{listed}\n{shown:#?}"
    );
    assert!(matches!(
        key(&mut status, &state, KeyCode::Char('a')),
        ViewOutcome::SendToConversation(UiCommand::Slash {
            command: SlashCommand::Runs { archive: None },
        })
    ));
}

/// The folder's row once its hidden history takes room, with the command that trims it, as
/// the picture of a folder without git has it. The tab keeps its own label column, two
/// cells wider than the picture's, so the row is compared from its first word on.
#[test]
fn a_folder_without_git_shows_what_its_history_takes() {
    let wanted = include_str!("../../tests/golden/no_git_fixes.txt")
        .lines()
        .find_map(|row| row.strip_prefix("Folder"))
        .expect("the picture has the folder's row")
        .trim();
    let state = state();
    let mut view = picture();
    view.folder.shown = "~/code/todo".to_owned();
    view.folder.history_size = Some("18 MB".to_owned());
    let mut status = tab(view);
    let rows = rows(&mut status, &state, 118);
    let folder = row(&rows, " Folder ");
    let drawn = folder
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .strip_prefix("Folder")
        .expect("the row starts with its label")
        .trim_matches(|c: char| c == ' ' || c == '│');
    assert_eq!(drawn, wanted, "{folder}");
}

#[test]
fn status_draws_every_part_of_the_picture() {
    let state = state();
    let mut status = tab(picture());
    let rows = rows(&mut status, &state, 110);
    assert!(rows[1].contains("[Status]"), "{rows:#?}");
    for fact in [
        " Version         Argonauts Code 0.0.1",
        " Conversation    conversation-014 · started 7:12pm",
        " Folder          ~/workspace/rate-limiter-app · no git, workflows keep a hidden history",
        " Main session    Opus 5.5 on Claude Code · xhigh effort · 212k of 1M context",
        " Workflow        Captain · ultracode off · workflow size medium",
        " Mode            Supervised",
    ] {
        assert!(rows.iter().any(|row| row == fact), "{fact}: {rows:#?}");
    }
    assert!(row(&rows, "◆ your CLIs").ends_with("who you're signed in as, or what each can use │"));
    assert!(
        row(&rows, "● Claude Code").contains(
            "● Claude Code     2.1.295   you@example.com · Max · newer than checked · works"
        ),
        "{rows:#?}"
    );
    assert!(
        row(&rows, "○ Codex")
            .contains("○ Codex           0.160.0   you@example.com · Plus · checked")
    );
    assert!(row(&rows, "○ OpenCode").contains("—         not installed · /update can't add it"));
    assert!(
        row(&rows, "◆ settings in effect")
            .ends_with("enter opens All settings: each key and where it came from │")
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
        " tab · ←  →  tabs · ↓  into the cards · esc closes"
    );
}

#[test]
fn enter_on_the_settings_opens_all_settings_over_the_panel() {
    let state = state();
    let mut status = tab(picture());
    // Into the cards, past your CLIs and their health, to the settings.
    for _ in 0..3 {
        key(&mut status, &state, KeyCode::Down);
    }
    let shown = rows(&mut status, &state, 110);
    assert!(
        shown.last().unwrap().contains("enter all settings"),
        "{shown:#?}"
    );
    assert!(matches!(
        key(&mut status, &state, KeyCode::Enter),
        ViewOutcome::Request(ViewRequest::Settings { section: None })
    ));
    // Esc has one meaning: it closes the panel.
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
        stopped_at: "plan approval, waiting for you".to_owned(),
        updated: datetime!(2026-10-08 19:30 UTC),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: true,
        remind: false,
        shape_known: true,
    }];
    let mut status = tab(view);
    let shown = rows(&mut status, &state, 110);
    assert!(
        row(&shown, "export-reports-csv")
            .contains("● export-reports-csv · Oct 8 · plan approval, waiting for you"),
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|row| row.contains("None yet")));
    for _ in 0..4 {
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
    // Signed in, but nothing it says names the address.
    let unnamed = row(&rows, "signed in, address not shown");
    assert!(
        unnamed.contains("signed in, address not shown · "),
        "{unnamed}"
    );
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
        "Claude Code: 2.1.295, you@example.com, Max, newer than checked · works",
        "OpenCode: not installed",
    ] {
        assert!(lines.iter().any(|line| line == text), "{text}: {lines:#?}");
    }
}

/// End goes to the last card and Home to the first, as g and G do with vim keys on.
#[test]
fn home_and_end_go_to_the_first_and_last_card() {
    let state = state();
    let mut status = tab(picture());
    key(&mut status, &state, KeyCode::Down);
    let on_settings = |status: &mut OverviewView| {
        rows(status, &state, 110)
            .last()
            .unwrap()
            .contains("enter all settings")
    };
    key(&mut status, &state, KeyCode::End);
    assert!(!on_settings(&mut status));
    key(&mut status, &state, KeyCode::Up);
    assert!(
        on_settings(&mut status),
        "End went to the last card, the runs"
    );
    key(&mut status, &state, KeyCode::Home);
    assert!(!on_settings(&mut status));
    // Past your CLIs' health, the second card.
    key(&mut status, &state, KeyCode::Down);
    assert!(!on_settings(&mut status));
    key(&mut status, &state, KeyCode::Down);
    assert!(
        on_settings(&mut status),
        "Home went to the first card, your CLIs"
    );
}

/// The CLIs of the picture: three signed in to an account, and OpenCode at a new major version
/// with two providers.
fn pictured_clis() -> Vec<CliStatus> {
    vec![
        cli(
            "claude-code",
            "Claude Code",
            Some("2.1.296"),
            "Max",
            "checked · works",
        ),
        CliStatus {
            account: Some("another@example.org".to_owned()),
            ..cli("codex", "Codex", Some("0.160.0"), "Plus", "checked")
        },
        cli(
            "antigravity-cli",
            "agy",
            Some("1.3.3"),
            "Google AI Pro",
            "newer than checked · works",
        ),
        CliStatus {
            account: None,
            providers: Some(vec![
                CliProvider {
                    name: "OpenRouter".to_owned(),
                    billing: ProviderBilling::ApiKey,
                },
                CliProvider {
                    name: "Ollama".to_owned(),
                    billing: ProviderBilling::Local,
                },
            ]),
            default_model: Some("GLM 5 on OpenRouter".to_owned()),
            check: CliVersionCheck {
                standing: VersionStanding::NewMajor,
                checked_with: Some("1.18.34".to_owned()),
                ..CliVersionCheck::default()
            },
            ..cli("opencode", "OpenCode", Some("2.0.26"), "", "")
        },
    ]
}

/// The CLIs card as drawn in a view 120 cells wide, the picture's width with its margins.
fn clis_card(clis: Vec<CliStatus>) -> Buffer {
    use crate::kit::{Frame, Kit};

    let mut view = picture();
    view.clis = clis;
    let theme = crate::overview::golden::theme();
    let kit = Kit::new(&theme);
    let names = sample::names();
    let cards = super::StatusTabCards {
        view: &view,
        names: &names,
        clock: crate::format::WallClock::pinned(datetime!(2026-10-08 20:00 UTC)),
        mode: None,
    };
    let drawn = cards.clis(&kit, Frame::view(120));
    let mut buf = Buffer::empty(Rect::new(0, 0, 120, drawn.lines.len() as u16));
    for (y, line) in drawn.lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, 120);
    }
    buf
}

fn buffer_rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// The CLIs card at the picture's width, in its colours: who each CLI is signed in as, what
/// OpenCode can use and how each provider bills, and how every version stands.
#[test]
fn the_clis_card_matches_its_picture() {
    use crate::overview::golden::{Picture, Region, assert_matches, theme};

    let shot = Picture::load("status_clis");
    let buf = clis_card(pictured_clis());
    // The picture shows today's card first; the card as it is now is its second frame.
    let after = Region {
        rows: 7..15,
        columns: 0..shot.width(),
        at: (1, 0),
    };
    assert_matches(&shot, &buf, &after, &theme());
}

/// The whole tab on the screen its pictures were drawn for, their 118 cells with the panel's
/// column each side: the CLIs card and the health card under it, each cell for cell where its
/// picture has it.
#[test]
fn the_tab_draws_both_cards_as_their_pictures_have_them() {
    use crate::overview::golden::{Picture, Region, assert_matches, theme};
    use crate::overview::health::tests as health;

    let mut view = picture();
    view.clis = pictured_clis();
    view.health = health::picture_rows();
    let theme = theme();
    let buf = screen(&mut tab(view), &state(), &theme, 120);
    let rows = buffer_rows(&buf);
    let top_of = |title: &str| {
        let titled = rows.iter().position(|row| row.contains(title));
        titled.unwrap_or_else(|| panic!("no {title}: {rows:#?}")) as u16 - 1
    };

    // The picture shows today's card first; the card as it is now is its second frame.
    let clis = Picture::load("status_clis");
    let after = Region {
        rows: 7..15,
        columns: 0..clis.width(),
        at: (1, top_of("◆ your CLIs")),
    };
    assert_matches(&clis, &buf, &after, &theme);

    let card = Picture::load("health_card");
    let whole = Region::whole(&card, (1, top_of("CLI health")));
    let unlike = health::differences_past_model_names(&card, &buf, &whole, &theme);
    assert!(unlike.is_empty(), "{}\n{rows:#?}", unlike.join("\n"));
}

/// A screen two cells narrower than the pictures' still says everything: a long folder's
/// history and the command that trims it take a row of their own, a CLI that needs an update
/// keeps the command that updates it, and the health card's last column is whole.
#[test]
fn a_screen_narrower_than_the_pictures_keeps_what_matters() {
    use crate::overview::health::tests as health;

    let mut view = picture();
    view.folder.shown = "/private/tmp/argo-lEWveT/repo".to_owned();
    view.folder.history_size = Some("1 MB".to_owned());
    view.clis = pictured_clis();
    view.clis[1] = CliStatus {
        account: None,
        plan: None,
        signed_out: true,
        version: Some("0.150.0".to_owned()),
        check: CliVersionCheck {
            standing: VersionStanding::Older,
            oldest_supported: Some("0.158.0".to_owned()),
            update_command: Some("npm install -g @openai/codex".to_owned()),
            ..CliVersionCheck::default()
        },
        ..view.clis[1].clone()
    };
    view.health = health::picture_rows();
    let state = state();
    let rows = rows(&mut tab(view), &state, 118);
    let at = |text: &str| {
        let found = rows.iter().position(|row| row.contains(text));
        found.unwrap_or_else(|| panic!("no {text}: {rows:#?}"))
    };
    let setup = &rows[..at("◆ settings in effect")];
    assert!(!setup.iter().any(|row| row.contains('…')), "{setup:#?}");

    let folder = at(" Folder ");
    assert_eq!(
        rows[folder],
        " Folder          /private/tmp/argo-lEWveT/repo · no git, workflows keep a hidden history"
    );
    assert_eq!(
        rows[folder + 1],
        "                 history.git 1 MB · /git clean"
    );

    let codex = at("○ Codex");
    assert!(
        rows[codex].contains("○ Codex           0.150.0   not signed in · /login signs in  "),
        "{}",
        rows[codex]
    );
    assert!(
        rows[codex + 1].contains(
            "│                             older than supported · 0.158.0 needed · npm install -g @openai/codex  "
        ),
        "{}",
        rows[codex + 1]
    );
    for said in ["after 2 quiet minutes │", "out if silent an hour │"] {
        assert!(rows[at(said)].ends_with(said), "{said}");
    }
}

/// On a narrow screen a fact or a CLI's row runs on by whole parts under its own words, so
/// nothing of them is cut: the folder's three parts take a row each, and OpenCode's version,
/// default model and what it was checked with each start a row.
#[test]
fn a_narrow_screen_wraps_facts_and_cli_rows_by_whole_parts() {
    let mut view = picture();
    view.folder.history_size = Some("18 MB".to_owned());
    view.clis = pictured_clis();
    let state = state();
    let rows = rows(&mut tab(view), &state, 80);
    let at = |text: &str| {
        let found = rows.iter().position(|row| row.contains(text));
        found.unwrap_or_else(|| panic!("no {text}: {rows:#?}"))
    };
    let above_health = &rows[..at("CLI health")];
    assert!(
        !above_health.iter().any(|row| row.contains('…')),
        "{above_health:#?}"
    );
    let folder = at(" Folder ");
    assert_eq!(
        rows[folder..folder + 3],
        [
            " Folder          ~/workspace/rate-limiter-app",
            "                 no git, workflows keep a hidden history",
            "                 history.git 18 MB · /git clean",
        ]
    );
    let inside = |row: &str| row.trim_matches(['│', ' ']).to_owned();
    let opencode = at("○ OpenCode");
    let drawn: Vec<String> = rows[opencode..opencode + 4]
        .iter()
        .map(|row| inside(row))
        .collect();
    assert_eq!(
        drawn,
        [
            "○ OpenCode        2.0.26    OpenRouter (API key, billed) · Ollama (local)",
            "new major version",
            "default GLM 5 on OpenRouter",
            "checked with 1.18.34 · compared at start-up",
        ]
    );
    // Each row after the first starts under the first's words.
    let words_at = |row: &str, word: &str| row.find(word).map(|at| row[..at].chars().count());
    let first = words_at(&rows[opencode], "OpenRouter");
    assert_eq!(words_at(&rows[opencode + 1], "new major"), first);
    assert_eq!(words_at(&rows[opencode + 3], "checked with"), first);
}

/// The Status tab says how each CLI stands and then how each of its models does: the CLIs
/// card with who is signed in and what its version is, and under it the health card with a
/// row a model, the one that fills in for another saying so.
#[test]
fn the_clis_card_and_each_models_health_show_together() {
    use argonauts_core::status_tab::{CliHealth, Covers, HealthRows, ModelStanding, Standing};

    let model = |name: &str, state: Standing, why: &str| ModelStanding {
        name: name.to_owned(),
        state,
        why: why.to_owned(),
        covers: None,
        next_check: None,
    };
    let fills_in = ModelStanding {
        covers: Some(Covers {
            job: "review:due-tests".to_owned(),
            cli: "claude-code".to_owned(),
            model: "Opus 5.5".to_owned(),
        }),
        ..model("Sonnet 5.5", Standing::Healthy, "")
    };
    let mut view = picture();
    view.clis = pictured_clis();
    view.health = HealthRows {
        clis: vec![CliHealth {
            id: "claude-code".to_owned(),
            name: "Claude Code".to_owned(),
            installed: true,
            main: true,
            models: vec![
                model(
                    "Opus 5.5",
                    Standing::Overloaded,
                    "2 overload errors in a row",
                ),
                fills_in,
            ],
        }],
        quiet_secs: Some(120),
        cooldown_secs: 300,
    };
    let state = state();
    let rows = rows(&mut tab(view), &state, 120);
    let at = |text: &str| {
        rows.iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("no {text}: {rows:#?}"))
    };
    let (clis, health) = (at("◆ your CLIs"), at("CLI health"));
    assert!(clis < health, "{rows:#?}");
    let card = &rows[clis..health];
    for said in [
        "2.1.296   you@example.com · Max · checked · works",
        "0.160.0   another@example.org · Plus · checked",
        "OpenRouter (API key, billed) · Ollama (local) · new major version",
    ] {
        assert!(
            card.iter().any(|row| row.contains(said)),
            "{said}: {card:#?}"
        );
    }
    let models = &rows[health..];
    let of = |name: &str| {
        models
            .iter()
            .find(|row| row.contains(name))
            .unwrap_or_else(|| panic!("no {name}: {models:#?}"))
    };
    let opus = of("Opus 5.5 ");
    assert!(
        opus.contains("Claude Code") && opus.contains("overloaded"),
        "{opus}"
    );
    assert!(opus.contains("2 overload errors in a row"), "{opus}");
    let sonnet = of("Sonnet 5.5");
    assert!(
        sonnet.contains("healthy") && sonnet.contains("review:due-tests for Opus 5.5"),
        "{sonnet}"
    );
}

/// OpenCode with no provider connected says how to connect one, where the others say who is
/// signed in.
#[test]
fn opencode_with_no_provider_says_how_to_connect_one() {
    let mut clis = pictured_clis();
    clis[3].providers = Some(Vec::new());
    clis[3].default_model = None;
    let rows = buffer_rows(&clis_card(clis));
    let opencode = row(&rows, "○ OpenCode");
    assert!(
        opencode.contains("OpenCode        2.0.26    no provider set up · opencode auth login"),
        "{opencode}"
    );
    let picture = include_str!("../../tests/golden/overview/status_clis.txt");
    assert!(
        picture.contains("no provider set up · opencode auth login"),
        "the picture's words"
    );
}

/// OpenCode has no one account, so its row never speaks of an address: while it is asked it
/// says so, and an answer that couldn't be read names the command of its own that lists them.
#[test]
fn opencode_never_reads_as_an_account_whose_address_is_not_shown() {
    let mut clis = pictured_clis();
    clis[3].lists_providers = true;
    clis[3].providers = None;
    clis[3].default_model = None;
    let rows = buffer_rows(&clis_card(clis.clone()));
    let asking = row(&rows, "○ OpenCode");
    assert!(asking.contains("2.0.26    asking OpenCode…"), "{asking}");

    clis[3].providers_unread = true;
    let rows = buffer_rows(&clis_card(clis));
    let unread = row(&rows, "○ OpenCode");
    assert!(
        unread.contains("2.0.26    couldn't list its providers · opencode auth list shows them"),
        "{unread}"
    );
    for row in [asking, unread] {
        assert!(!row.contains("signed in"), "{row}");
        assert!(!row.contains("address"), "{row}");
    }
}

/// A version that needs an update, or has a newer one out, says so on its row with the command
/// that updates it; one that is up to date adds nothing.
#[test]
fn an_update_is_said_on_the_row_and_nothing_else_is() {
    let mut clis = pictured_clis();
    clis[0].check.newest = Some("2.1.297".to_owned());
    clis[0].check.newer_is_out = true;
    clis[0].check.update_command = Some("claude update".to_owned());
    clis[1].check = CliVersionCheck {
        standing: VersionStanding::Older,
        oldest_supported: Some("0.160.0".to_owned()),
        update_command: Some("brew upgrade codex".to_owned()),
        ..CliVersionCheck::default()
    };
    clis[1].version = Some("0.158.0".to_owned());
    // A shorter address, so the whole row fits the card.
    clis[1].account = Some("you@example.com".to_owned());
    clis[2].check.newest = Some("1.3.3".to_owned());
    let rows = buffer_rows(&clis_card(clis));
    assert!(
        row(&rows, "● Claude Code")
            .contains("· Max · checked · works · 2.1.297 is out · claude update"),
        "{rows:#?}"
    );
    assert!(
        row(&rows, "○ Codex")
            .contains("· Plus · older than supported · 0.160.0 needed · brew upgrade codex"),
        "{rows:#?}"
    );
    assert!(
        row(&rows, "○ agy")
            .trim_end_matches(['│', ' '])
            .ends_with("· Google AI Pro · newer than checked · works"),
        "{rows:#?}"
    );
}

/// A folder agy hasn't trusted is said plainly, with what to do about it.
#[test]
fn a_folder_agy_has_not_trusted_says_to_start_it_here_once() {
    let mut clis = pictured_clis();
    clis[2].account = None;
    clis[2].plan = None;
    clis[2].untrusted_folder = true;
    clis[2].command = "agy".to_owned();
    let rows = buffer_rows(&clis_card(clis));
    assert!(
        row(&rows, "○ agy")
            .contains("1.3.3     agy hasn't trusted this folder; start agy here once · newer"),
        "{rows:#?}"
    );
}

/// Tools you turned off say so, and by which key.
#[test]
fn tools_turned_off_say_by_which_key() {
    let mut view = picture();
    view.settings.tools.clear();
    view.settings.tools_off = true;
    let state = state();
    let mut tab = tab(view);
    let rows = rows(&mut tab, &state, 140);
    assert!(
        row(&rows, "tools for the main session").contains("off · main_session.tools = false"),
        "{rows:#?}"
    );
}
