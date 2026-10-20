use std::time::Instant;

use argonauts_core::app::{AppEvent, HeaderAndBottomLine, SettingRow, UiCommand};
use argonauts_core::input::SlashCommand;
use argonauts_core::log::NoteLevel;
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::approvals::{AUTOPILOT_BACK, ApprovalMode};
use argonauts_core::settings::ui::NarratorMode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};

use super::ConfigTab;
use crate::cell::{HistoryCell, ResultLine, Tone, UserMessage};
use crate::overview::golden::{Picture, Region, assert_matches, theme};
use crate::overview::{OverviewView, Tab, UsageWatch, sample};
use crate::render::{Clock, RenderCtx};
use crate::state::{AppState, SettingOutcome};
use crate::theme::Theme;
use crate::views::{View, ViewCtx, ViewOutcome};

/// Every key the picture's settings hold, at the values it shows, in a file of 713 keys.
fn settings() -> Vec<SettingRow> {
    let mut rows: Vec<SettingRow> = [
        ("compaction", "\"inherit\"", "built-in"),
        ("fallback.mode", "\"auto\"", "built-in"),
        ("ui.tips", "true", "built-in"),
        ("ui.motion", "\"normal\"", "built-in"),
        ("ui.recaps", "true", "built-in"),
        ("captain.rewind", "true", "built-in"),
        ("ui.verbosity", "\"normal\"", "built-in"),
        ("ui.turn_duration", "true", "built-in"),
        ("ui.format.clock", "\"auto\"", "built-in"),
        ("approvals.start_mode", "\"last\"", "built-in"),
        ("ui.theme", "\"dark\"", "global"),
        ("ui.keys", "\"vim\"", "global"),
        ("crew.main", "\"claude-code/claude-opus-5-5\"", "global"),
        ("roles.main.effort", "\"high\"", "project-local"),
        ("workflow", "\"captain\"", "global"),
        ("crew.reviewer", "[\"codex\"]", "global"),
        ("crew.judge", "[\"agy\"]", "global"),
        ("fallback.same_cli", "false", "built-in"),
        ("limits.spend_per_run", "3.0", "global"),
        ("crew.worker_workflows", "true", "built-in"),
        ("workflow_size", "\"medium\"", "built-in"),
        ("notifications.os", "true", "built-in"),
        ("fallback.ask_timeout", "\"5m\"", "built-in"),
        ("crew.orchestrator", "\"claude-code\"", "built-in"),
        ("crew.worker", "[\"claude-code\"]", "built-in"),
        ("crew.merger", "[\"claude-code\"]", "built-in"),
        ("approvals.answers", "", "built-in"),
        ("limits.spend_per_day", "", "built-in"),
        ("sandbox.mode", "\"workspace-write\"", "built-in"),
        ("sandbox.network", "true", "built-in"),
        ("sandbox.agents_commit", "true", "built-in"),
        ("delivery.commits", "\"squash\"", "built-in"),
        ("ui.narrator", "\"on\"", "built-in"),
        ("ui.verbs", "\"voyage\"", "built-in"),
        ("ui.glyphs", "\"unicode\"", "built-in"),
        ("ui.agents_in_conversation", "\"finished\"", "built-in"),
        ("ui.newer_cli_notice", "true", "built-in"),
        ("awake.while_working", "true", "built-in"),
        ("telemetry.enabled", "false", "built-in"),
    ]
    .into_iter()
    .map(|(key, value, layer)| setting(key, value, layer))
    .collect();
    let mut more = 0;
    while rows.len() < 713 {
        more += 1;
        rows.push(setting(&format!("examples.key_{more}"), "1", "built-in"));
    }
    rows
}

fn setting(key: &str, value: &str, layer: &str) -> SettingRow {
    SettingRow {
        key: key.to_owned(),
        value: value.to_owned(),
        layer: layer.to_owned(),
        workflow_from: None,
        default_scope: SettingScope::Global,
        safety: key.starts_with("sandbox."),
        allowed: Vec::new(),
        description: String::new(),
    }
}

fn state() -> AppState {
    let mut state = AppState::default();
    state.names = sample::names();
    state
}

/// The tab as `/config` opens it, with every key come and autopilot on.
fn opened() -> OverviewView {
    let mut tab = ConfigTab::opened_by("config");
    tab.set_rows(settings());
    tab.set_mode_now(Some("autopilot".to_owned()));
    OverviewView::new(Tab::Config, UsageWatch::default()).with_config(tab)
}

