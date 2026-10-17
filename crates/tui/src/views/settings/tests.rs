use std::time::Instant;

use argonauts_core::app::{MissingCli, ModelChoice, SettingRow, UiCommand, ViewData, ViewRequest};
use argonauts_core::input::SlashCommand;
use argonauts_core::limits::CostEstimate;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::registry::ModelAccess;
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::ui::UiSettings;
use crossterm::event::{KeyCode, KeyEvent};

use super::{on_picker, open_for};
use crate::bottom::PickerEvent;
use crate::bridge::{Cx, Effects};
use crate::cell::HistoryCell;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::text::plain_text;
use crate::theme::Theme;

fn row(key: &str, value: &str, layer: &str, allowed: &[&str]) -> SettingRow {
    SettingRow {
        key: key.to_owned(),
        value: value.to_owned(),
        layer: layer.to_owned(),
        workflow_from: None,
        default_scope: SettingScope::Global,
        safety: false,
        allowed: allowed.iter().map(|value| (*value).to_owned()).collect(),
        description: format!("Sets {key}"),
    }
}

fn theme_row(layer: &str) -> SettingRow {
    row(
        "ui.theme",
        "\"dark\"",
        layer,
        &["\"auto\"", "\"dark\"", "\"light\""],
    )
}

fn settings(command: &str, rows: Vec<SettingRow>) -> ViewData {
    ViewData::Settings {
        rows,
        command: Some(command.to_owned()),
        section: None,
        not_installed: Vec::new(),
    }
}

fn choice(worker: &str, current: bool) -> ModelChoice {
    ModelChoice {
        worker: worker.parse().unwrap(),
        provider: "provider".into(),
        access: ModelAccess::Subscription,
        cost: CostEstimate::Unknown,
        plan_usage: None,
        efforts: Vec::new(),
        strengths: Vec::new(),
        current,
        note: None,
        older: false,
        name: String::new(),
        description: None,
        recommended: false,
        new: false,
        default_effort: None,
        effort: None,
        alias: false,
        runs: None,
    }
}

fn models(role: Option<RoleName>, choices: Vec<ModelChoice>) -> ViewData {
    ViewData::Models {
        choices,
        role,
        refreshed: None,
        refreshing: false,
        update: false,
    }
}

/// The bridge's side of a picker: answers come in through `open_for`, and keys go to the
/// picker first and then to `on_picker`, as the runtime passes them.
struct Screen {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
}

impl Screen {
    fn new() -> Self {
        Self {
            model: ViewModel::default(),
            state: AppState::default(),
            settings: UiSettings::default(),
        }
    }

    fn cx<'a>(&'a mut self, out: &'a mut Effects) -> Cx<'a> {
        Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out,
        }
    }

    fn answer(&mut self, data: ViewData) -> Effects {
        let mut out = Effects::default();
        open_for(&data, &mut self.cx(&mut out));
        out
    }

    fn press(&mut self, code: KeyCode) -> Effects {
        let mut out = Effects::default();
        let picker = self.model.bottom.picker.as_mut().expect("a picker is open");
        let event = picker.handle_key(KeyEvent::from(code));
        if event != PickerEvent::Unhandled {
            on_picker(&event, &mut self.cx(&mut out));
        }
        out
    }

    fn labels(&self) -> Vec<String> {
        let picker = self.model.bottom.picker.as_ref().expect("a picker is open");
        picker.rows.iter().map(|row| row.label.clone()).collect()
    }

    fn title(&self) -> String {
        let picker = self.model.bottom.picker.as_ref().expect("a picker is open");
        picker.title.clone()
    }

    fn transcript(&self) -> String {
        let theme = Theme::dark();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        self.model
            .history
            .cells()
            .iter()
            .map(|cell| plain_text(&cell.lines(&ctx)))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[test]
fn model_opens_on_the_main_agents_clis_with_the_roles_a_level_down() {
    let mut screen = Screen::new();
    let asked = screen.answer(settings(
        "model",
        vec![
            row("crew.main", "\"claude-code\"", "global", &[]),
            row(
                "roles.reviewer.models",
                "[\"codex/gpt-5.6\"]",
                "built-in",
                &[],
            ),
        ],
    ));
    assert_eq!(
        asked.commands,
        [UiCommand::Request {
            view: ViewRequest::Models { role: None }
        }]
    );
    assert_eq!(screen.title(), "Main agent");
    screen.answer(models(
        None,
        vec![choice("claude-code", true), choice("codex/gpt-5.6", false)],
    ));
    assert_eq!(screen.labels(), ["Claude Code", "Codex", "Roles…"]);
    assert_eq!(pointed_label(&screen), "Claude Code");

    // Roles… opens the roles, with each one's workers by name.
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Down);
    assert!(screen.press(KeyCode::Enter).commands.is_empty());
    assert_eq!(screen.title(), "Roles");
    assert_eq!(screen.labels(), ["reviewer"]);
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert_eq!(picker.rows[0].description, "GPT 5.6 on Codex");

    // Esc goes back up and asks for the main agent's list again.
    let back = screen.press(KeyCode::Esc);
    assert_eq!(
        back.commands,
        [UiCommand::Request {
            view: ViewRequest::Models { role: None }
        }]
    );
    assert!(screen.model.bottom.picker.is_some());
    assert_eq!(screen.title(), "Main agent");
}

