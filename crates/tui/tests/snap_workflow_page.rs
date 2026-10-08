//! The page `/workflow edit <name>` opens, drawn whole: the steps in order, the keys of the
//! one that is open, and the check under them.

mod common;

use argonauts_core::workflow_page::{PageCheck, PageField, PageGroup, WorkflowPage};
use argonauts_tui::stability::{assert_frames, buffer_text};
use argonauts_tui::state::AppState;
use argonauts_tui::views::workflow_page::WorkflowPageView;
use argonauts_tui::views::{View, ViewCtx};
use common::{t0, theme};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const SIZES: [(u16, u16); 2] = [(100, 32), (80, 24)];

fn field(key: &str, label: &str, value: &str, choices: &[&str], optional: bool) -> PageField {
    PageField {
        key: key.to_owned(),
        label: label.to_owned(),
        value: value.to_owned(),
        choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
        optional,
        about: match key {
            "role" => "Who takes it. The [crew] settings say which model fills each role.",
            "until" => "A goal on this step: its agent keeps going until this is true.",
            _ => "What the key does.",
        }
        .to_owned(),
    }
}

fn step(index: u32, name: &str, role: &str, who: &str, how: &str) -> PageGroup {
    const FLAG: &[&str] = &["false", "true"];
    let others: Vec<&str> = ["Spec", "Build", "Review", "Sign off"]
        .into_iter()
        .filter(|other| *other != name)
        .collect();
    let back = if name == "Review" { "Build" } else { "" };
    let until = if name == "Build" {
        "its acceptance tests pass"
    } else {
        ""
    };
    PageGroup {
        step: Some(index),
        title: name.to_owned(),
        role: role.to_owned(),
        who: who.to_owned(),
        how: how.to_owned(),
        fields: vec![
            field("name", "Name", name, &[], false),
            field(
                "role",
                "Role",
                role,
                &["orchestrator", "workers", "reviewers", "judge", "main"],
                false,
            ),
            field("does", "Does", "", &[], true),
            field("for_each", "For each", "task", &["task"], true),
            field("count", "Count", "", &[], true),
            field("independent", "Independent", "false", FLAG, false),
            field("not_the_author", "Not the author", "false", FLAG, false),
            field("send_back_to", "Sends back to", back, &others, true),
            field("then", "Then", "", &others, true),
            field("until", "Until", until, &[], true),
            field("kept_by", "Kept by", "", &["cli", "argonauts"], true),
            field("max_rounds", "Max rounds", "", &[], true),
            field("model", "Model", "", &[], true),
            field("own_workflow", "Own workflow", "false", FLAG, false),
        ],
    }
}

fn page() -> WorkflowPage {
    WorkflowPage {
        name: "spec-first".to_owned(),
        path: ".argonauts/workflows/spec-first.toml".to_owned(),
        about: "Write the spec first, then build to it.".to_owned(),
        check: PageCheck {
            valid: true,
            lines: vec![
                "the file is valid · about 9 to 13 agents if Spec lists 3 tasks".to_owned(),
            ],
        },
        groups: vec![
            PageGroup {
                step: None,
                title: "Workflow".to_owned(),
                role: String::new(),
                who: String::new(),
                how: "the steps run exactly as written · no goal".to_owned(),
                fields: vec![
                    field(
                        "about",
                        "About",
                        "Write the spec first, then build to it.",
                        &[],
                        false,
                    ),
                    field("adapt", "Adapt", "false", &["false", "true"], false),
                    field("goal", "Goal", "", &[], true),
                    field("max_rounds", "Max rounds", "", &[], true),
                ],
            },
            step(
                0,
                "Spec",
                "orchestrator",
                "Opus 5.5 on Claude Code",
                "1 agent",
            ),
            step(
                1,
                "Build",
                "workers",
                "GPT 5.6 on Codex",
                "one per task · until its acceptance tests pass",
            ),
            step(
                2,
                "Review",
                "reviewers",
                "Gemini 3.6 Pro, GPT 5.6",
                "2 per task · sends back to Build",
            ),
            step(
                3,
                "Sign off",
                "judge",
                "GPT 5.6 on Codex",
                "the model this step names",
            ),
        ],
        said: None,
        refresh: false,
    }
}

fn press(view: &mut dyn View, code: KeyCode) {
    let theme = theme();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: t0(),
    };
    view.handle_key(KeyEvent::from(code), &ctx);
}

/// The screen as text, trailing spaces trimmed, after the frame check.
fn screen(view: &mut dyn View, width: u16, height: u16) -> String {
    let theme = theme();
    let state = AppState::default();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let ctx = ViewCtx {
                theme: &theme,
                state: &state,
                now: t0(),
            };
            view.render(frame.area(), frame.buffer_mut(), &ctx);
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    assert_frames(&buf);
    buffer_text(&buf)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_page_lists_the_steps_in_order_with_the_check_under_them() {
    let mut view = WorkflowPageView::new(page());
    for (width, height) in SIZES {
        insta::assert_snapshot!(
            format!("workflow_page_folded_{width}"),
            screen(&mut view, width, height)
        );
    }
}

#[test]
fn an_open_step_shows_its_keys_and_what_the_one_under_the_pointer_does() {
    let mut view = WorkflowPageView::new(page());
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Down);
    for (width, height) in SIZES {
        insta::assert_snapshot!(
            format!("workflow_page_open_{width}"),
            screen(&mut view, width, height)
        );
    }
}

#[test]
fn a_line_being_typed_shows_its_cursor_and_how_to_save_it() {
    let mut view = WorkflowPageView::new(page());
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Enter);
    for _ in 0..10 {
        press(&mut view, KeyCode::Down);
    }
    press(&mut view, KeyCode::Enter);
    let text = screen(&mut view, 100, 32);
    assert!(text.contains("its acceptance tests pass▏"), "{text}");
    assert!(text.contains("⏎ save · esc leave it as it was"), "{text}");
    assert!(
        text.contains("A goal on this step: its agent keeps going until this is true."),
        "{text}"
    );
}

#[test]
fn a_file_that_does_not_check_says_why_in_place_of_the_check() {
    let mut broken = page();
    broken.check = PageCheck {
        valid: false,
        lines: vec![
            "step 3 (Review): send_back_to names Biuld, and no step is called that".to_owned(),
        ],
    };
    broken.groups[3].fields[7].value = "Biuld".to_owned();
    let mut view = WorkflowPageView::new(broken);
    press(&mut view, KeyCode::End);
    press(&mut view, KeyCode::Up);
    press(&mut view, KeyCode::Enter);
    let text = screen(&mut view, 100, 40);
    assert!(
        text.contains("✗ step 3 (Review): send_back_to names Biuld"),
        "{text}"
    );
    // The value no step answers to is shown beside the ones it may take.
    assert!(text.contains("Biuld  —  Spec  Build  Sign off"), "{text}");
}
