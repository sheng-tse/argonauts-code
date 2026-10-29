use argonauts_core::app::{HeaderAndBottomLine, PlanUsageChip};
use argonauts_core::limits::{HealthState, PlanUsageReading, PlanUsageSource, PlanUsageStatus};
use argonauts_core::names::AgentId;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::ui::NarratorMode;
use time::macros::datetime;

use super::*;
use crate::kit::logo::golden::{pictured, with_stand_ins};
use crate::state::binding_reading;
use crate::theme::Glyphs;

const NOW: OffsetDateTime = datetime!(2026-09-30 10:01 UTC);

fn reading(agent_id: &str, window: &str, used: f64) -> PlanUsageReading {
    PlanUsageReading {
        agent_id: AgentId(agent_id.to_owned()),
        status: PlanUsageStatus::Allowed,
        used: Some(used),
        resets_at: None,
        window: Some(window.to_owned()),
        source: PlanUsageSource::RateLimitEvent,
        observed_at: datetime!(2026-09-30 10:00 UTC),
        detail: None,
        wrap_up: false,
    }
}

fn chip(label: &str, windows: Vec<PlanUsageReading>, health: HealthState) -> PlanUsageChip {
    PlanUsageChip {
        label: label.into(),
        reading: binding_reading(&windows).cloned(),
        health,
        windows,
    }
}

/// Claude Code in use at 71% and 59%, Codex used most and at 84%, agy out until Tuesday,
/// OpenCode with no plan and six cents spent this week, and OpenRouter down.
fn state() -> AppState {
    let mut out = reading("antigravity-cli", "five_hour", 1.0);
    out.status = PlanUsageStatus::Rejected;
    out.resets_at = Some(datetime!(2026-10-06 15:45 UTC));
    let fine = HealthState::Healthy;
    let plan_chips = vec![
        chip(
            "Claude Code",
            vec![
                reading("claude-code", "five_hour", 0.71),
                reading("claude-code", "seven_day", 0.59),
            ],
            fine,
        ),
        chip(
            "Codex",
            vec![
                reading("codex", "five_hour", 0.12),
                reading("codex", "seven_day", 0.84),
            ],
            fine,
        ),
        chip("agy", vec![out], fine),
        chip("OpenCode", Vec::new(), fine),
        chip("OpenRouter", Vec::new(), HealthState::Down),
    ];
    let mut line = LineFacts {
        most_used: vec!["claude-code".into(), "codex".into(), "opencode".into()],
        ..LineFacts::default()
    };
    line.spent_this_week.insert("opencode".into(), 0.06);
    let mut state = AppState::default();
    state.terminal.utc_offset = Some(UtcOffset::UTC);
    state.status = Some(HeaderAndBottomLine {
        title: String::new(),
        repo: "~/code/notes".into(),
        workflow: "captain".into(),
        ultracode: false,
        ultra: None,
        run: None,
        main_agent: "claude-code".parse().unwrap(),
        plan_chips,
        line: Box::new(line),
        spent: None,
        limit: None,
        narrator: NarratorMode::Off,
        approval_mode: ApprovalMode::Supervised,
        awake: None,
    });
    state
}

fn mode() -> BottomRow {
    BottomRow {
        approval: crate::bottom::ApprovalMode::Supervised,
        footer_hints: true,
        ..BottomRow::default()
    }
}

fn picker(state: &AppState) -> LinePicker {
    LinePicker::new(
        state,
        mode(),
        &UiSettings::default(),
        &BTreeMap::new(),
        None,
    )
}

fn theme(logos: bool) -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    if logos { pictured(&theme) } else { theme }
}

/// The picker's rows as a terminal shows them, with each logo as its picture's stand-ins.
fn screen(picker: &LinePicker, state: &AppState, logos: bool, width: u16) -> Vec<String> {
    let lines = picker.lines(state, &theme(logos), width, NOW, Instant::now());
    let height = u16::try_from(lines.len()).unwrap();
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    for (y, line) in lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, width);
    }
    let shown = with_stand_ins(&buf);
    (0..height)
        .map(|y| {
            let row: String = (0..width).map(|x| shown[(x, y)].symbol()).collect();
            row.trim_end().to_owned()
        })
        .collect()
}