fn key(view: &mut OverviewView, state: &AppState, code: KeyCode) -> ViewOutcome {
    let theme = theme();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::from(code), &ctx)
}

fn screen(view: &mut OverviewView, state: &AppState, theme: &Theme, width: u16) -> Buffer {
    let ctx = ViewCtx {
        theme,
        state,
        now: Instant::now(),
    };
    let height = view.dock_height(Size::new(width, 60), &ctx).unwrap();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

fn rows(view: &mut OverviewView, state: &AppState, width: u16) -> Vec<String> {
    let buf = screen(view, state, &theme(), width);
    crate::views::dock::lines(&buf)
        .iter()
        .map(|line| line.to_string().trim_end().to_owned())
        .collect()
}

/// The picture: the pointer on Default approval mode, nine rows into the list.
#[test]
fn the_config_tab_draws_as_the_picture() {
    let state = state();
    let mut view = opened();
    key(&mut view, &state, KeyCode::Down);
    for _ in 0..9 {
        key(&mut view, &state, KeyCode::Down);
    }
    let picture = Picture::load("config_tab");
    let width = u16::try_from(picture.width()).unwrap();
    let buf = screen(&mut view, &state, &theme(), width);
    assert_eq!(
        buf.area.height as usize,
        picture.rows.len(),
        "the panel's height"
    );
    assert_matches(&picture, &buf, &Region::whole(&picture, (0, 0)), &theme());
}

/// The header and bottom line of a main session on `agent` at `effort`, as the app sends it.
fn main_session(agent: &str, effort: &str) -> HeaderAndBottomLine {
    let line = argonauts_core::app::LineFacts {
        main_effort: Some(effort.to_owned()),
        ..Default::default()
    };
    HeaderAndBottomLine {
        title: String::new(),
        repo: "~/workspace/rate-limiter-app/captain".to_owned(),
        workflow: "captain".to_owned(),
        ultracode: false,
        ultra: None,
        run: None,
        main_agent: agent.parse().unwrap(),
        plan_chips: Vec::new(),
        line: Box::new(line),
        spent: None,
        limit: None,
        narrator: NarratorMode::Off,
        approval_mode: ApprovalMode::Autopilot,
        awake: None,
    }
}

/// The setup's recommended crew names only the main agent's CLI and leaves its effort to the
/// model. Model and Effort still say what runs, as the header does: the model and its CLI, as
/// the picture's Model row reads, and the level that applies, said as the default.
#[test]
fn model_and_effort_say_what_runs_when_the_settings_leave_it_to_the_cli() {
    let picture = Picture::load("config_tab");
    let width = u16::try_from(picture.width()).unwrap();
    let mut state = state();
    state.status = Some(main_session("claude-code/claude-opus-5-5", "xhigh"));
    let mut rows = settings();
    for row in &mut rows {
        match row.key.as_str() {
            "crew.main" => row.value = "\"claude-code\"".to_owned(),
            "roles.main.effort" => row.value = String::new(),
            _ => {}
        }
    }
    let mut tab = ConfigTab::opened_by("config");
    tab.set_rows(rows);
    tab.set_mode_now(Some("autopilot".to_owned()));
    let mut view = OverviewView::new(Tab::Config, UsageWatch::default()).with_config(tab);
    for _ in 0..10 {
        key(&mut view, &state, KeyCode::Down);
    }
    let buf = screen(&mut view, &state, &theme(), width);
    let model = Region {
        rows: 20..21,
        columns: 0..picture.width(),
        at: (0, 20),
    };
    assert_matches(&picture, &buf, &model, &theme());
    let lines = crate::views::dock::lines(&buf);
    let effort = lines[21].to_string();
    assert_eq!(
        effort.trim_end(),
        format!("    Effort{}xhigh (default) ›", " ".repeat(37))
    );
}

/// A link row whose value is too long for a narrow panel keeps its `›`: the value is cut, not
/// the mark that says Enter opens it.
#[test]
fn a_link_cut_short_keeps_its_mark() {
    let mut state = state();
    state
        .names
        .clis
        .insert("antigravity-cli".to_owned(), "Antigravity CLI".to_owned());
    let mut tab = ConfigTab::opened_by("config");
    let mut keys = settings();
    for key in &mut keys {
        if key.key == "crew.reviewer" {
            key.value = r#"["claude-code", "codex", "antigravity-cli"]"#.to_owned();
        }
    }
    tab.set_rows(keys);
    let mut view = OverviewView::new(Tab::Config, UsageWatch::default()).with_config(tab);
    let shown = rows(&mut view, &state, 80);
    let reviewer = shown
        .iter()
        .find(|row| row.trim_start().starts_with("Reviewer"))
        .unwrap_or_else(|| panic!("{shown:#?}"));
    assert!(reviewer.ends_with("… ›"), "{reviewer:?}");
    assert!(reviewer.contains("Claude Code, Codex, "), "{reviewer:?}");
    assert!(crate::text::str_width(reviewer) <= 80, "{reviewer:?}");
}

#[test]
fn it_opens_on_the_search_and_typing_filters_the_rows() {
    let state = state();
    let mut view = opened();
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown.iter().any(
            |row| row.contains("Type to filter · Enter/↓ to select · ↑ to tabs · Esc to clear")
        ),
        "{shown:#?}"
    );
    for ch in "edit".chars() {
        key(&mut view, &state, KeyCode::Char(ch));
    }
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown.iter().any(|row| row.contains("Editor mode")),
        "{shown:#?}"
    );
    assert!(
        !shown.iter().any(|row| row.contains("Auto-compact")),
        "{shown:#?}"
    );
    for ch in "zzz".chars() {
        key(&mut view, &state, KeyCode::Char(ch));
    }
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("No settings match \"editzzz\"")),
        "{shown:#?}"
    );
    // Esc clears the search first, then closes.
    key(&mut view, &state, KeyCode::Esc);
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown.iter().any(|row| row.contains("Auto-compact")),
        "{shown:#?}"
    );
}

