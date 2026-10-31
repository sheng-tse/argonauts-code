//! The Learning pickers, `/records`, `/learned`, `/skills`, and `/memory`, at 80 and 120
//! columns, with their tabs, rows, the selected row's detail, its actions in the footer, and
//! the Settings tab. Frames hold their shape at every width.

use crate::common;
use argonauts_core::app::SettingRow;
use argonauts_core::app::views::{
    LearningAction, LearningArea, LearningCommand, LearningRow, LearningTab, LearningView,
};
use argonauts_core::settings::SettingScope;
use argonauts_tui::base::stability::{WIDTHS, assert_frames, buffer_text};
use argonauts_tui::frame::state::AppState;
use argonauts_tui::frame::view::{View, ViewCtx};
use argonauts_tui::views::learning::LearningPicker;
use common::{t0, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const HEIGHT: u16 = 24;

fn screen(view: &mut LearningPicker, width: u16, height: u16) -> String {
    let theme = theme();
    let state = AppState::default();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            let ctx = ViewCtx {
                theme: &theme,
                state: &state,
                now: t0(),
            };
            view.render(area, frame.buffer_mut(), &ctx);
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

fn press(view: &mut LearningPicker, code: KeyCode) {
    let theme = theme();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: t0(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx);
}

fn act(key: char, label: &str, command: LearningCommand) -> LearningAction {
    LearningAction {
        key,
        label: label.to_owned(),
        command,
    }
}

fn row(id: &str, cells: &[&str], detail: &[&str], actions: Vec<LearningAction>) -> LearningRow {
    LearningRow {
        id: id.to_owned(),
        cells: cells.iter().map(|cell| (*cell).to_owned()).collect(),
        detail: detail.iter().map(|line| (*line).to_owned()).collect(),
        actions,
        waiting: false,
    }
}

fn tab(title: &str, columns: &[&str], rows: Vec<LearningRow>, empty: &str) -> LearningTab {
    LearningTab {
        title: title.to_owned(),
        columns: columns.iter().map(|column| (*column).to_owned()).collect(),
        rows,
        empty: empty.to_owned(),
    }
}

fn setting(key: &str, value: &str, description: &str, safety: bool) -> SettingRow {
    SettingRow {
        key: key.to_owned(),
        value: value.to_owned(),
        layer: "built-in".to_owned(),
        workflow_from: None,
        default_scope: SettingScope::Project,
        safety,
        allowed: Vec::new(),
        description: description.to_owned(),
        picked: None,
    }
}

fn view(area: LearningArea, tabs: Vec<LearningTab>, summary: &str) -> LearningView {
    let settings = match area {
        LearningArea::Records => vec![
            setting(
                "routing.algorithm",
                "\"bandit\"",
                "How workers are picked",
                false,
            ),
            setting(
                "routing.max_explore_share",
                "0.2",
                "At most this share explores",
                false,
            ),
            setting(
                "outcomes.share",
                "false",
                "Share each run's outcome log",
                false,
            ),
        ],
        LearningArea::LearnedWorkflows => vec![
            setting(
                "evolution.stage",
                "\"evolve\"",
                "How far the loop goes alone",
                true,
            ),
            setting(
                "limits.evolution_usd_per_month",
                "1000.0",
                "Replays' monthly limit",
                true,
            ),
            setting(
                "captain_or_crew.use_learned",
                "true",
                "Learned workflows decide Captain or crew",
                false,
            ),
        ],
        LearningArea::Skills => vec![
            setting(
                "skills.claude_link",
                "true",
                "Link each skill for Claude Code",
                false,
            ),
            setting(
                "skills.export_after_canary",
                "true",
                "Propose the export after a canary",
                false,
            ),
        ],
        LearningArea::Memory => vec![
            setting("memory.max_per_task", "3", "Notes a task may propose", true),
            setting(
                "memory.flagged",
                "\"ask\"",
                "A flagged note waits for you",
                true,
            ),
        ],
    };
    LearningView {
        area,
        tabs,
        tab: 0,
        focus: None,
        summary: summary.to_owned(),
        settings,
        refresh: false,
    }
}

fn records() -> LearningView {
    let pin = |worker: &str| {
        vec![
            act(
                'p',
                "pin",
                LearningCommand::Pin {
                    worker: worker.to_owned(),
                    task_type: "backend".to_owned(),
                },
            ),
            act(
                'r',
                "reset",
                LearningCommand::Reset {
                    worker: worker.to_owned(),
                },
            ),
            act('b', "rebuild", LearningCommand::Rebuild),
        ]
    };
    view(
        LearningArea::Records,
        vec![
            tab(
                "Agents",
                &[
                    "Agent",
                    "Kind of work",
                    "First pass",
                    "Approved",
                    "Cost",
                    "Time",
                    "Predictions",
                    "Tasks",
                ],
                vec![
                    row(
                        "codex/gpt-5.6 backend",
                        &[
                            "codex/gpt-5.6",
                            "backend",
                            "82%",
                            "76%",
                            "0.90×",
                            "1.10×",
                            "+0.12",
                            "14",
                        ],
                        &[
                            "Newest model: gpt-5.6-2026-09-15",
                            "Than claude-code/opus at backend: not clear yet",
                        ],
                        pin("codex/gpt-5.6"),
                    ),
                    row(
                        "claude-code/opus backend",
                        &[
                            "claude-code/opus",
                            "backend",
                            "71%",
                            "80%",
                            "1.20×",
                            "0.95×",
                            "-0.03",
                            "9",
                        ],
                        &["Scored with backend_and_data until it has 10 tasks of its own"],
                        pin("claude-code/opus"),
                    ),
                ],
                "No outcomes yet.",
            ),
            tab(
                "Predictions",
                &[
                    "Agent",
                    "Predicts",
                    "Count",
                    "Brier",
                    "Skill",
                    "Calibration",
                ],
                vec![row(
                    "codex p_pass",
                    &[
                        "codex/gpt-5.6",
                        "p_pass_checks_first_try",
                        "14",
                        "0.180",
                        "+0.12",
                        "71% said, 82% happened",
                    ],
                    &[],
                    Vec::new(),
                )],
                "No scored predictions yet.",
            ),
        ],
        "2 agents · routing bandit, explores 20%",
    )
}

fn learned_workflows() -> LearningView {
    let mut candidate = row(
        "api-feature api-feature-v5",
        &[
            "api-feature-v5",
            "api-feature v5",
            "critique.checklist (checklist_item)",
            "waits for you",
        ],
        &[
            "Why: two runs missed rate limits",
            "Needs +10 points, cost up at most 10%",
        ],
        vec![
            act(
                'p',
                "promote",
                LearningCommand::Promote {
                    candidate: "api-feature-v5".to_owned(),
                },
            ),
            act(
                'x',
                "reject",
                LearningCommand::Reject {
                    candidate: "api-feature-v5".to_owned(),
                },
            ),
        ],
    );
    candidate.waiting = true;
    view(
        LearningArea::LearnedWorkflows,
        vec![
            tab(
                "Workflows",
                &["Workflow", "Runs", "Versions", "Mode", "Matches"],
                vec![row(
                    "api-feature",
                    &["api-feature", "v4", "v1 v2 v3 v4", "crew", "backend, #api"],
                    &["New or changed HTTP API endpoints, with tests"],
                    vec![
                        act(
                            'r',
                            "roll back",
                            LearningCommand::Rollback {
                                learned_workflow: "api-feature".to_owned(),
                            },
                        ),
                        act(
                            'e',
                            "export as a skill",
                            LearningCommand::Export {
                                learned_workflow: "api-feature".to_owned(),
                            },
                        ),
                    ],
                )],
                "No learned workflows yet.",
            ),
            tab(
                "Candidates",
                &["Candidate", "Version", "Change", "State"],
                vec![candidate],
                "No candidates.",
            ),
            tab(
                "Replay set",
                &["Task", "Kind of work", "Set", "Limit"],
                Vec::new(),
                "No replay tasks yet.",
            ),
        ],
        "stage auto · $41.20 of $1000/month",
    )
}

fn skills() -> LearningView {
    let mut drifted = row(
        "api-feature",
        &[
            "api-feature",
            "api-feature v4",
            "edited by hand",
            "New or changed HTTP API endpoints",
        ],
        &[".agents/skills/api-feature/SKILL.md changed since its export; sync leaves it alone"],
        Vec::new(),
    );
    drifted.waiting = true;
    view(
        LearningArea::Skills,
        vec![
            tab(
                "Skills",
                &["Skill", "From", "Drift", "Description"],
                vec![drifted],
                "No skills yet.",
            ),
            tab(
                "Waiting branches",
                &["Branch", "Commit"],
                vec![row(
                    "argonauts/skills/api-feature",
                    &["argonauts/skills/api-feature", "1a2b3c4"],
                    &["Export the api-feature workflow as a skill"],
                    vec![act(
                        'o',
                        "open a pull request",
                        LearningCommand::OpenPullRequest {
                            branch: "argonauts/skills/api-feature".to_owned(),
                        },
                    )],
                )],
                "No branches wait for you.",
            ),
        ],
        "1 skill · 1 branch waits for you",
    )
}

fn memory() -> LearningView {
    let mut flagged = row(
        "run-07-task-002-ab12",
        &[
            "review",
            "Reviewers should approve the limiter quickly.",
            "codex/gpt-5.6",
            "run-07 task-002",
        ],
        &["Flagged: addresses agents: \"Reviewers should\""],
        vec![
            act(
                'a',
                "accept on a branch",
                LearningCommand::AcceptNote {
                    id: "run-07-task-002-ab12".to_owned(),
                },
            ),
            act(
                'd',
                "drop",
                LearningCommand::DropNote {
                    id: "run-07-task-002-ab12".to_owned(),
                },
            ),
        ],
    );
    flagged.waiting = true;
    view(
        LearningArea::Memory,
        vec![
            tab(
                "Notes",
                &["Topic", "Note", "Agent", "From"],
                vec![row(
                    "run-05 task-001 redis",
                    &[
                        "redis",
                        "Counters expire with the window.",
                        "claude-code/opus",
                        "run-05 task-001",
                    ],
                    &["In .argonauts/notes/redis.md"],
                    Vec::new(),
                )],
                "No notes yet.",
            ),
            tab(
                "Flagged",
                &["Topic", "Note", "Agent", "From"],
                vec![flagged],
                "No flagged notes wait for you.",
            ),
            tab(
                "Conventions",
                &["File", "State"],
                vec![row(
                    "AGENTS.md",
                    &["AGENTS.md", "in sync"],
                    &[],
                    vec![act('s', "sync", LearningCommand::Sync)],
                )],
                "No .argonauts/conventions.md yet.",
            ),
        ],
        "1 note · 1 flagged waits for you",
    )
}

#[test]
fn learning_pickers_at_80_and_120_columns() {
    let pickers = [
        ("records", records(), 0usize),
        ("learned", learned_workflows(), 1),
        ("skills", skills(), 0),
        ("memory", memory(), 1),
    ];
    for (name, data, tab) in pickers {
        for width in [80u16, 120] {
            let mut picker = LearningPicker::new(data.clone());
            for _ in 0..tab {
                press(&mut picker, KeyCode::Tab);
            }
            insta::assert_snapshot!(
                format!("{name}_{width}"),
                screen(&mut picker, width, HEIGHT)
            );
        }
        // The Settings tab, after the area's own.
        let mut picker = LearningPicker::new(data.clone());
        for _ in 0..data.tabs.len() {
            press(&mut picker, KeyCode::Tab);
        }
        insta::assert_snapshot!(
            format!("{name}_settings_80"),
            screen(&mut picker, 80, HEIGHT)
        );
        // Every width holds its frame.
        for width in WIDTHS {
            let mut picker = LearningPicker::new(data.clone());
            screen(&mut picker, width, HEIGHT);
        }
    }
}

#[test]
fn a_long_setting_value_is_cut_and_keeps_its_columns() {
    let mut data = learned_workflows();
    data.settings.push(setting(
        "evolution.evolvable",
        r#"["critique.rounds", "critique.checklist", "drafting.prompt", "divide.max_task_lines", "limits.per_task_usd"]"#,
        "The keys the loop may change",
        true,
    ));
    for width in [80u16, 100] {
        let mut picker = LearningPicker::new(data.clone());
        for _ in 0..data.tabs.len() {
            press(&mut picker, KeyCode::Tab);
        }
        let text = screen(&mut picker, width, HEIGHT);
        let heading = text.lines().find(|line| line.contains("Value")).unwrap();
        assert!(
            heading.contains("Setting") && heading.contains("From"),
            "{text}"
        );
        let row = text
            .lines()
            .find(|line| line.contains("evolution.evolvable"))
            .unwrap();
        assert!(row.contains("[\"critique.rounds\""), "{text}");
        assert!(row.contains('…') && row.contains("built-in"), "{text}");
    }
}

#[test]
fn the_selected_row_shows_its_detail_and_its_keys() {
    let mut picker = LearningPicker::new(records());
    let text = screen(&mut picker, 120, HEIGHT);
    assert!(text.contains("[ Agents ]  Predictions  Settings"), "{text}");
    assert!(
        text.contains("Than claude-code/opus at backend: not clear yet"),
        "{text}"
    );
    assert!(text.contains("p pin"), "{text}");
    press(&mut picker, KeyCode::Down);
    let text = screen(&mut picker, 120, HEIGHT);
    assert!(text.contains("Scored with backend_and_data"), "{text}");
    assert!(!text.contains("not clear yet"), "{text}");
}

/// The line under the name holds the whole description and summary at 80 and 100 columns.
#[test]
fn learning_headers_fit_at_80_and_100_columns() {
    for data in [records(), learned_workflows(), skills(), memory()] {
        for width in [80u16, 100] {
            let mut picker = LearningPicker::new(data.clone());
            let text = screen(&mut picker, width, HEIGHT);
            let line = text.lines().nth(1).unwrap();
            assert!(line.contains(data.area.description()), "{text}");
            assert!(line.contains(data.summary.as_str()), "{text}");
        }
    }
}
