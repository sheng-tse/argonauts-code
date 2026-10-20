use crate::common;
use argonauts_core::display_names::DisplayNames;
use argonauts_tui::app::compose_live;
use argonauts_tui::bottom::PanelEvent;
use argonauts_tui::model::{
    Activity, AgentRow, AgentState, BottomState, DiffView, Hint, History, LinePart, QuestionNote,
    QuestionOption, QuestionPrompt, Tone, ToolCall, ToolItem, ToolKind, ToolState, VimMode,
    WindowUse, WorkingLine,
};
use argonauts_tui::model::{AddedPart, CliUse};
use argonauts_tui::stability;
use argonauts_tui::theme::Role;
use common::{assert_fits, at, ctx, fg, row_styled, row_text, row_with, style_of, t0, text, theme};
use ratatui::style::Modifier;

const WIDTHS: [u16; 2] = [80, 120];

/// Seconds into the run for every frame here: 4m 12s, with the spinner on `✻`.
const NOW: f64 = 252.48;

fn idle() -> BottomState {
    let mut bottom = BottomState::new();
    bottom.composer.label = Some("council".into());
    bottom
}

fn crew() -> BottomState {
    let mut bottom = idle();
    let mut working = WorkingLine::new("Building", t0());
    working.tokens_down = 48_200;
    working.details = vec!["build".into(), "3 agents".into()];
    // A run alone is at work, and Esc leaves its agents going.
    working.esc_stops = false;
    bottom.working = Some(working);
    bottom.agents.rows = vec![
        agent(
            "claude-code",
            "task-002",
            AgentState::Working,
            Some((2, 5)),
            0.0,
            18_100,
        )
        .with_model("opus"),
        agent(
            "codex",
            "task-003",
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
            row.note = Some("waiting for task-002".into());
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

fn merge_question() -> QuestionPrompt {
    let options = vec![
        QuestionOption::irreversible("Merge"),
        QuestionOption::new("Open a pull request"),
        QuestionOption::checklist(
            "Request changes on some tasks",
            vec!["task-002 Add the JSON writer".into()],
        ),
        QuestionOption::new("Keep the branch"),
        QuestionOption::irreversible("Abandon the run"),
        QuestionOption::view("Show the diff"),
    ];
    let mut question =
        QuestionPrompt::new(2, "Merge task-002 into main?", "Merge task-002?", options);
    question.detail = vec!["3 files · +48 -6 · tests passed · agy reviewed it and approved".into()];
    question.allow_note = true;
    question.focus();
    question
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
        "rule, input, rule, bottom line:\n{}",
        rows.join("\n")
    );
    assert!(
        rows[0].starts_with('─') && rows[0].ends_with(" council ─"),
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
    // The label is in the dim grey, over the rule's. Purple is kept for the ultracode tag.
    assert_eq!(
        style_of(&view.lines, "council").and_then(|s| s.fg),
        fg(Role::Dim)
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
    // Esc leaves a run's agents going, so the line makes no offer of it.
    assert!(
        all.contains("  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts · ← 3 agents · ↓ to manage"),
        "{all}"
    );
    for row in ["claude-code", "codex", "agy"] {
        assert!(
            row_with(&view.lines, row).is_some(),
            "{row} has a row:\n{all}"
        );
    }
    // Each row ends with its to-do count, how long, and its tokens, as Claude Code ends its
    // rows, and a note such as a wait takes the place of the time.
    let row = |name: &str| {
        row_with(&view.lines, name)
            .map(row_text)
            .unwrap_or_default()
    };
    let claude = row("claude-code/opus");
    assert!(claude.ends_with("2/5 · 4m · ↓ 18.1k tokens"), "{all}");
    let codex = row("codex/gpt-5.6");
    assert!(codex.ends_with("1/5 · 3m · ↓ 12.9k tokens"), "{all}");
    let stats_at = |row: &str| row.find("/5").map(|at| row[..at].chars().count());
    assert_eq!(stats_at(&claude), stats_at(&codex), "{all}");
    assert!(
        row_with(&view.lines, "agy/gemini-3.6")
            .is_some_and(|row| row_text(row).ends_with("waiting for task-002"))
    );
    assert_eq!(
        style_of(&view.lines, "✻").and_then(|s| s.fg),
        fg(Role::Accent)
    );
    assert_eq!(
        style_of(&view.lines, "4m 12s").and_then(|s| s.fg),
        fg(Role::Dim)
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
fn question_expanded() {
    let mut bottom = idle();
    bottom.question = Some(merge_question());
    snapshot_widths("question_expanded", &bottom);
    let theme = theme();
    let view = bottom.render(&ctx(&theme, 80, NOW));
    let all = text(&view.lines);
    assert!(
        all.starts_with(&format!(
            "{}\n◆ Merge task-002 into main?\n  3 files · +48 -6",
            "─".repeat(80)
        )),
        "{all}"
    );
    assert!(
        all.contains("  Enter to choose · Tab to add a note · Esc to carry on without me"),
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
        !all.contains("council ─"),
        "the question stands in for the input while it has the keys: {all}"
    );
}

/// A question that needs you, under the working line, took the keys as it showed. Letters typed
/// at it change its key line, and nothing offers to guide the run.
#[test]
fn a_question_that_needs_you_holds_the_keys() {
    let mut bottom = crew();
    let mut question = merge_question();
    question.focused = false;
    question.needs_you = true;
    bottom.question = Some(question);
    assert!(bottom.hand_keys_to_question(std::time::Instant::now()));
    snapshot_widths("question_needs_you", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    let all = text(&view.lines);
    assert!(all.contains("❯ 1. Merge"), "{all}");
    assert!(!all.contains("guide the run"), "{all}");
}

/// Passed on while work runs, the question leaves the screen whole and the input guides the run.
/// No question that asks waits above the input, open or as a line.
#[test]
fn a_question_passed_on_under_the_working_line_leaves_the_input_to_the_run() {
    let mut bottom = crew();
    let mut question = merge_question();
    question.needs_you = true;
    question.focus();
    question.dismiss();
    bottom.question = Some(question);
    assert!(!bottom.hand_keys_to_question(std::time::Instant::now()));
    snapshot_widths("question_passed_on_working", &bottom);
    let view = bottom.render(&ctx(&theme(), 80, NOW));
    let all = text(&view.lines);
    assert!(!all.contains("Merge"), "{all}");
    assert!(!all.contains("to answer the question"), "{all}");
    assert!(view.cursor.is_some());
}

#[test]
fn question_with_a_note() {
    let mut bottom = idle();
    let mut question = merge_question();
    question.selected = 1;
    question.note = Some(QuestionNote::with_text(1, "squash the commits first"));
    bottom.question = Some(question);
    snapshot_widths("question_note", &bottom);
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
    let rule = all.find("council ─").unwrap();
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
fn bottom_line() {
    let mut bottom = idle();
    let mut claude = CliUse::new("Claude", Role::CliClaude);
    claude.main = true;
    claude.windows = vec![WindowUse::new("5h", 0.38), WindowUse::new("7d", 0.56)];
    let mut codex = CliUse::new("Codex", Role::CliCodex);
    codex.windows = vec![WindowUse::new("5h", 0.92)];
    codex.windows[0].resets = Some("4:40pm".into());
    bottom.line.right = vec![
        LinePart::PlanUsage(vec![claude, codex]),
        LinePart::Added(AddedPart::words("spend", "$0.46 today", Tone::Dim)),
    ];
    snapshot_widths("bottom_line", &bottom);
    // The bottom line is the bottom row, with the chips against its right edge.
    let view = bottom.render(&ctx(&theme(), 120, NOW));
    let all = text(&view.lines);
    let last = all.lines().last().unwrap();
    assert!(last.starts_with("  ǁ supervised mode on"), "{all}");
    assert!(last.contains("● Claude 5h "), "{all}");
    assert!(last.ends_with(" 92% resets 4:40pm  $0.46 today"), "{all}");
    assert_eq!(
        style_of(&view.lines, "92%").and_then(|s| s.fg),
        fg(Role::Warn)
    );
    // A narrow screen takes the hints and the added part first, and keeps every CLI: the one
    // near its limit in words, the one in use with its fuller window.
    let narrow = text(&bottom.render(&ctx(&theme(), 64, NOW)).lines);
    let last = narrow.lines().last().unwrap();
    assert!(last.starts_with("  ǁ supervised  "), "{narrow}");
    assert!(last.ends_with(" 92% 4:40pm"), "{narrow}");
    assert!(
        last.contains("● Claude") && last.contains(" 56%"),
        "{narrow}"
    );
    assert!(!narrow.contains("$0.46"), "{narrow}");
}

#[test]
fn the_focused_agent_panel_marks_its_row() {
    let mut bottom = crew();
    bottom.agents.selected = Some(1);
    snapshot_widths("agents_focused", &bottom);
    let view = bottom.render(&ctx(&theme(), 100, NOW));
    // The selected agent has the filled dot, the rule over the input names it, the input
    // offers to message it, and the list's keys take the bottom line.
    let codex = row_text(row_with(&view.lines, "task-003").unwrap());
    assert!(codex.starts_with("  ● codex/gpt-5.6"), "{codex}");
    let all = text(&view.lines);
    assert!(
        all.contains(" codex/gpt-5.6 ─\n❯ Message @codex/gpt-5.6…\n"),
        "{all}"
    );
    assert!(
        all.contains(
            "\n  ↑/↓ to select · enter to view · x to stop it · ctrl+x ctrl+k to stop all agents\n"
        ),
        "{all}"
    );
    // `x` asks on the bottom line, like Ctrl+C's double press, and no question comes up.
    let key = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('x'));
    assert_eq!(bottom.panel_key(key), PanelEvent::StopAsked);
    let all = text(&bottom.render(&ctx(&theme(), 100, NOW)).lines);
    assert!(
        all.contains("x again to stop codex/gpt-5.6 · esc to keep"),
        "{all}"
    );
    assert!(bottom.question.is_none(), "{all}");
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
    for (key, cli, model, role, task, file) in [
        (
            "builder",
            "claude-code",
            "opus",
            "worker · task-002",
            "Add the JSON writer",
            "src/export.rs",
        ),
        (
            "wirer",
            "codex",
            "gpt-5.6",
            "worker · task-003",
            "Wire --json into the CLI",
            "src/cli.rs",
        ),
    ] {
        let mut block = ToolCall::agent(
            DisplayNames::default().agent(cli, Some(model)),
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
    // Every block shrinks before any is hidden; the composer and bottom line always show.
    assert!(
        all.contains("● Opus on Claude Code · worker · task-002"),
        "{all}"
    );
    assert!(
        all.contains("● GPT 5.6 on Codex · worker · task-003"),
        "{all}"
    );
    assert!(all.contains("council ─\n❯"), "{all}");
    assert!(all.contains("ǁ supervised mode on"), "{all}");
    let (_, row) = tight.cursor.expect("the cursor shows");
    let input = tight
        .lines
        .iter()
        .position(|l| row_text(l).starts_with('❯'))
        .unwrap();
    assert_eq!(usize::from(row), input);

    let mut waiting = crew();
    waiting.question = Some(merge_question());
    let with_question = compose_live(
        &history.live_blocks(&ctx(&theme, 100, NOW)),
        &history,
        &waiting,
        &ctx(&theme, 100, NOW),
        24,
    );
    let all = text(&with_question.lines);
    assert!(with_question.lines.len() <= 24);
    assert!(all.contains("6. Show the diff"), "{all}");
    assert!(
        !all.contains("council ─"),
        "the question stands in for the input: {all}"
    );
    assert!(
        style_of(&with_question.lines, "Merge task-002 into main?")
            .is_some_and(|s| s.add_modifier.contains(Modifier::BOLD) || s.fg.is_some())
    );
}