/// A toggle flips at once and saves to your global file without a word in the conversation;
/// Esc then says what changed under the command.
#[test]
fn a_toggle_saves_at_once_and_esc_says_what_changed() {
    let state = state();
    let mut view = opened();
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Down);
    // Show tips, true.
    let saved = key(&mut view, &state, KeyCode::Enter);
    let ViewOutcome::SaveQuietly(UiCommand::SetSetting {
        key: saved_key,
        value,
        scope,
    }) = saved
    else {
        panic!("{saved:?}");
    };
    assert_eq!(
        (saved_key.as_str(), value.as_str(), scope),
        ("ui.tips", "false", SettingScope::Global)
    );
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("Show tips") && row.ends_with("false")),
        "{shown:#?}"
    );
    let closed = key(&mut view, &state, KeyCode::Esc);
    let ViewOutcome::CloseSaying { command, line } = closed else {
        panic!("{closed:?}");
    };
    assert_eq!(command, "/config");
    assert_eq!(line, "Set Show tips to false");
}

/// A choice steps with ← and →, each step saved; Last used names the mode on its row only.
#[test]
fn a_choice_steps_both_ways_and_esc_lists_each_change_once() {
    let state = state();
    let mut view = opened();
    key(&mut view, &state, KeyCode::Down);
    for _ in 0..9 {
        key(&mut view, &state, KeyCode::Down);
    }
    key(&mut view, &state, KeyCode::Right);
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("‹ as your approvals say ›")),
        "{shown:#?}"
    );
    key(&mut view, &state, KeyCode::Left);
    // Editor mode, three rows down: vim to normal and back.
    for _ in 0..2 {
        key(&mut view, &state, KeyCode::Down);
    }
    key(&mut view, &state, KeyCode::Char(' '));
    key(&mut view, &state, KeyCode::Char(' '));
    let ViewOutcome::CloseSaying { line, .. } = key(&mut view, &state, KeyCode::Esc) else {
        panic!("Esc says what changed");
    };
    assert_eq!(
        line,
        "Set Default approval mode to Last used · Set Editor mode to vim"
    );
}

#[test]
fn esc_with_nothing_changed_says_the_dialog_was_dismissed() {
    let state = state();
    let mut view = opened();
    key(&mut view, &state, KeyCode::Down);
    let ViewOutcome::CloseSaying { command, line } = key(&mut view, &state, KeyCode::Esc) else {
        panic!("Esc closes with a line");
    };
    assert_eq!(
        (command.as_str(), line.as_str()),
        ("/config", "Config dialog dismissed")
    );
}