fn press(
    picker: &mut LinePicker,
    state: &AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> ViewOutcome {
    let theme = theme(false);
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    picker.handle_key(KeyEvent::new(code, modifiers), &ctx)
}

fn key(picker: &mut LinePicker, state: &AppState, code: KeyCode) -> ViewOutcome {
    press(picker, state, code, KeyModifiers::NONE)
}

fn saved(outcome: ViewOutcome) -> (Vec<LineCli>, bool, Option<SettingScope>) {
    match outcome {
        ViewOutcome::SendToConversation(UiCommand::Slash {
            command:
                SlashCommand::BottomLine {
                    change: LineChange::Set { clis, holding_up },
                    scope,
                },
        }) => (clis, holding_up, scope),
        other => panic!("not a save: {other:?}"),
    }
}

/// What a bare `/bottom-line` opens at 120 columns, cell for cell: every CLI the line can
/// show with its windows, why it's there and its meters, the holding-up choice, then the line
/// itself as the picks draw it, and the keys.
#[test]
fn the_picker_at_120_columns_is_its_picture() {
    let state = state();
    let rule = "─".repeat(120);
    let want = [
        rule.as_str(),
        "  The bottom line",
        "  Which CLIs show their plan meters under the input, and in what order. The one in use always comes first.",
        "",
        "  ❯ ☒ ◧◨ Claude Code  both windows   in use                   5h ▄▄▄▄▁▁  71%  7d ▄▄▄▄▁▁  59%",
        "    ☒ ◧◨ Codex        fuller window  most used, 30 days       7d ▄▄▄▄▄▁  84%",
        "    ☐ ◧◨ agy          fuller window  when it holds up a task  out until Tue 3:45pm",
        "    ☐ ◧◨ OpenCode                    no plan                  $0.06 spent this week",
        "    ☐ ◧◨ OpenRouter                  no plan                  down",
        "",
        "    ☒ Add a CLI that holds up a running task: out, down or 90% used",
        "",
        "  The line, as it will look:",
        "  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts      ◧◨ 5h ▄▄▄▄▁▁  71%  7d ▄▄▄▄▁▁  59%   ◧◨ 7d ▄▄▄▄▄▁  84%",
        "",
        "  space shows or hides · shift+↑↓  moves · ←  →  windows · enter saves everywhere · ctrl+s this session · esc cancels",
    ];
    assert_eq!(screen(&picker(&state), &state, true, 120), want);
}

/// Where the terminal draws no logos each row keeps its colour dot in the logo's two cells,
/// and on a narrow one the words wrap and the line gives up what it gives up there.
#[test]
fn the_picker_without_logos_and_at_80_columns() {
    let state = state();
    let rows = screen(&picker(&state), &state, false, 80);
    // The longer reasons give way before any row is cut, and the CLI in use keeps both windows.
    assert_eq!(
        rows[5],
        "  ❯ ☒ ●  Claude Code  both windows   in use   5h ▄▄▄▄▁▁  71%  7d ▄▄▄▄▁▁  59%"
    );
    assert_eq!(
        rows[8],
        "    ☐ ●  OpenCode                    no plan  $0.06 spent this week"
    );
    assert!(rows.iter().all(|row| !row.ends_with('…')), "{rows:#?}");
    assert_eq!(
        &rows[2..4],
        [
            "  Which CLIs show their plan meters under the input, and in what order. The one",
            "  in use always comes first."
        ]
    );
    assert_eq!(
        rows[14],
        "  ǁ supervised mode on     ● Claude Code 5h ▄▄▄▄▁▁  71%   ○ Codex 7d ▄▄▄▄▄▁  84%"
    );
    // The keys wrap between keys, never inside one.
    assert_eq!(
        &rows[16..],
        [
            "  space shows or hides · shift+↑↓  moves · ←  →  windows",
            "  enter saves everywhere · ctrl+s this session · esc cancels"
        ]
    );
    assert!(rows.iter().all(|row| row.chars().count() <= 80));
}

/// Space shows a CLI, shift+↑ moves it up, → picks its windows, and the line below shows the
/// picks before Enter saves them as an ordered list.
#[test]
fn the_keys_pick_order_and_windows_and_enter_saves_them() {
    let state = state();
    let mut picker = picker(&state);
    key(&mut picker, &state, KeyCode::Down);
    key(&mut picker, &state, KeyCode::Down);
    key(&mut picker, &state, KeyCode::Char(' '));
    press(&mut picker, &state, KeyCode::Up, KeyModifiers::SHIFT);
    key(&mut picker, &state, KeyCode::Right);
    let rows = screen(&picker, &state, true, 120);
    assert_eq!(
        rows[5],
        "  ❯ ☒ ◧◨ agy          5h window                               out until Tue 3:45pm"
    );
    assert_eq!(
        rows[6],
        "    ☒ ◧◨ Codex        fuller window  most used, 30 days       7d ▄▄▄▄▄▁  84%"
    );
    // The line under the picker shows agy between the two, as it will be.
    assert!(
        rows[13].ends_with("59%   ◧◨ out until Tue 3:45pm   ◧◨ 7d ▄▄▄▄▄▁  84%"),
        "{}",
        rows[13]
    );

    let mut agy = LineCli::new("antigravity-cli");
    agy.windows = Some(PlanUsageWindows::FiveHours);
    let (clis, holding_up, scope) = saved(key(&mut picker, &state, KeyCode::Enter));
    assert_eq!(
        clis,
        [LineCli::new("claude-code"), agy, LineCli::new("codex")]
    );
    assert!(holding_up);
    assert_eq!(scope, None);
}

/// The one in use can't be hidden or moved; the last row turns holding up off; Ctrl+S keeps
/// the picks for this session; Esc leaves without saving.
#[test]
fn the_cli_in_use_stays_first_and_ctrl_s_keeps_it_for_the_session() {
    let state = state();
    let mut picker = picker(&state);
    assert!(matches!(
        key(&mut picker, &state, KeyCode::Char(' ')),
        ViewOutcome::Notice(_)
    ));
    press(&mut picker, &state, KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(picker.rows[0].id, "claude-code");
    // → on the one in use picks its fuller window, and ← takes it back.
    key(&mut picker, &state, KeyCode::Right);
    assert_eq!(picker.rows[0].windows, Some(PlanUsageWindows::Fullest));
    key(&mut picker, &state, KeyCode::Left);
    assert_eq!(picker.rows[0].windows, None);

    for _ in 0..9 {
        key(&mut picker, &state, KeyCode::Down);
    }
    key(&mut picker, &state, KeyCode::Char(' '));
    let rows = screen(&picker, &state, true, 120);
    assert_eq!(
        rows[10],
        "  ❯ ☐ Add a CLI that holds up a running task: out, down or 90% used"
    );
    // A CLI the line doesn't show no longer says it would join.
    assert_eq!(
        rows[6],
        "    ☐ ◧◨ agy          fuller window                           out until Tue 3:45pm"
    );
    let (clis, holding_up, scope) = saved(press(
        &mut picker,
        &state,
        KeyCode::Char('s'),
        KeyModifiers::CONTROL,
    ));
    assert_eq!(clis, [LineCli::new("claude-code"), LineCli::new("codex")]);
    assert!(!holding_up);
    assert_eq!(scope, Some(SettingScope::Session));
    assert!(matches!(
        key(&mut picker, &state, KeyCode::Esc),
        ViewOutcome::Close
    ));
}

/// A CLI with no plan has no windows to pick, and the keys say why.
#[test]
fn a_cli_without_a_plan_has_no_windows_to_pick() {
    let state = state();
    let mut picker = picker(&state);
    for _ in 0..3 {
        key(&mut picker, &state, KeyCode::Down);
    }
    assert_eq!(picker.rows[picker.pointed].id, "opencode");
    match key(&mut picker, &state, KeyCode::Right) {
        ViewOutcome::Notice(text) => assert_eq!(text, "OpenCode has no plan windows to pick"),
        other => panic!("{other:?}"),
    }
}

/// With a place named, as `/bottom-line --project` does, Enter saves there and says so.
#[test]
fn enter_saves_where_the_command_said() {
    let state = state();
    let mut picker = LinePicker::new(
        &state,
        mode(),
        &UiSettings::default(),
        &BTreeMap::new(),
        Some(SettingScope::Project),
    );
    let rows = screen(&picker, &state, true, 120);
    assert_eq!(
        rows[15..],
        [
            "  space shows or hides · shift+↑↓  moves · ←  →  windows · enter saves to this project · ctrl+s this session",
            "  esc cancels"
        ]
    );
    // Enter with nothing changed writes nothing.
    assert!(matches!(
        key(&mut picker, &state, KeyCode::Enter),
        ViewOutcome::Close
    ));
    key(&mut picker, &state, KeyCode::Down);
    key(&mut picker, &state, KeyCode::Char(' '));
    let (_, _, scope) = saved(key(&mut picker, &state, KeyCode::Enter));
    assert_eq!(scope, Some(SettingScope::Project));
}