#[test]
fn answers_land_in_the_picker_that_asked() {
    let mut screen = Screen::new();
    screen.answer(settings(
        "model",
        vec![
            row("crew.main", "\"claude-code/opus\"", "global", &[]),
            row(
                "roles.reviewer.models",
                "[\"codex/gpt-5.6\"]",
                "built-in",
                &[],
            ),
        ],
    ));
    screen.answer(models(None, vec![choice("claude-code/opus", true)]));
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Enter);
    assert_eq!(screen.labels(), ["reviewer"]);
    let asked = screen.press(KeyCode::Enter);
    assert_eq!(
        asked.commands,
        [UiCommand::Request {
            view: ViewRequest::Models {
                role: Some(RoleName::Reviewer)
            }
        }]
    );

    // A late answer for the main agent, and a section nobody here asked for, stay out.
    screen.answer(models(None, vec![choice("claude-code/haiku", true)]));
    screen.answer(ViewData::Settings {
        rows: vec![theme_row("global")],
        command: None,
        section: Some("ui".into()),
        not_installed: Vec::new(),
    });
    assert!(screen.labels().is_empty(), "{:?}", screen.labels());
    assert_eq!(screen.title(), "Model for reviewer");

    screen.answer(models(
        Some(RoleName::Reviewer),
        vec![
            choice("codex/gpt-5.6", true),
            choice("claude-code/opus", false),
        ],
    ));
    assert_eq!(screen.labels(), ["Codex", "Claude Code"]);
    assert_eq!(screen.title(), "Model for reviewer");
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert!(picker.flow.waiting.is_none());
    assert!(picker.rows[0].current);
    assert_eq!(picker.esc_hint, "Esc to go back");

    // A role's CLIs sit under the roles.
    assert!(screen.press(KeyCode::Esc).commands.is_empty());
    assert_eq!(screen.labels(), ["reviewer"]);

    // One echo for the whole picker.
    assert_eq!(screen.transcript().matches("/model").count(), 1);
}

#[test]
fn a_section_answer_opens_its_command_picker() {
    let mut screen = Screen::new();
    screen.answer(ViewData::Settings {
        rows: vec![theme_row("global")],
        command: None,
        section: Some("ui".into()),
        not_installed: Vec::new(),
    });
    assert_eq!(screen.title(), "Theme");
    assert_eq!(screen.labels(), ["auto", "dark", "light", "Reset"]);
    assert!(screen.transcript().contains("/theme"));
}

#[test]
fn model_pick_sends_argument_form() {
    let pick = |role: Option<RoleName>| {
        let mut screen = Screen::new();
        screen.answer(models(
            role,
            vec![
                choice("claude-code/haiku", true),
                choice("codex/gpt-5.6", false),
            ],
        ));
        let opened = screen.press(KeyCode::Char('2'));
        assert!(opened.commands.is_empty(), "a CLI opens its models");
        assert_eq!(screen.labels(), ["GPT 5.6", "Other model…"]);
        let out = screen.press(KeyCode::Char('1'));
        assert!(screen.model.bottom.picker.is_none(), "a pick closes it");
        out.commands
    };
    let worker: ModelRef = "codex/gpt-5.6".parse().unwrap();
    let agent = UiCommand::Slash {
        command: SlashCommand::Model {
            role: None,
            worker: Some(worker.clone()),
            effort: None,
        },
    };
    assert_eq!(pick(None), std::slice::from_ref(&agent));
    assert_eq!(pick(Some(RoleName::Main)), [agent]);
    assert_eq!(
        pick(Some(RoleName::Reviewer)),
        [UiCommand::Slash {
            command: SlashCommand::Model {
                role: Some(RoleName::Reviewer),
                worker: Some(worker),
                effort: None,
            }
        }]
    );
}