/// A row that changes safety or what the team shares asks where to save, in All settings; a
/// value the project holds says so on its row.
#[test]
fn shared_rows_and_rows_a_project_holds_open_all_settings() {
    let state = state();
    let mut tab = ConfigTab::opened_by("config");
    let mut held = settings();
    held[3] = setting("ui.motion", "\"reduced\"", "project");
    tab.set_rows(held);
    let mut view = OverviewView::new(Tab::Config, UsageWatch::default()).with_config(tab);
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("Reduce motion") && row.contains("true · set by the project")),
        "{shown:#?}"
    );
    key(&mut view, &state, KeyCode::Down);
    for _ in 0..3 {
        key(&mut view, &state, KeyCode::Down);
    }
    assert!(matches!(
        key(&mut view, &state, KeyCode::Enter),
        ViewOutcome::Open(_)
    ));
}

/// The tab with Default approval mode a safety key, as the registry marks it.
fn with_a_safety_start_mode() -> OverviewView {
    let mut tab = ConfigTab::opened_by("config");
    let mut rows = settings();
    for row in &mut rows {
        if row.key == "approvals.start_mode" {
            row.safety = true;
            row.allowed = [
                "last",
                "approvals",
                "supervised",
                "auto_preview",
                "auto",
                "autopilot",
            ]
            .iter()
            .map(|mode| format!("\"{mode}\""))
            .collect();
        }
    }
    tab.set_rows(rows);
    tab.set_mode_now(Some("autopilot".to_owned()));
    OverviewView::new(Tab::Config, UsageWatch::default()).with_config(tab)
}

/// On the row, from the search box.
fn on_row(view: &mut OverviewView, state: &AppState, words: &str) {
    for ch in words.chars() {
        key(view, state, KeyCode::Char(ch));
    }
    key(view, state, KeyCode::Down);
}

/// A row that asks where steps in place like any other, then asks where to save in the rows'
/// place; the panel stays up over the app's safety question, and Esc says the change once it
/// is saved, in the row's words.
#[test]
fn a_safety_row_steps_asks_where_and_says_its_change_once_saved() {
    let mut state = state();
    let mut view = with_a_safety_start_mode();
    on_row(&mut view, &state, "default approval");
    assert!(matches!(
        key(&mut view, &state, KeyCode::Right),
        ViewOutcome::Consumed
    ));
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown.iter().any(|row| row.contains("Where to save it")),
        "{shown:#?}"
    );
    for place in ["Just this session", "Everywhere", "Enter to save"] {
        assert!(shown.iter().any(|row| row.contains(place)), "{shown:#?}");
    }
    let sent = key(&mut view, &state, KeyCode::Enter);
    let ViewOutcome::SaveQuietly(UiCommand::SetSetting {
        key: saved,
        value,
        scope,
    }) = sent
    else {
        panic!("{sent:?}");
    };
    assert_eq!(
        (saved.as_str(), value.as_str(), scope),
        (
            "approvals.start_mode",
            "\"approvals\"",
            SettingScope::Global
        )
    );
    assert!(view.stays_over_question(), "the app may ask about it");
    state
        .setting_outcomes
        .push(SettingOutcome::Changed { key: saved, value });
    rows(&mut view, &state, 100);
    assert!(!view.stays_over_question());
    let closed = key(&mut view, &state, KeyCode::Esc);
    assert!(
        matches!(&closed, ViewOutcome::CloseSaying { command, line }
            if command == "/config" && line.starts_with("Set Default approval mode to ")),
        "{closed:?}"
    );
}

/// A change its question left as it was takes the row's old value back, and Esc says nothing
/// changed.
#[test]
fn a_change_left_as_it_was_takes_its_value_back() {
    let mut state = state();
    let mut view = with_a_safety_start_mode();
    on_row(&mut view, &state, "network");
    key(&mut view, &state, KeyCode::Enter);
    assert!(matches!(
        key(&mut view, &state, KeyCode::Enter),
        ViewOutcome::SaveQuietly(_)
    ));
    let before = rows(&mut view, &state, 100);
    assert!(
        before
            .iter()
            .any(|row| row.contains("Network") && row.contains("false")),
        "{before:#?}"
    );
    state.setting_outcomes.push(SettingOutcome::NotChanged);
    let after = rows(&mut view, &state, 100);
    assert!(
        after
            .iter()
            .any(|row| row.contains("Network") && row.contains("true")),
        "{after:#?}"
    );
    let closed = key(&mut view, &state, KeyCode::Esc);
    assert!(
        matches!(&closed, ViewOutcome::CloseSaying { line, .. } if line == "Config dialog dismissed"),
        "{closed:?}"
    );
}

