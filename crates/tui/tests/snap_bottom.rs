mod common;

use argonauts_core::display_names::DisplayNames;
use argonauts_tui::app::compose_live;
use argonauts_tui::bottom::PanelEvent;
use argonauts_tui::model::{
    Activity, AgentRow, AgentState, BottomState, CardNote, CardOption, CardPrompt, DiffView, Hint,
    History, StatusPart, Tone, ToolCall, ToolItem, ToolKind, ToolState, VimMode, WorkingLine,
};
use argonauts_tui::stability;
use argonauts_tui::theme::Role;
use common::{
    assert_fits, at, bg, ctx, fg, row_styled, row_text, row_with, style_of, t0, text, theme,
};
use ratatui::style::Modifier;

const WIDTHS: [u16; 2] = [80, 120];

/// Seconds into the run for every frame here: 4m 12s, with the spinner on `✻`.
const NOW: f64 = 252.48;

fn idle() -> BottomState {
    let mut bottom = BottomState::new();
    bottom.composer.label = Some("peer-crew".into());
    bottom
}

fn crew() -> BottomState {
    let mut bottom = idle();
    let mut working = WorkingLine::new("Building", t0());
    working.tokens_down = 48_200;
    working.details = vec!["build".into(), "3 agents".into()];
    bottom.working = Some(working);
    bottom.agents.rows = vec![
        agent(
            "claude-code",
            "card-002",
            AgentState::Working,
            Some((2, 5)),
            0.0,
            18_100,
        )
        .with_model("opus"),
        agent(
            "codex",
            "card-003",
            AgentState::Working,
            Some((1, 5)),
            32.0,
            12_900,
        )
        .with_model("gpt-5.6"),
        {
            let mut row = agent("agy", "review", AgentState::Waiting, Some((0, 3)), 0.0, 0)
                .with_model("gemini-3.6");
            row.started_at = None;
            row.note = Some("waiting for card-002".into());
            row
        },
    ];
    bottom.mode.background_agents = 3;
    bottom
}

fn agent(
    name: &str,
    task: &str,
    state: AgentState,
    steps: Option<(u32, u32)>,
    started: f64,
    tokens: u64,
) -> AgentRow {
    let mut row = AgentRow::new(name, task, state);
    row.steps = steps;
    row.started_at = Some(at(started));
    row.tokens_down = tokens;
    row
}

fn merge_card() -> CardPrompt {
    let options = vec![
        CardOption::irreversible("Merge"),
        CardOption::new("Open a pull request"),
        CardOption::checklist(
            "Request changes on some cards",
            vec!["card-002 Add the JSON writer".into()],
        ),
        CardOption::new("Keep the branch"),
        CardOption::irreversible("Abandon the run"),
        CardOption::view("Show the diff"),
    ];
    let mut card = CardPrompt::new(2, "Merge card-002 into main?", "Merge card-002?", options);
    card.detail = vec!["3 files · +48 -6 · tests passed · agy reviewed it and approved".into()];
    card.allow_note = true;
    card.focus();
    card
}

/// Snapshots at 80 and 120 columns, and the frame check at every width.
fn snapshot_widths(name: &str, bottom: &BottomState) {
    let theme = theme();
    for width in stability::WIDTHS {
        stability::assert_lines(&bottom.render(&ctx(&theme, width, NOW)).lines, width);
    }
    for width in WIDTHS {
        let view = bottom.render(&ctx(&theme, width, NOW));
        assert!(!view.lines.is_empty(), "{name} rendered nothing at {width}");
        assert_fits(&view.lines, width);
        insta::assert_snapshot!(format!("{name}_{width}"), text(&view.lines));
    }
}