#[test]
fn reset_row_only_for_file_and_session_layers() {
    for layer in argonauts_core::settings::LAYERS {
        let mut screen = Screen::new();
        screen.answer(settings("theme", vec![theme_row(layer)]));
        let picker = screen.model.bottom.picker.as_ref().unwrap();
        let reset = picker.rows.iter().position(|row| row.reset);
        let expected = matches!(*layer, "global" | "project" | "project-local" | "session");
        assert_eq!(reset.is_some(), expected, "{layer}");
        let Some(reset) = reset else { continue };
        let digit = char::from_digit(reset as u32 + 1, 10).unwrap();
        let out = screen.press(KeyCode::Char(digit));
        let scope = super::words::reset_scope(layer).unwrap();
        assert_eq!(
            out.commands,
            [UiCommand::ResetSetting {
                key: "ui.theme".into(),
                scope
            }],
            "{layer}"
        );
    }
}

#[test]
fn esc_on_scope_question_sends_nothing() {
    let mut screen = Screen::new();
    screen.answer(settings("theme", vec![theme_row("project")]));
    let moved = screen.press(KeyCode::Down);
    assert!(
        moved.theme.is_some(),
        "/theme shows the value under the pointer"
    );
    screen.press(KeyCode::Enter);
    assert_eq!(screen.title(), "Where to save it");
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    let pointed = picker.pointed().unwrap();
    assert_eq!(picker.rows[pointed].label, "Everywhere");
    assert!(
        picker.rows[pointed]
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("This project's file sets this")),
        "{:?}",
        picker.rows[pointed]
    );

    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty(), "{:?}", out.commands);
    assert!(out.theme.is_some(), "the palette goes back");
    assert!(screen.model.bottom.picker.is_none());
    let transcript = screen.transcript();
    assert!(transcript.contains("/theme"), "{transcript}");
    assert!(transcript.contains("⎿  Kept theme as dark"), "{transcript}");
    // The conversation's record keeps the line after the command.
    assert_eq!(
        out.replies,
        [argonauts_core::app::CommandReply {
            text: "Kept theme as".to_owned(),
            value: Some("dark".to_owned()),
            after: None,
            dim: false,
            error: false,
        }]
    );
}

#[test]
fn a_scope_number_saves_the_pick() {
    let mut screen = Screen::new();
    screen.answer(settings("theme", vec![theme_row("project")]));
    screen.press(KeyCode::Char('3'));
    assert_eq!(screen.title(), "Where to save it");
    let out = screen.press(KeyCode::Char('2'));
    assert_eq!(
        out.commands,
        [UiCommand::SetSetting {
            key: "ui.theme".into(),
            value: "\"light\"".into(),
            scope: SettingScope::ProjectLocal
        }]
    );
    assert!(screen.model.bottom.picker.is_none());
}

#[test]
fn keys_without_values_take_a_typed_line() {
    let mut screen = Screen::new();
    screen.answer(settings(
        "limits",
        vec![
            row("limits.spend_per_run", "30.0", "built-in", &[]),
            row(
                "limits.at_limit",
                "\"ask\"",
                "built-in",
                &["\"ask\"", "\"stop\""],
            ),
        ],
    ));
    assert_eq!(screen.labels(), ["spend_per_run", "at_limit"]);
    screen.press(KeyCode::Enter);
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert_eq!(picker.input.as_deref(), Some("30.0"));
    for _ in 0..4 {
        screen.press(KeyCode::Backspace);
    }
    screen.press(KeyCode::Char('4'));
    screen.press(KeyCode::Char('5'));
    screen.press(KeyCode::Enter);
    assert_eq!(screen.title(), "Where to save it");
    let out = screen.press(KeyCode::Char('3'));
    assert_eq!(
        out.commands,
        [UiCommand::SetSetting {
            key: "limits.spend_per_run".into(),
            value: "45".into(),
            scope: SettingScope::Project
        }]
    );
}

#[test]
fn effort_offers_only_the_models_levels() {
    let rows = vec![
        row(
            "roles.main.effort",
            "\"high\"",
            "built-in",
            &["\"low\"", "\"high\""],
        ),
        row("roles.reviewer.effort", "\"medium\"", "built-in", &[]),
    ];
    let mut screen = Screen::new();
    screen.answer(settings("effort", rows.clone()));
    assert_eq!(screen.labels(), ["Main agent", "reviewer"]);
    screen.press(KeyCode::Enter);
    assert_eq!(screen.labels(), ["low", "high"]);

    // Closed unchanged, it names the main agent's level, as /model names its model.
    let mut screen = Screen::new();
    screen.answer(settings("effort", rows));
    screen.press(KeyCode::Esc);
    assert!(
        screen.transcript().contains("⎿  Kept effort as high"),
        "{}",
        screen.transcript()
    );

    // A model that lists no levels takes every one.
    let mut screen = Screen::new();
    screen.answer(settings(
        "effort",
        vec![row("roles.reviewer.effort", "\"medium\"", "built-in", &[])],
    ));
    assert_eq!(screen.labels(), argonauts_core::input::EFFORT_LEVELS);
}