/// Esc on the question of where goes back to the rows with nothing sent.
#[test]
fn esc_on_the_question_of_where_goes_back_to_the_rows() {
    let state = state();
    let mut view = with_a_safety_start_mode();
    on_row(&mut view, &state, "network");
    key(&mut view, &state, KeyCode::Enter);
    assert!(matches!(
        key(&mut view, &state, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    let shown = rows(&mut view, &state, 100);
    assert!(
        !shown.iter().any(|row| row.contains("Where to save it")),
        "{shown:#?}"
    );
    assert!(!view.stays_over_question());
}

/// A link row leaves the panel for its own picker; the last row opens All settings over it.
#[test]
fn links_open_their_pickers_and_the_last_row_all_settings() {
    let state = state();
    let mut view = opened();
    for ch in "theme".chars() {
        key(&mut view, &state, KeyCode::Char(ch));
    }
    key(&mut view, &state, KeyCode::Down);
    let opened = key(&mut view, &state, KeyCode::Enter);
    assert!(
        matches!(
            &opened,
            ViewOutcome::SendToConversation(UiCommand::Slash {
                command: SlashCommand::Theme { name: None }
            })
        ),
        "{opened:?}"
    );
    let mut view = self::opened();
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::End);
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("❯ All settings · 713 keys ›")),
        "{shown:#?}"
    );
    assert!(matches!(
        key(&mut view, &state, KeyCode::Enter),
        ViewOutcome::Open(_)
    ));
}

/// A row that opens a picker after the tab changed something says that change under
/// `/config` first, so leaving for the picker never loses it.
#[test]
fn a_link_row_after_a_change_says_the_change_first() {
    let state = state();
    let mut view = opened();
    for _ in 0..3 {
        key(&mut view, &state, KeyCode::Down);
    }
    // Show tips, true, turned off.
    key(&mut view, &state, KeyCode::Enter);
    key(&mut view, &state, KeyCode::Up);
    key(&mut view, &state, KeyCode::Up);
    key(&mut view, &state, KeyCode::Up);
    for ch in "theme".chars() {
        key(&mut view, &state, KeyCode::Char(ch));
    }
    key(&mut view, &state, KeyCode::Down);
    let left = key(&mut view, &state, KeyCode::Enter);
    let ViewOutcome::CloseSayingThen {
        command,
        line,
        then,
    } = left
    else {
        panic!("{left:?}");
    };
    assert_eq!(command, "/config");
    assert_eq!(line, "Set Show tips to false");
    assert!(
        matches!(
            then,
            UiCommand::Slash {
                command: SlashCommand::Theme { name: None }
            }
        ),
        "{then:?}"
    );
}

/// The list moves with j and k, and with ctrl+n and ctrl+p, as Claude Code's Settings list
/// does: none of them starts a search.
#[test]
fn j_k_and_ctrl_n_p_move_through_the_rows() {
    let state = state();
    let mut view = opened();
    key(&mut view, &state, KeyCode::Down);
    let pointed = |view: &mut OverviewView| {
        rows(view, &state, 100)
            .into_iter()
            .find(|row| row.trim_start().starts_with('❯'))
            .unwrap_or_default()
    };
    let ctrl = |view: &mut OverviewView, ch: char| {
        let theme = theme();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL),
            &ctx,
        );
    };
    let first = pointed(&mut view);
    key(&mut view, &state, KeyCode::Char('j'));
    let second = pointed(&mut view);
    assert_ne!(first, second);
    key(&mut view, &state, KeyCode::Char('k'));
    assert_eq!(pointed(&mut view), first);
    ctrl(&mut view, 'n');
    assert_eq!(pointed(&mut view), second);
    ctrl(&mut view, 'p');
    assert_eq!(pointed(&mut view), first);
    let shown = rows(&mut view, &state, 100);
    assert!(
        !shown
            .iter()
            .any(|row| row.contains("⌕ j") || row.contains("⌕ k")),
        "{shown:#?}"
    );
}