#[test]
fn idle_prompt() {
    snapshot_widths("idle", &idle());
    let theme = theme();
    let view = idle().render(&ctx(&theme, 64, NOW));
    let rows: Vec<String> = view.lines.iter().map(|l| row_text(l)).collect();
    assert_eq!(
        rows.len(),
        4,
        "rule, input, rule, mode line:\n{}",
        rows.join("\n")
    );
    assert!(
        rows[0].starts_with('─') && rows[0].ends_with(" peer-crew ─"),
        "{}",
        rows[0]
    );
    assert!(rows[1].starts_with('❯'), "{}", rows[1]);
    assert_eq!(rows[2], "─".repeat(64));
    assert_eq!(
        rows[3],
        "  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts"
    );
    assert_eq!(view.cursor, Some((2, 1)), "the cursor waits after `❯ `");
    assert_eq!(
        style_of(&view.lines, "──").and_then(|s| s.fg),
        fg(Role::Rule)
    );
    assert_eq!(
        style_of(&view.lines, "peer-crew").and_then(|s| s.fg),
        fg(Role::Label)
    );
    assert_eq!(
        style_of(&view.lines, "ǁ supervised mode on").and_then(|s| s.fg),
        fg(Role::ModeSupervised)
    );
    assert_eq!(
        style_of(&view.lines, "? for shortcuts").and_then(|s| s.fg),
        fg(Role::Dim)
    );
    // Nothing animates on an idle prompt, so the screen never redraws on its own.
    assert_eq!(idle().next_change(&ctx(&theme, 64, NOW)), None);
}

#[test]
fn working_with_agents() {
    snapshot_widths("working_agents", &crew());
    let theme = theme();
    let view = crew().render(&ctx(&theme, 100, NOW));
    let all = text(&view.lines);
    assert!(
        all.starts_with("✻ Building… (4m 12s · ↓ 48.2k tokens · build · 3 agents)\n\n"),
        "{all}"
    );
    assert!(
        all.contains("  ǁ supervised mode on (shift+tab to cycle) · esc to interrupt · ← 3 agents · ↓ to manage"),
        "{all}"
    );
    for row in ["claude-code", "codex", "agy"] {
        assert!(
            row_with(&view.lines, row).is_some(),
            "{row} has a row:\n{all}"
        );
    }
    // Steps and tokens in their columns, and the times at the edge.
    let row = |name: &str| {
        row_with(&view.lines, name)
            .map(row_text)
            .unwrap_or_default()
    };
    let claude = row("claude-code/opus");
    assert!(claude.contains("2/5  ↓ 18.1k tokens"), "{all}");
    assert!(claude.trim_end().ends_with("4m12s"), "{all}");
    let codex = row("codex/gpt-5.6");
    assert!(codex.contains("1/5  ↓ 12.9k tokens"), "{all}");
    assert_eq!(
        claude.trim_end().chars().count(),
        codex.trim_end().chars().count(),
        "{all}"
    );
    assert!(
        row_with(&view.lines, "agy/gemini-3.6")
            .is_some_and(|row| row_text(row).ends_with("waiting for card-002"))
    );
    assert_eq!(
        style_of(&view.lines, "✻").and_then(|s| s.fg),
        fg(Role::Accent)
    );
    assert_eq!(
        style_of(&view.lines, "4m 12s").and_then(|s| s.fg),
        fg(Role::Dim)
    );
    let bar = row_with(&view.lines, "claude-code").unwrap();
    let fills: Vec<_> = bar.spans.iter().filter_map(|s| s.style.bg).collect();
    assert!(
        fills.contains(&bg(Role::BarFill).unwrap()) && fills.contains(&bg(Role::BarTrack).unwrap()),
        "{}",
        row_styled(bar)
    );
    let next = crew()
        .next_change(&ctx(&theme, 100, NOW))
        .expect("it animates");
    assert!(next > at(NOW) && next <= at(NOW + 1.0));
}

#[test]
fn the_working_line_reads_like_claude_code() {
    let theme = theme();
    let mut bottom = idle();
    let mut working = WorkingLine::new("Rowing", t0());
    working.activity = Activity::Thinking { since: t0() };
    bottom.working = Some(working);
    let first = |bottom: &BottomState, elapsed: f64| {
        text(&bottom.render(&ctx(&theme, 100, elapsed)).lines[..1])
    };
    assert!(
        first(&bottom, 3.0).contains("thinking"),
        "{}",
        first(&bottom, 3.0)
    );
    assert!(
        first(&bottom, 12.0).contains("still thinking"),
        "{}",
        first(&bottom, 12.0)
    );
    assert!(
        first(&bottom, 50.0).contains("deep in thought"),
        "{}",
        first(&bottom, 50.0)
    );
}