#[test]
fn agents_lists_each_cli_then_add_an_endpoint() {
    let mut screen = Screen::new();
    screen.answer(settings(
        "agents",
        vec![
            row(
                "agents.codex.enabled",
                "true",
                "built-in",
                &["true", "false"],
            ),
            row("agents.codex.model", "\"gpt-5.6\"", "global", &[]),
            row(
                "endpoints.local.base_url",
                "\"http://localhost:8080/v1\"",
                "global",
                &[],
            ),
        ],
    ));
    assert_eq!(
        screen.labels(),
        ["codex", "local (endpoint)", "Add an endpoint"]
    );
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert_eq!(picker.rows[0].description, "on · gpt-5.6");

    screen.press(KeyCode::Char('3'));
    for ch in "lab http://10.0.0.2:8000/v1 LAB_KEY".chars() {
        screen.press(KeyCode::Char(ch));
    }
    screen.press(KeyCode::Enter);
    let out = screen.press(KeyCode::Enter);
    assert_eq!(
        out.commands,
        [UiCommand::SetSetting {
            key: "endpoints.lab".into(),
            value: "{ base_url = \"http://10.0.0.2:8000/v1\", api_key_env = \"LAB_KEY\" }".into(),
            scope: SettingScope::Global
        }]
    );
}

/// A CLI that isn't installed says so, with how to add it, rather than reading as on.
#[test]
fn agents_says_which_clis_are_not_installed_and_how_to_add_them() {
    let enabled = |id: &str| {
        row(
            &format!("agents.{id}.enabled"),
            "true",
            "built-in",
            &["true", "false"],
        )
    };
    let rows = vec![
        enabled("codex"),
        enabled("opencode"),
        row("agents.opencode.auth", "\"subscription\"", "built-in", &[]),
        enabled("hermes"),
        row(
            "endpoints.opencode.base_url",
            "\"http://localhost:4096/v1\"",
            "global",
            &[],
        ),
    ];
    let missing = |id: &str, install: Option<&str>| MissingCli {
        id: id.into(),
        install: install.map(Into::into),
    };
    let answer = ViewData::Settings {
        rows: rows.clone(),
        command: Some("agents".into()),
        section: None,
        not_installed: vec![
            missing("opencode", Some("npm i -g opencode-ai@latest")),
            missing("hermes", None),
        ],
    };
    let mut screen = Screen::new();
    screen.answer(answer.clone());
    let notes = |screen: &Screen| -> Vec<String> {
        let picker = screen.model.bottom.picker.as_ref().unwrap();
        picker
            .rows
            .iter()
            .map(|row| row.description.clone())
            .collect()
    };
    let expected = [
        "on",
        "not installed · npm i -g opencode-ai@latest",
        "not installed",
        "http://localhost:4096/v1",
        "an OpenAI-compatible server, by its base URL",
    ];
    assert_eq!(screen.labels()[1], "opencode");
    assert_eq!(screen.labels()[3], "opencode (endpoint)");
    assert_eq!(notes(&screen), expected);

    // Fresh rows after a change keep the notes.
    screen.answer(answer);
    assert_eq!(notes(&screen), expected);
}

/// Each agent's row starts its note with its state, as the glyph every view draws for that
/// state: on, off, and not installed. An endpoint has no switch, so it carries none.
#[test]
fn agents_mark_each_row_with_its_state() {
    use crate::kit::State;

    let enabled = |id: &str, on: &str| {
        row(
            &format!("agents.{id}.enabled"),
            on,
            "built-in",
            &["true", "false"],
        )
    };
    let mut screen = Screen::new();
    screen.answer(ViewData::Settings {
        rows: vec![
            enabled("codex", "true"),
            row("agents.codex.model", "\"gpt-5.6\"", "global", &[]),
            enabled("agy", "false"),
            enabled("opencode", "true"),
            row(
                "endpoints.local.base_url",
                "\"http://localhost:8080/v1\"",
                "global",
                &[],
            ),
        ],
        command: Some("agents".into()),
        section: None,
        not_installed: vec![MissingCli {
            id: "opencode".into(),
            install: None,
        }],
    });
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    let states: Vec<Option<State>> = picker.rows.iter().map(|row| row.state).collect();
    assert_eq!(
        states,
        [
            Some(State::Done),
            Some(State::Stopped),
            Some(State::Failed),
            None,
            None
        ]
    );

    let mut theme = Theme::dark();
    theme.glyphs = crate::theme::Glyphs::UNICODE;
    let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
    let drawn = plain_text(&picker.lines(&ctx));
    for row in [
        "codex              ✔ on · gpt-5.6",
        "agy                ■ off",
        "opencode           ✗ not installed",
        "local (endpoint)   http://localhost:8080/v1",
    ] {
        assert!(drawn.contains(row), "{row:?} in\n{drawn}");
    }
}