/// ↑ from the first row goes back to the search, and from the search to the tabs, where ←
/// and → change the tab.
#[test]
fn up_goes_from_the_rows_to_the_search_and_on_to_the_tabs() {
    let state = state();
    let mut view = opened();
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Up);
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown.iter().any(|row| row.contains("Type to filter")),
        "{shown:#?}"
    );
    key(&mut view, &state, KeyCode::Up);
    let shown = rows(&mut view, &state, 100);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("←/→/tab to switch · ↓ to return · Esc to close")),
        "{shown:#?}"
    );
    assert!(matches!(
        key(&mut view, &state, KeyCode::Right),
        ViewOutcome::Request(_)
    ));
    assert_eq!(view.tab(), Tab::Usage);
}

/// Draws `cells` one under another, each at its row of a buffer `width` cells across.
fn transcript(cells: &[(u16, &dyn HistoryCell)], width: u16, rows: u16) -> Buffer {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    let mut buf = Buffer::empty(Rect::new(0, 0, width, rows));
    for (at, cell) in cells {
        for (y, line) in cell.lines(&ctx).iter().enumerate() {
            buf.set_line(0, at + y as u16, line, width);
        }
    }
    buf
}

/// What Esc leaves under `/config`, and what `/config key=value` prints, as the picture has
/// them. Its last two rows say what `/config all` does rather than print a line.
#[test]
fn the_saved_lines_draw_as_the_picture() {
    let picture = Picture::load("config_saved");
    let width = u16::try_from(picture.width()).unwrap();
    let mut view = opened();
    let state = state();
    key(&mut view, &state, KeyCode::Down);
    for _ in 0..9 {
        key(&mut view, &state, KeyCode::Down);
    }
    key(&mut view, &state, KeyCode::Right);
    key(&mut view, &state, KeyCode::Left);
    for _ in 0..2 {
        key(&mut view, &state, KeyCode::Down);
    }
    key(&mut view, &state, KeyCode::Enter);
    key(&mut view, &state, KeyCode::Enter);
    let ViewOutcome::CloseSaying { command, line } = key(&mut view, &state, KeyCode::Esc) else {
        panic!("Esc leaves a line");
    };
    let echo = UserMessage::command(command);
    let said = ResultLine::new(Tone::Dim, line);
    let typed = UserMessage::command("/config ui.tips=false");
    let set = ResultLine::new(
        Tone::Dim,
        "Set ui.tips to false (in ~/.argonauts/config.toml)",
    );
    let cells: [(u16, &dyn HistoryCell); 4] = [(0, &echo), (1, &said), (3, &typed), (4, &set)];
    let buf = transcript(&cells, width, 5);
    // The lines under the commands. Each echo is drawn on the conversation's band, as every
    // command's is, where the picture leaves it plain.
    for row in [1, 4] {
        let region = Region {
            rows: row..row + 1,
            columns: 0..picture.width(),
            at: (0, row as u16),
        };
        assert_matches(&picture, &buf, &region, &theme());
    }
    let text = crate::picture_dump::cut(&buf, (0, 0), (picture.width(), 5));
    assert_eq!(text[0].trim_end(), picture.text()[0]);
    assert_eq!(text[3].trim_end(), picture.text()[3]);
}

/// Autopilot coming back at start says so in one amber line, two cells in and with no mark, as
/// the transcript prints the app's notice. The picture's first row stands for the start header,
/// which the app draws as its logo header and the resumed_after picture's test compares (its
/// rows 0-10, in the bridge's resumed_golden), and its last says what the setting does.
#[test]
fn autopilot_coming_back_draws_as_the_picture() {
    let picture = Picture::load("start_mode_lines");
    let width = u16::try_from(picture.width()).unwrap();
    let mut bridge = crate::bridge::Bridge::new(&crate::options::UiOptions::default());
    let mut model = crate::model::ViewModel::default();
    let notice = AppEvent::Notice {
        level: NoteLevel::Caution,
        text: AUTOPILOT_BACK.to_owned(),
    };
    bridge.on_event(&mut model, &notice, Instant::now());
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    let printed = model.history.drain_printable(&ctx);
    let line = printed
        .iter()
        .find(|line| crate::text::plain_text(std::slice::from_ref(*line)).contains(AUTOPILOT_BACK))
        .expect("the line, printed");
    let mut buf = Buffer::empty(Rect::new(0, 0, width, 2));
    buf.set_line(0, 1, line, width);
    let region = Region {
        rows: 1..2,
        columns: 0..picture.width(),
        at: (0, 1),
    };
    assert_matches(&picture, &buf, &region, &theme);
}