#[test]
fn card_expanded() {
    let mut bottom = idle();
    bottom.card = Some(merge_card());
    snapshot_widths("card_expanded", &bottom);
    let theme = theme();
    let view = bottom.render(&ctx(&theme, 80, NOW));
    let all = text(&view.lines);
    assert!(
        all.starts_with(&format!(
            "{}\n◆ Merge card-002 into main?\n  3 files · +48 -6",
            "─".repeat(80)
        )),
        "{all}"
    );
    assert!(
        all.contains("  Enter to choose · Tab to add a note · Esc to set aside"),
        "{all}"
    );
    let merge = row_with(&view.lines, "1. Merge")
        .map(|r| row_text(r))
        .unwrap();
    assert!(merge.trim_start().starts_with("❯ 1. Merge"), "{merge}");
    assert!(
        merge.trim_end().ends_with("Merge"),
        "the number alone picks it, with no letter after: {merge}"
    );
    assert!(
        !all.contains("peer-crew ─"),
        "the card stands in for the input while it has the keys: {all}"
    );
}

/// A card that needs you, under the working line, took the keys as it showed. Letters typed
/// at it change its key line, and nothing offers to guide the run.
#[test]
fn a_card_that_needs_you_holds_the_keys() {
    let mut bottom = crew();
    let mut card = merge_card();
    card.focused = false;
    card.needs_you = true;
    bottom.card = Some(card);
    assert!(bottom.hand_keys_to_card(std::time::Instant::now()));
    snapshot_widths("card_needs_you", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    let all = text(&view.lines);
    assert!(all.contains("❯ 1. Merge"), "{all}");
    assert!(!all.contains("guide the run"), "{all}");

    bottom.card.as_mut().unwrap().stray_typing = true;
    snapshot_widths("card_needs_you_typed_at", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    assert!(
        text(&view.lines).contains(
            "\n  Typing waits while the card has the keys · ↑↓ to choose · Esc to set aside"
        ),
        "{}",
        text(&view.lines)
    );
}

#[test]
fn a_new_card_waits_under_the_working_line() {
    let mut bottom = crew();
    let mut card = merge_card();
    card.focused = false;
    bottom.card = Some(card);
    snapshot_widths("card_waiting", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    let all = text(&view.lines);
    assert!(!all.contains("❯ 1."), "{all}");
    assert!(
        all.contains("❯ ↑ to answer the card, or type to guide the run"),
        "{all}"
    );
    assert!(view.cursor.is_some());
}

#[test]
fn card_pinned() {
    let mut bottom = idle();
    let mut card = merge_card();
    card.pinned = true;
    card.focused = false;
    bottom.card = Some(card);
    snapshot_widths("card_pinned", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    assert_eq!(
        text(&view.lines[..1]),
        "◆ Merge card-002? · needs you · ↑ to answer"
    );
}

#[test]
fn card_with_a_note() {
    let mut bottom = idle();
    let mut card = merge_card();
    card.selected = 1;
    card.note = Some(CardNote::with_text(1, "squash the commits first"));
    bottom.card = Some(card);
    snapshot_widths("card_note", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    let all = text(&view.lines);
    let option = all.find("2. Open a pull request").unwrap();
    let note = all
        .find("squash the commits first")
        .expect("the note shows");
    assert!(note > option, "the note sits under its option:\n{all}");
}

#[test]
fn queued_messages() {
    let mut bottom = crew();
    bottom.queued = vec![
        "also add --pretty".into(),
        "and mention it in the README".into(),
    ];
    snapshot_widths("queued", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    let all = text(&view.lines);
    let queued = all.find("also add --pretty").unwrap();
    let rule = all.find("peer-crew ─").unwrap();
    assert!(queued < rule, "queued messages sit above the input:\n{all}");
    assert_eq!(
        style_of(&view.lines, "also add --pretty").and_then(|s| s.fg),
        fg(Role::Dim)
    );
}

#[test]
fn ctrl_c_hint() {
    let mut bottom = crew();
    // The Ui sets this hint on the first Ctrl+C; inline_vt100.rs covers that.
    bottom.hint = Some(Hint {
        text: "Press Ctrl-C again to exit · the run keeps going".into(),
        until: at(NOW + 1.0),
    });
    snapshot_widths("ctrl_c_hint", &bottom);
    let all = text(&bottom.render(&ctx(&theme(), 100, NOW)).lines);
    assert!(
        all.contains("Press Ctrl-C again to exit · the run keeps going"),
        "{all}"
    );
    let later = text(&bottom.render(&ctx(&theme(), 100, NOW + 2.0)).lines);
    assert!(!later.contains("Press Ctrl-C"), "{later}");
}

#[test]
fn vim_insert() {
    let mut bottom = idle();
    bottom.mode.vim = Some(VimMode::Insert);
    snapshot_widths("vim_insert", &bottom);
    let all = text(&bottom.render(&ctx(&theme(), 80, NOW)).lines);
    assert!(
        all.ends_with("  -- INSERT -- ǁ supervised mode on (shift+tab to cycle)"),
        "{all}"
    );
}

#[test]
fn multi_line_draft() {
    let mut bottom = idle();
    bottom.composer.set_text(
        "Add a --json flag to the export command.\nKeep the CSV output as it is, and make the \
         JSON output one object per line so long exports can stream.\nThen update the help.",
    );
    snapshot_widths("multi_line_draft", &bottom);
    let theme = theme();
    let view = bottom.render(&ctx(&theme, 80, NOW));
    let rows: Vec<String> = view.lines.iter().map(|l| row_text(l)).collect();
    let first = rows.iter().position(|r| r.starts_with('❯')).unwrap();
    let bottom_rule = rows.iter().rposition(|r| r.starts_with('─')).unwrap();
    assert!(
        bottom_rule - first >= 4,
        "the input grows a row at a time:\n{}",
        rows.join("\n")
    );
    assert!(
        rows[first + 1..bottom_rule]
            .iter()
            .all(|r| r.starts_with("  "))
    );
    let (col, row) = view.cursor.expect("the cursor shows");
    assert_eq!(
        usize::from(row),
        bottom_rule - 1,
        "the cursor ends the draft"
    );
    assert!(col > 2);
}

#[test]
fn placeholder() {
    let mut bottom = idle();
    bottom.composer.placeholder = Some("Ask anything, or describe a change".into());
    snapshot_widths("placeholder", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    let input = row_with(&view.lines, "Ask anything").expect("the placeholder shows");
    assert!(
        row_text(input).starts_with('❯'),
        "on the input row: {}",
        row_text(input)
    );
    assert_eq!(
        style_of(&view.lines, "sk anything").and_then(|s| s.fg),
        fg(Role::Dim),
        "{}",
        row_styled(input)
    );
}

#[test]
fn status_line() {
    let mut bottom = idle();
    bottom.status_line = vec![
        StatusPart::quota("Claude 5h 38% used", 38, Tone::Dim),
        StatusPart::quota("Codex 5h 92% used", 92, Tone::Warning),
        StatusPart::spend("$0.46 today"),
    ];
    snapshot_widths("status_line", &bottom);
    // The mode line is the bottom row, with the chips at its right end.
    let view = bottom.render(&ctx(&theme(), 120, NOW));
    let all = text(&view.lines);
    let last = all.lines().last().unwrap();
    assert!(last.starts_with("  ǁ supervised mode on"), "{all}");
    assert!(
        last.ends_with("  Claude 5h 38% used · Codex 5h 92% used · $0.46 today"),
        "{all}"
    );
    assert_eq!(
        style_of(&view.lines, "Codex 5h 92%").and_then(|s| s.fg),
        fg(Role::Warning)
    );
    // A narrow screen loses whole chips, least used first, before the mode's own hints, and
    // keeps the one near its limit.
    let narrow = text(&bottom.render(&ctx(&theme(), 64, NOW)).lines);
    let last = narrow.lines().last().unwrap();
    assert!(
        last.starts_with("  ǁ supervised mode on (shift+tab to cycle)  "),
        "{narrow}"
    );
    assert!(last.ends_with("  Codex 5h 92% used"), "{narrow}");
    assert!(!narrow.contains("Claude"), "{narrow}");
}

#[test]
fn the_focused_agent_panel_marks_its_row() {
    let mut bottom = crew();
    bottom.agents.selected = Some(1);
    snapshot_widths("agents_focused", &bottom);
    let view = bottom.render(&ctx(&theme(), 100, NOW));
    let codex = row_text(row_with(&view.lines, "codex/").unwrap());
    assert!(codex.contains('❯'), "{codex}");
    // `x` asks on the mode line, like Ctrl+C's double press, and no card comes up.
    let key = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('x'));
    assert_eq!(bottom.panel_key(key), PanelEvent::StopAsked);
    let all = text(&bottom.render(&ctx(&theme(), 100, NOW)).lines);
    assert!(
        all.contains("x again to stop codex/gpt-5.6 · esc to keep"),
        "{all}"
    );
    assert!(bottom.card.is_none(), "{all}");
    let PanelEvent::Stop { name, .. } = bottom.panel_key(key) else {
        panic!("a second x on an agent row stops it");
    };
    assert_eq!(name, "codex/gpt-5.6");
    let all = text(&bottom.render(&ctx(&theme(), 100, NOW)).lines);
    assert!(!all.contains("x again"), "{all}");
}

#[test]
fn live_blocks_share_a_short_screen() {
    let theme = theme();
    let mut history = History::new();
    for (key, harness, model, role, task, file) in [
        (
            "builder",
            "claude-code",
            "opus",
            "owner · card-002",
            "Add the JSON writer",
            "src/export.rs",
        ),
        (
            "wirer",
            "codex",
            "gpt-5.6",
            "owner · card-003",
            "Wire --json into the CLI",
            "src/cli.rs",
        ),
    ] {
        let mut block = ToolCall::agent(
            DisplayNames::default().agent(harness, Some(model)),
            None,
            role,
            task,
        )
        .with_key(key);
        block.push(ToolItem::text(format!("Read {file} (212 lines)")));
        let mut update = ToolCall::call(ToolKind::Edit, "Update", file);
        update.push(ToolItem::Diff(
            DiffView::from_unified(
                "@@ -41,2 +41,3 @@\n     fn write(&self) {\n-        self.csv()\n+        match self.format {\n+            Format::Json => self.json(),\n",
            )
            .unwrap(),
        ));
        update.state = ToolState::Done;
        block.push_call(update);
        block.push(ToolItem::Markdown(
            "Adding the JSON writer next to the CSV one.".into(),
        ));
        history.push_active(block);
    }
    let bottom = crew();
    let full = compose_live(
        &history.live_blocks(&ctx(&theme, 100, NOW)),
        &history,
        &bottom,
        &ctx(&theme, 100, NOW),
        60,
    );
    let tight = compose_live(
        &history.live_blocks(&ctx(&theme, 100, NOW)),
        &history,
        &bottom,
        &ctx(&theme, 100, NOW),
        20,
    );
    assert_fits(&full.lines, 100);
    assert_fits(&tight.lines, 100);
    assert!(
        full.lines.len() > 20,
        "the blocks need more than 20 rows in full"
    );
    assert!(tight.lines.len() <= 20, "{}", text(&tight.lines));
    insta::assert_snapshot!("live_tight_100", text(&tight.lines));

    let all = text(&tight.lines);
    // Every block shrinks before any is hidden; the composer and mode line always show.
    assert!(
        all.contains("● Opus on Claude Code · owner · card-002"),
        "{all}"
    );
    assert!(
        all.contains("● GPT 5.6 on Codex · owner · card-003"),
        "{all}"
    );
    assert!(all.contains("peer-crew ─\n❯"), "{all}");
    assert!(all.contains("ǁ supervised mode on"), "{all}");
    let (_, row) = tight.cursor.expect("the cursor shows");
    let input = tight
        .lines
        .iter()
        .position(|l| row_text(l).starts_with('❯'))
        .unwrap();
    assert_eq!(usize::from(row), input);

    let mut waiting = crew();
    waiting.card = Some(merge_card());
    let with_card = compose_live(
        &history.live_blocks(&ctx(&theme, 100, NOW)),
        &history,
        &waiting,
        &ctx(&theme, 100, NOW),
        24,
    );
    let all = text(&with_card.lines);
    assert!(with_card.lines.len() <= 24);
    assert!(all.contains("6. Show the diff"), "{all}");
    assert!(
        !all.contains("peer-crew ─"),
        "the card stands in for the input: {all}"
    );
    assert!(
        style_of(&with_card.lines, "Merge card-002 into main?")
            .is_some_and(|s| s.add_modifier.contains(Modifier::BOLD) || s.fg.is_some())
    );
}