#[test]
fn a_bad_endpoint_line_says_why_and_stays() {
    let mut screen = Screen::new();
    screen.answer(settings("agents", Vec::new()));
    screen.press(KeyCode::Enter);
    for ch in "lab localhost".chars() {
        screen.press(KeyCode::Char(ch));
    }
    let out = screen.press(KeyCode::Enter);
    assert!(out.commands.is_empty());
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert!(
        picker
            .warning
            .as_deref()
            .is_some_and(|w| w.contains("http"))
    );
    assert!(picker.input.is_some());
}

#[test]
fn a_session_value_is_named_once_over_the_scopes() {
    let mut screen = Screen::new();
    screen.answer(settings("theme", vec![theme_row("session")]));
    screen.press(KeyCode::Char('3'));
    assert_eq!(screen.title(), "Where to save it");
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert!(
        picker
            .warning
            .as_deref()
            .is_some_and(|warning| warning.starts_with("This session's value wins")),
        "{:?}",
        picker.warning
    );
    assert!(picker.rows.iter().all(|row| row.warning.is_none()));
}

fn model_update(
    role: Option<RoleName>,
    choices: Vec<ModelChoice>,
    refreshed: Option<time::OffsetDateTime>,
) -> ViewData {
    ViewData::Models {
        choices,
        role,
        refreshed,
        refreshing: false,
        update: true,
    }
}

#[test]
fn a_refreshed_list_reaches_only_the_open_model_list() {
    let mut screen = Screen::new();
    screen.answer(ViewData::Models {
        choices: vec![choice("claude-code", true), choice("codex", false)],
        role: None,
        refreshed: None,
        refreshing: true,
        update: false,
    });
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);
    let about = |screen: &Screen| screen.model.bottom.picker.as_ref().unwrap().about.clone();
    assert!(
        about(&screen).ends_with(". Checking each CLI's model list."),
        "{}",
        about(&screen)
    );
    screen.press(KeyCode::Down);

    // Another role's list doesn't land here.
    screen.answer(model_update(
        Some(RoleName::Reviewer),
        vec![choice("codex/other", false)],
        None,
    ));
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);

    let five_minutes_ago = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
    screen.answer(model_update(
        None,
        vec![
            choice("claude-code", true),
            choice("claude-code/opus", false),
            choice("codex", false),
            choice("codex/gpt-6-sol", false),
        ],
        Some(five_minutes_ago),
    ));
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert_eq!(picker.rows[picker.pointed().unwrap()].label, "Codex");
    assert_eq!(picker.rows[1].description, "1 model · subscription");
    assert!(
        about(&screen).ends_with(". Model lists refreshed 5m ago."),
        "{}",
        about(&screen)
    );

    // Once the picker is gone, a late list opens nothing.
    screen.press(KeyCode::Esc);
    screen.answer(model_update(None, vec![choice("codex", false)], None));
    assert!(screen.model.bottom.picker.is_none());
}

#[test]
fn a_models_answer_for_a_left_picker_opens_nothing_over_another() {
    let mut screen = Screen::new();
    screen.answer(settings(
        "model",
        vec![row("roles.reviewer.models", "[]", "built-in", &[])],
    ));
    screen.answer(models(
        Some(RoleName::Critic),
        vec![choice("codex/x", false)],
    ));
    assert!(
        screen.labels().is_empty(),
        "still waiting for the main agent"
    );
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    assert!(screen.transcript().contains("Kept model as it was"));

    // A role's list that comes after Esc went back to the roles opens nothing over them.
    let mut screen = Screen::new();
    screen.answer(settings(
        "model",
        vec![row("roles.reviewer.models", "[]", "built-in", &[])],
    ));
    screen.answer(models(None, vec![choice("claude-code", true)]));
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Enter);
    assert_eq!(screen.title(), "Roles");
    screen.press(KeyCode::Enter);
    assert!(screen.labels().is_empty(), "still waiting for the reviewer");
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    assert_eq!(screen.title(), "Roles", "Esc goes back to the roles");
    screen.answer(models(
        Some(RoleName::Reviewer),
        vec![choice("codex/x", false)],
    ));
    assert_eq!(screen.title(), "Roles", "the answer came for a level left");
    assert_eq!(screen.labels(), ["reviewer"]);
}

#[test]
fn esc_on_the_model_list_keeps_the_main_agent() {
    let mut screen = Screen::new();
    screen.answer(settings(
        "model",
        vec![row("crew.main", "\"claude-code/haiku\"", "global", &[])],
    ));
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    let transcript = screen.transcript();
    assert!(
        transcript.contains("⎿  Kept model as Haiku"),
        "{transcript}"
    );
}

#[test]
fn the_model_list_moves_with_vim_keys_only_when_they_are_on() {
    let open = |vim: bool| {
        let mut screen = Screen::new();
        if vim {
            screen.model.bottom.mode.vim = Some(crate::bottom::VimMode::Insert);
        }
        screen.answer(models(
            None,
            vec![
                choice("claude-code/haiku", true),
                choice("codex/gpt-5.6", false),
            ],
        ));
        screen
    };

    let mut screen = open(true);
    screen.press(KeyCode::Char('j'));
    screen.press(KeyCode::Char('l'));
    let out = screen.press(KeyCode::Char('l'));
    assert!(
        screen.model.bottom.picker.is_none(),
        "l picks, as Enter does"
    );
    let worker: ModelRef = "codex/gpt-5.6".parse().unwrap();
    assert_eq!(
        out.commands,
        [UiCommand::Slash {
            command: SlashCommand::Model {
                role: None,
                worker: Some(worker),
                effort: None,
            },
        }]
    );

    let mut screen = open(true);
    let out = screen.press(KeyCode::Char('h'));
    assert!(out.commands.is_empty());
    assert!(
        screen.model.bottom.picker.is_none(),
        "h closes, as Esc does"
    );
    assert!(screen.transcript().contains("Kept model as"));

    // A level down, h goes back up, as Esc does.
    let mut screen = open(true);
    screen.press(KeyCode::Char('j'));
    screen.press(KeyCode::Char('l'));
    assert_eq!(screen.labels(), ["GPT 5.6", "Other model…"]);
    screen.press(KeyCode::Char('h'));
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);

    // Without vim keys the same letters filter.
    let mut screen = open(false);
    screen.press(KeyCode::Char('d'));
    screen.press(KeyCode::Char('e'));
    screen.press(KeyCode::Char('x'));
    let picker = screen.model.bottom.picker.as_ref().expect("still open");
    assert_eq!(picker.filter, "dex");
    assert_eq!(picker.shown(), [1]);
}

fn noted(worker: &str, note: Option<&str>, older: bool) -> ModelChoice {
    ModelChoice {
        note: note.map(str::to_owned),
        older,
        ..choice(worker, false)
    }
}

/// A model the catalog has no words for, such as Mythos 5.1, reads as what it is, not its id.
#[test]
fn a_model_without_a_description_says_what_kind_it_is() {
    use super::models::model_description;

    let mythos = ModelChoice {
        provider: "anthropic".into(),
        name: "Mythos 5.1".into(),
        ..choice("claude-code/claude-mythos-5-1", false)
    };
    assert_eq!(
        model_description(&mythos),
        "An Anthropic model on your subscription"
    );
    let local = ModelChoice {
        provider: "olmo".into(),
        access: ModelAccess::Local,
        ..choice("ollama/olmo2", false)
    };
    assert_eq!(model_description(&local), "An Olmo model on this machine");
    let unknown = ModelChoice {
        provider: "unknown".into(),
        access: ModelAccess::ApiKey,
        ..choice("acme/x-1", false)
    };
    assert_eq!(model_description(&unknown), "A model on your API key");
    let described = ModelChoice {
        description: Some("Efficient for routine tasks".into()),
        ..mythos
    };
    assert_eq!(model_description(&described), "Efficient for routine tasks");
}

fn levels(names: &[&str]) -> Vec<String> {
    names.iter().map(|&name| name.to_owned()).collect()
}

/// Two CLIs as the controller sends them: each one's default, aliases, models, then older ones.
#[test]
fn kept_lines_name_models_as_people_do() {
    use super::models::worker_words;

    let default = ModelChoice {
        runs: Some("Opus 5.5".into()),
        ..choice("claude-code", false)
    };
    let alias = ModelChoice {
        alias: true,
        runs: Some("Opus 5.5".into()),
        ..choice("claude-code/opus", false)
    };
    let named = ModelChoice {
        name: "GPT-6 Sol".into(),
        ..choice("codex/gpt-6-sol", false)
    };
    let choices = [default, alias, named];
    assert_eq!(
        worker_words("claude-code", &choices),
        "Claude Code's default (Opus 5.5)"
    );
    assert_eq!(
        worker_words("claude-code/opus", &choices),
        "Opus (Opus 5.5)"
    );
    assert_eq!(worker_words("codex/gpt-6-sol", &choices), "GPT-6 Sol");
    assert_eq!(
        worker_words("claude-code/claude-opus-5-5", &choices),
        "Opus 5.5"
    );
    assert_eq!(worker_words("codex", &[]), "Codex's default");
}

#[test]
fn esc_on_the_cli_list_keeps_the_default_with_what_it_runs() {
    let mut screen = Screen::new();
    let mut choices = claude_and_codex();
    for choice in &mut choices {
        choice.current = choice.worker.model.is_none() && choice.worker.agent == "claude-code";
        if choice.current {
            choice.runs = Some("Opus 5.5".into());
        }
    }
    screen.answer(models(None, choices));
    screen.press(KeyCode::Esc);
    let transcript = screen.transcript();
    assert!(
        transcript.contains("⎿  Kept model as Claude Code's default (Opus 5.5)"),
        "{transcript}"
    );
}

fn claude_and_codex() -> Vec<ModelChoice> {
    let claude = levels(&["low", "medium", "high", "xhigh", "max"]);
    vec![
        ModelChoice {
            recommended: true,
            runs: Some("Opus 5.5".into()),
            description: Some("Best for everyday, complex tasks".into()),
            efforts: claude.clone(),
            ..noted("claude-code", None, false)
        },
        ModelChoice {
            current: true,
            alias: true,
            runs: Some("Opus 5.5".into()),
            efforts: claude.clone(),
            ..noted("claude-code/opus", Some("newest Opus"), false)
        },
        ModelChoice {
            alias: true,
            runs: Some("Haiku 4.5".into()),
            ..noted("claude-code/haiku", Some("newest Haiku"), false)
        },
        ModelChoice {
            name: "Opus 5.5".into(),
            description: Some("For complex work and everyday tasks".into()),
            efforts: claude.clone(),
            default_effort: Some("xhigh".into()),
            ..noted("claude-code/claude-opus-5-5", None, false)
        },
        ModelChoice {
            description: Some("Best for everyday, complex tasks".into()),
            efforts: claude,
            default_effort: Some("high".into()),
            ..noted("claude-code/claude-opus-4-8", None, true)
        },
        noted("claude-code/claude-3-haiku-20240307", Some("retired"), true),
        noted("codex", Some("runs gpt-6-sol"), false),
        noted("codex/gpt-6-sol", None, false),
    ]
}

fn pointed_label(screen: &Screen) -> String {
    let picker = screen
        .model
        .bottom
        .picker
        .as_ref()
        .expect("a picker is open");
    picker.rows[picker.pointed().expect("a pointer")]
        .label
        .clone()
}

#[test]
fn model_lists_the_clis_then_enter_opens_one() {
    let mut screen = Screen::new();
    screen.answer(models(None, claude_and_codex()));
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert!(picker.rows[0].current);
    assert_eq!(picker.rows[0].description, "opus · subscription");
    assert_eq!(picker.rows[1].description, "1 model · subscription");
    assert_eq!(pointed_label(&screen), "Claude Code");

    let out = screen.press(KeyCode::Enter);
    assert!(
        out.commands.is_empty(),
        "nothing is asked for a CLI's models"
    );
    assert_eq!(screen.title(), "Claude Code");
    assert_eq!(
        screen.labels(),
        [
            "Default (recommended)",
            "Opus 5.5",
            "Opus 4.8",
            "Claude 3 Haiku",
            "Other model…",
        ]
    );
    assert_eq!(
        pointed_label(&screen),
        "Default (recommended)",
        "the alias in use has no row of its own"
    );
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    let notes: Vec<&str> = picker.rows.iter().map(|r| r.description.as_str()).collect();
    assert_eq!(
        notes,
        [
            "Opus 5.5 · Best for everyday, complex tasks",
            "For complex work and everyday tasks",
            "Best for everyday, complex tasks",
            "retired",
            "type any model name it takes",
        ]
    );
    let under: Vec<Option<&str>> = picker.rows.iter().map(|r| r.under.as_deref()).collect();
    assert_eq!(
        under,
        [
            Some("Aliases: opus (in use), haiku"),
            None,
            None,
            None,
            None
        ],
        "the aliases are one line under Default"
    );
    let headings: Vec<Option<&str>> = picker.rows.iter().map(|r| r.heading.as_deref()).collect();
    assert_eq!(
        headings, [None; 5],
        "older models follow with no heading, as Claude Code lists them"
    );

    // A model nothing describes says what kind it is, so its row doesn't read as blank.
    let mut choices = claude_and_codex();
    choices.push(ModelChoice {
        provider: "anthropic".into(),
        ..choice("claude-code/claude-mythos-5-1", false)
    });
    let mut screen = Screen::new();
    screen.answer(models(None, choices));
    screen.press(KeyCode::Enter);
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    let mythos = picker
        .rows
        .iter()
        .find(|row| row.label == "Mythos 5.1")
        .unwrap_or_else(|| panic!("{:?}", screen.labels()));
    assert_eq!(
        mythos.description,
        "An Anthropic model on your subscription"
    );
    assert_eq!(picker.esc_hint, "Esc to go back");

    // Esc goes back to the CLIs, on the one it left; the next opens on its default.
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);
    assert_eq!(pointed_label(&screen), "Claude Code");
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Enter);
    assert_eq!(screen.labels(), ["Default", "GPT 6 Sol", "Other model…"]);
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert_eq!(picker.rows[0].description, "runs gpt-6-sol");
    let out = screen.press(KeyCode::Enter);
    assert_eq!(
        out.commands,
        [UiCommand::Slash {
            command: SlashCommand::Model {
                role: None,
                worker: Some("codex".parse().unwrap()),
                effort: None,
            },
        }]
    );
}

#[test]
fn other_model_takes_any_name_the_cli_runs() {
    let mut screen = Screen::new();
    screen.answer(models(Some(RoleName::Reviewer), claude_and_codex()));
    screen.press(KeyCode::Enter);
    screen.press(KeyCode::End);
    screen.press(KeyCode::Enter);
    assert_eq!(screen.title(), "Other model on Claude Code");
    let typing = |screen: &Screen| screen.model.bottom.picker.as_ref().unwrap().input.clone();
    assert_eq!(typing(&screen), Some(String::new()));

    let out = screen.press(KeyCode::Enter);
    assert!(out.commands.is_empty());
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert!(picker.warning.is_some(), "an empty name says what to do");

    // Esc goes back to the list, on "Other model…".
    screen.press(KeyCode::Esc);
    assert_eq!(screen.title(), "Claude Code");
    assert_eq!(pointed_label(&screen), "Other model…");

    screen.press(KeyCode::Enter);
    for ch in "claude-opus-4-1".chars() {
        screen.press(KeyCode::Char(ch));
    }
    let out = screen.press(KeyCode::Enter);
    assert!(screen.model.bottom.picker.is_none());
    assert_eq!(
        out.commands,
        [UiCommand::Slash {
            command: SlashCommand::Model {
                role: Some(RoleName::Reviewer),
                worker: Some("claude-code/claude-opus-4-1".parse().unwrap()),
                effort: None,
            },
        }]
    );
}

#[test]
fn esc_through_both_levels_keeps_the_model() {
    let mut screen = Screen::new();
    screen.answer(models(None, claude_and_codex()));
    screen.press(KeyCode::Enter);
    screen.press(KeyCode::Esc);
    assert!(
        screen.model.bottom.picker.is_some(),
        "the first Esc goes back"
    );
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    assert!(screen.model.bottom.picker.is_none());
    let transcript = screen.transcript();
    assert!(transcript.contains("⎿  Kept model as Opus"), "{transcript}");
}

#[test]
fn a_refresh_reaches_the_open_cli_without_moving_the_pointer() {
    let mut screen = Screen::new();
    screen.answer(models(None, claude_and_codex()));
    screen.press(KeyCode::Enter);
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Down);
    assert_eq!(pointed_label(&screen), "Opus 4.8");

    let mut newer = claude_and_codex();
    newer.insert(3, noted("claude-code/claude-sonnet-5-5", None, false));
    let just_now = time::OffsetDateTime::now_utc();
    screen.answer(model_update(None, newer, Some(just_now)));
    assert_eq!(screen.title(), "Claude Code");
    assert!(screen.labels().contains(&"Sonnet 5.5".to_owned()));
    assert_eq!(pointed_label(&screen), "Opus 4.8");
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    assert_eq!(
        picker.flow.effort.as_ref().and_then(|line| line.level()),
        Some("high"),
        "the effort line stays on the same model"
    );
    let about = screen.model.bottom.picker.as_ref().unwrap().about.clone();
    assert!(
        about.ends_with("Model lists refreshed just now."),
        "{about}"
    );

    // Going back shows the newer list's CLIs too.
    screen.press(KeyCode::Esc);
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);
}

#[test]
fn verbosity_is_one_picker_of_its_levels() {
    let mut screen = Screen::new();
    let levels = ["\"full\"", "\"normal\"", "\"quiet\""];
    screen.answer(settings(
        "verbosity",
        vec![row("ui.verbosity", "\"normal\"", "built-in", &levels)],
    ));
    assert_eq!(screen.title(), "Verbosity");
    let picker = screen.model.bottom.picker.as_ref().unwrap();
    let labels: Vec<&str> = picker
        .rows
        .iter()
        .filter(|row| !row.reset)
        .map(|row| row.label.as_str())
        .collect();
    for level in ["full", "normal", "quiet"] {
        assert!(
            labels.iter().any(|label| label.contains(level)),
            "{level} in {labels:?}"
        );
    }
}

mod effort;
