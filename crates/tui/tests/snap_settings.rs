mod common;

use argonauts_core::app::{ModelChoice, SettingRow, UiCommand, ViewRequest};
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::limits::{CostEstimate, PlanUsageReading, PlanUsageSource, PlanUsageStatus};
use argonauts_core::model_picker::{EffortSlider, EffortStop};
use argonauts_core::names::{AgentId, ApprovalId, RoleName};
use argonauts_core::policy::{ApprovalRow, ApprovalsView as ApprovalsData};
use argonauts_core::registry::ModelAccess;
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::{KeyScheme, RunId};
use argonauts_tui::bottom::{Picker, PickerEvent};
use argonauts_tui::stability::{WIDTHS, assert_frames, assert_lines, buffer_text};
use argonauts_tui::state::{AppState, RunState};
use argonauts_tui::theme::Role;
use argonauts_tui::views::settings::approvals::ApprovalsView;
use argonauts_tui::views::settings::config::ConfigView;
use argonauts_tui::views::settings::flow::{self, Kept, Step};
use argonauts_tui::views::{View, ViewCtx, ViewOutcome};
use common::{fg, still, style_of, t0, text, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use time::OffsetDateTime;

const SIZES: [(u16, u16); 2] = [(80, 24), (120, 32)];

fn row(key: &str, value: &str, layer: &str, allowed: &[&str], description: &str) -> SettingRow {
    SettingRow {
        key: key.to_owned(),
        value: value.to_owned(),
        layer: layer.to_owned(),
        workflow_from: None,
        default_scope: SettingScope::Project,
        safety: key.starts_with("approvals.") || key.starts_with("limits."),
        allowed: allowed.iter().map(|value| (*value).to_owned()).collect(),
        description: description.to_owned(),
    }
}

fn picker_lines(picker: &Picker, width: u16) -> String {
    let theme = theme();
    text(&picker.lines(&still(&theme, width)))
}

/// Snapshots at 80 and 120 columns, and the frame check at every width.
fn check_picker(name: &str, picker: &Picker) {
    let theme = theme();
    for width in WIDTHS {
        assert_lines(&picker.lines(&still(&theme, width)), width);
    }
    for width in [80, 120] {
        insta::assert_snapshot!(format!("{name}_{width}"), picker_lines(picker, width));
    }
}

fn press(picker: &mut Picker, code: KeyCode) -> Step {
    let event = picker.handle_key(KeyEvent::from(code));
    assert_ne!(event, PickerEvent::Unhandled);
    flow::advance(picker, event)
}

fn workflows() -> Picker {
    let names = [
        "council",
        "orchestrator",
        "captain",
        "maker-checker",
        "best-of-n",
        "draft-and-vote",
        "ablate-read",
        "ablate-map",
        "ablate-critic",
        "ablate-run_review",
    ];
    let allowed: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
    let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
    flow::settings_picker(
        "workflow",
        vec![row(
            "workflow",
            "\"captain\"",
            "project",
            &allowed,
            "How the crew works together",
        )],
    )
}

#[test]
fn settings_picker_frame() {
    let picker = workflows();
    check_picker("settings_picker_frame", &picker);

    let shown = picker_lines(&picker, 80);
    assert!(shown.contains("❯ 3. captain ✓"), "{shown}");
    assert!(shown.contains("↓ 8. ablate-map"), "{shown}");
    assert!(shown.contains("… +3 more"), "{shown}");
    assert!(!shown.contains("Reset"), "Reset is past the fold:\n{shown}");

    let theme = theme();
    let lines = picker.lines(&still(&theme, 80));
    assert_eq!(style_of(&lines, "✓").unwrap().fg, fg(Role::Success));
    assert_eq!(style_of(&lines, "───").unwrap().fg, fg(Role::Question));
    assert_eq!(style_of(&lines, "Workflow").unwrap().fg, fg(Role::Question));
    assert_eq!(
        style_of(&lines, "The CLI you talk to does the work itself")
            .unwrap()
            .fg,
        fg(Role::Dim)
    );

    // The last rows scroll in, with Reset for the project file's value.
    let mut picker = picker;
    press(&mut picker, KeyCode::End);
    let end = picker_lines(&picker, 80);
    assert!(end.contains("❯ 11. Reset"), "{end}");
    assert!(end.contains("remove it from this project's file"), "{end}");
    assert!(!end.contains("more"), "{end}");
}

#[test]
fn scope_question() {
    let mut picker = flow::settings_picker(
        "theme",
        vec![SettingRow {
            default_scope: SettingScope::Global,
            ..row(
                "ui.theme",
                "\"dark\"",
                "project",
                &["\"auto\"", "\"dark\"", "\"light\""],
                "Light or dark colors, or follow the terminal",
            )
        }],
    );
    assert_eq!(press(&mut picker, KeyCode::Char('3')), Step::Stay);
    check_picker("scope_question", &picker);

    let shown = picker_lines(&picker, 80);
    let order: Vec<usize> = [
        "This project's file sets this",
        "Just this session",
        "This project, just me",
        "This project, shared",
        "❯ 4. Everywhere",
    ]
    .iter()
    .map(|needle| {
        shown
            .find(needle)
            .unwrap_or_else(|| panic!("{needle}:\n{shown}"))
    })
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{shown}");
    for (n, label) in ["session", "just me", "shared", "Everywhere"]
        .into_iter()
        .enumerate()
    {
        let line = shown.lines().find(|line| line.contains(label)).unwrap();
        assert!(line.contains(&format!("{}. ", n + 1)), "{line:?}");
    }

    // Saving just for you in this project isn't hidden by the shared file, so no warning.
    press(&mut picker, KeyCode::Up);
    press(&mut picker, KeyCode::Up);
    let local = picker_lines(&picker, 80);
    assert!(!local.contains("won't change"), "{local}");

    assert_eq!(
        press(&mut picker, KeyCode::Esc),
        Step::Cancel {
            kept: Kept::as_value("theme", "dark")
        }
    );
}

fn choice(
    worker: &str,
    access: ModelAccess,
    cost: CostEstimate,
    strengths: &[&str],
) -> ModelChoice {
    ModelChoice {
        worker: worker.parse().unwrap(),
        provider: String::new(),
        access,
        cost,
        plan_usage: None,
        efforts: Vec::new(),
        strengths: strengths.iter().map(|s| (*s).to_owned()).collect(),
        current: false,
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

fn claude(model: Option<&str>, note: Option<&str>, older: bool) -> ModelChoice {
    let worker = match model {
        Some(model) => format!("claude-code/{model}"),
        None => "claude-code".to_owned(),
    };
    let mut choice = choice(
        &worker,
        ModelAccess::Subscription,
        CostEstimate::PlanUsage(0.04),
        &[],
    );
    choice.note = note.map(str::to_owned);
    choice.older = older;
    choice.plan_usage = Some(PlanUsageReading {
        agent_id: AgentId("claude-code".into()),
        status: PlanUsageStatus::Allowed,
        used: Some(0.42),
        resets_at: None,
        window: Some("five_hour".into()),
        source: PlanUsageSource::RateLimitEvent,
        observed_at: OffsetDateTime::UNIX_EPOCH,
        detail: None,
        wrap_up: false,
    });
    choice
}

/// The reviewer's choices as the controller sends them: Claude Code's default, aliases,
/// models, and older ones, then Codex and a local endpoint.
fn reviewer_choices() -> Vec<ModelChoice> {
    let levels: Vec<String> = ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .map(|&level| level.to_owned())
        .collect();
    let named = |model: &str, name: &str, description: &str, default: &str, older: bool| {
        let mut choice = claude(Some(model), None, older);
        choice.name = name.to_owned();
        choice.description = Some(description.to_owned());
        choice.efforts = levels.clone();
        choice.default_effort = Some(default.to_owned());
        choice
    };
    let alias = |model: &str, runs: &str, current: bool| {
        let mut choice = claude(Some(model), None, false);
        choice.alias = true;
        choice.runs = Some(runs.to_owned());
        choice.current = current;
        choice
    };
    let mut default = claude(None, None, false);
    default.recommended = true;
    default.runs = Some("Opus 5.5".into());
    default.description = Some("Best for everyday, complex tasks".into());
    default.efforts = levels.clone();
    vec![
        default,
        alias("opus", "Opus 5.5", true),
        alias("sonnet", "Sonnet 5.5", false),
        named(
            "claude-opus-5-5",
            "Opus 5.5",
            "For complex work and everyday tasks",
            "xhigh",
            false,
        ),
        named(
            "claude-sonnet-5-5",
            "Sonnet 5.5",
            "Most efficient for simpler tasks",
            "high",
            false,
        ),
        named(
            "claude-opus-4-8",
            "Opus 4.8",
            "Best for everyday, complex tasks",
            "high",
            true,
        ),
        claude(Some("claude-3-7-sonnet-20250219"), Some("retired"), true),
        choice(
            "codex/gpt-5.6",
            ModelAccess::ApiKey,
            CostEstimate::Usd(0.38),
            &["refactors", "tests"],
        ),
        choice(
            "ollama/llama3.2",
            ModelAccess::Local,
            CostEstimate::Free,
            &[],
        ),
    ]
}

/// `/model` opens on the main agent's CLIs, with "Roles…" a row away, and each role's
/// workers named by their CLI.
#[test]
fn model_picker_opens_on_the_main_agent() {
    let rows = vec![
        row(
            "crew.main",
            "\"claude-code\"",
            "global",
            &[],
            "Who answers in conversation",
        ),
        row(
            "narrator.model",
            "\"claude-code/haiku\"",
            "built-in",
            &[],
            "The narrator's model",
        ),
        row(
            "roles.reviewer.models",
            "[\"codex/gpt-5.6\", \"claude-code/opus\"]",
            "built-in",
            &[],
            "Who reviews",
        ),
    ];
    let mut picker = flow::settings_picker("model", rows);
    assert_eq!(
        flow::asking(&picker),
        Some(ViewRequest::Models { role: None })
    );
    flow::fill_models(&mut picker, None, &reviewer_choices());
    check_picker("model_picker_main_agent", &picker);
    let shown = picker_lines(&picker, 80);
    assert!(shown.contains("Roles…"), "{shown}");

    press(&mut picker, KeyCode::End);
    assert_eq!(press(&mut picker, KeyCode::Enter), Step::Stay);
    check_picker("model_picker_roles", &picker);
    let shown = picker_lines(&picker, 80);
    assert!(shown.contains("Haiku on Claude Code"), "{shown}");
    assert!(
        shown.contains("GPT 5.6 on Codex, Opus on Claude Code"),
        "{shown}"
    );
}

#[test]
fn models_picker() {
    let mut picker = flow::models_picker(
        "model reviewer",
        Some(RoleName::Reviewer),
        &reviewer_choices(),
    );
    check_picker("models_picker", &picker);

    let shown = picker_lines(&picker, 120);
    assert!(shown.contains("❯ 1. Claude Code ✓"), "{shown}");
    assert!(shown.contains("opus · subscription · 42% used"), "{shown}");
    assert!(shown.contains("2. Codex"), "{shown}");
    assert!(shown.contains("1 model · API key"), "{shown}");
    assert!(shown.contains("3. Ollama"), "{shown}");
    assert!(shown.contains("Enter to see its models"), "{shown}");

    assert_eq!(press(&mut picker, KeyCode::Enter), Step::Stay);
    assert_eq!(press(&mut picker, KeyCode::Down), Step::Stay);
    check_picker("model_list_picker", &picker);
    let shown = picker_lines(&picker, 120);
    assert!(
        shown.contains("1. Default (recommended)   Opus 5.5 · Best for everyday, complex tasks"),
        "{shown}"
    );
    assert!(shown.contains("Aliases: opus (in use), sonnet"), "{shown}");
    assert!(
        shown.contains("❯ 2. Opus 5.5                For complex work and everyday tasks"),
        "{shown}"
    );
    assert!(
        shown.contains("◉ xhigh effort (default)  ←/→ to adjust"),
        "{shown}"
    );
    assert!(shown.contains("6. Other model…"), "{shown}");
    assert!(shown.contains("Esc to go back"), "{shown}");
    // Older models follow the newer ones with no heading row, as Claude Code lists them.
    assert!(!shown.lines().any(|line| line.trim() == "Older"), "{shown}");
    let newer = shown.lines().position(|line| line.contains("Sonnet 5.5"));
    let older = shown.lines().position(|line| line.contains("Opus 4.8"));
    assert_eq!(newer.map(|at| at + 1), older, "{shown}");

    // → raises the effort, and Enter saves it with the model.
    assert_eq!(press(&mut picker, KeyCode::Right), Step::Stay);
    assert!(picker_lines(&picker, 80).contains("◉ max effort (default xhigh)"));
    let Step::Done(UiCommand::Slash { command }) = press(&mut picker, KeyCode::Enter) else {
        panic!("a model pick sends the argument form");
    };
    assert_eq!(
        command,
        argonauts_core::input::SlashCommand::Model {
            role: Some(RoleName::Reviewer),
            worker: Some("claude-code/claude-opus-5-5".parse().unwrap()),
            effort: Some("max".into()),
        }
    );

    // Back up, then into Codex, whose model is picked by its number.
    let mut picker = flow::models_picker(
        "model reviewer",
        Some(RoleName::Reviewer),
        &reviewer_choices(),
    );
    press(&mut picker, KeyCode::Enter);
    assert_eq!(press(&mut picker, KeyCode::Esc), Step::Stay);
    assert_eq!(press(&mut picker, KeyCode::Char('2')), Step::Stay);
    let Step::Done(UiCommand::Slash { command }) = press(&mut picker, KeyCode::Char('1')) else {
        panic!("a model pick sends the argument form");
    };
    assert_eq!(
        command,
        argonauts_core::input::SlashCommand::Model {
            role: Some(RoleName::Reviewer),
            worker: Some("codex/gpt-5.6".parse().unwrap()),
            effort: None,
        }
    );
}

fn slider(chosen: &str) -> EffortSlider {
    let stops = ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .map(|&level| EffortStop {
            level: level.into(),
            description: argonauts_core::model_catalog::builtin_effort_note(level)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect();
    EffortSlider::new(
        None,
        "claude-code/claude-opus-5-5".parse().unwrap(),
        "Opus 5.5",
        stops,
        Some("xhigh"),
        Some(chosen),
    )
    .unwrap()
}

/// The slider for a model whose default is `default`, with no level set.
fn unset_slider(worker: &str, model: &str, default: &str) -> EffortSlider {
    let stops = ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .map(|&level| EffortStop {
            level: level.into(),
            description: argonauts_core::model_catalog::builtin_effort_note(level)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect();
    EffortSlider::new(
        None,
        worker.parse().unwrap(),
        model,
        stops,
        Some(default),
        None,
    )
    .unwrap()
}

/// With no effort set, `/effort` opens on the level the model runs at, drawn just as that
/// level picked would be, and marked as the model's default.
#[test]
fn with_no_effort_set_the_slider_starts_on_the_models_default() {
    let cases = [
        (
            "effort_slider_unset_xhigh",
            unset_slider("claude-code", "Opus 5.5", "xhigh"),
            "Extra depth for hard, many-step problems. The model's default",
        ),
        (
            "effort_slider_unset_high",
            unset_slider("claude-code/claude-sonnet-5-5", "Sonnet 5.5", "high"),
            "Thinks longer for complex problems. The model's default",
        ),
    ];
    let theme = theme();
    for (name, unset, says) in cases {
        let level = unset.level().expect("the model has levels").to_owned();
        let mut picked = unset.clone();
        picked.chosen = picked.default.expect("the model has a default");
        let unset = flow::effort_picker("effort", unset);
        check_picker(name, &unset);
        let shown = picker_lines(&unset, 80);
        assert!(shown.contains(says), "{shown}");
        assert!(!shown.contains("The CLI's own effort"), "{shown}");
        let picked = flow::effort_picker("effort", picked);
        for width in [80, 120] {
            let ctx = still(&theme, width);
            assert_eq!(
                common::styled(&unset.lines(&ctx)),
                common::styled(&picked.lines(&ctx)),
                "{level} unset reads as {level} picked"
            );
        }
        let lines = unset.lines(&still(&theme, 80));
        assert_eq!(style_of(&lines, "▲"), Some(theme.style(Role::Question)));
        let label = style_of(&lines, &format!(" {level} ")).or_else(|| style_of(&lines, &level));
        assert_eq!(
            label.and_then(|style| style.fg),
            theme.style(Role::Question).fg,
            "{level} in the accent color"
        );
    }
}

/// `/model`'s effort line on Claude Code's Default, with no effort set, names the level the
/// model it runs starts at.
#[test]
fn with_no_effort_set_the_models_effort_line_names_the_default() {
    for (runs, default) in [("Opus 5.5", "xhigh"), ("Sonnet 5.5", "high")] {
        let mut choices = reviewer_choices();
        choices[0].runs = Some(runs.into());
        choices[0].default_effort = Some(default.into());
        choices[0].current = true;
        choices[1].current = false;
        let mut picker = flow::models_picker("model", None, &choices);
        assert_eq!(press(&mut picker, KeyCode::Enter), Step::Stay);
        let name = format!("model_list_unset_{default}");
        check_picker(&name, &picker);
        let shown = picker_lines(&picker, 80);
        assert!(
            shown.contains(&format!("◉ {default} effort (default)  ←/→ to adjust")),
            "{shown}"
        );
        assert!(!shown.contains("the CLI's own effort"), "{shown}");
    }
}

#[test]
fn effort_slider() {
    let mut picker = flow::effort_picker("effort", slider("xhigh"));
    check_picker("effort_slider", &picker);
    let shown = picker_lines(&picker, 80);
    assert!(shown.contains("Extra depth for hard, many-step problems. The model's default"));
    assert!(shown.contains("←/→ to adjust · Enter to confirm · Esc to cancel"));

    let event = picker.handle_key(KeyEvent::from(KeyCode::Right));
    assert_eq!(flow::advance(&mut picker, event), Step::Stay);
    check_picker("effort_slider_max", &picker);
    let event = picker.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        flow::advance(&mut picker, event),
        Step::Done(UiCommand::Slash {
            command: argonauts_core::input::SlashCommand::Effort {
                role: None,
                level: Some("max".into()),
                ultracode: None,
            }
        })
    );

    // The chosen level is drawn in the question color, and the others dim.
    let theme = theme();
    let picker = flow::effort_picker("effort", slider("low"));
    let lines = picker.lines(&still(&theme, 80));
    let levels = lines
        .iter()
        .find(|line| text(std::slice::from_ref(line)).contains("medium"))
        .unwrap();
    let low = levels
        .spans
        .iter()
        .find(|span| span.content == "low")
        .unwrap();
    let max = levels
        .spans
        .iter()
        .find(|span| span.content == "max")
        .unwrap();
    assert_eq!(low.style.fg, theme.style(Role::Question).fg);
    assert_eq!(max.style.fg, theme.style(Role::Dim).fg);
}

/// Ultracode beside the slider, off and then on, as Claude Code places Ultracode: its name and
/// state level with the line, `Tab to toggle` under them, and while it's on a line saying what
/// it does. Tab flips it, and Enter saves it without pinning the level it left alone.
#[test]
fn effort_slider_ultracode() {
    let theme = theme();
    let mut picker = flow::effort_picker("effort", slider("xhigh").with_ultracode(false));
    for (state, on) in [("off", false), ("on", true)] {
        if on {
            let event = picker.handle_key(KeyEvent::from(KeyCode::Tab));
            assert_eq!(event, PickerEvent::Toggled);
            assert_eq!(flow::advance(&mut picker, event), Step::Stay);
        }
        for width in WIDTHS {
            assert_lines(&picker.lines(&still(&theme, width)), width);
        }
        for width in [80, 100, 120, 200] {
            let shown = picker_lines(&picker, width);
            insta::assert_snapshot!(format!("effort_slider_ultracode_{state}_{width}"), shown);
            let lines: Vec<&str> = shown.lines().collect();
            let rule = lines.iter().position(|line| line.contains('▲')).unwrap();
            assert!(
                lines[rule].ends_with(&format!("Ultracode  {state}")),
                "{shown}"
            );
            assert!(lines[rule + 1].ends_with("Tab to toggle"), "{shown}");
            let column = |line: &str, part: &str| line[..line.find(part).unwrap()].chars().count();
            assert_eq!(
                column(lines[rule], "Ultracode"),
                column(lines[rule + 1], "Tab to toggle"),
                "{shown}"
            );
            let means = "Ultracode: a workflow for every real request";
            assert_eq!(shown.contains(means), on, "{shown}");
        }
    }
    let lines = picker.lines(&still(&theme, 80));
    assert_eq!(
        style_of(&lines, "Ultracode").and_then(|style| style.fg),
        fg(Role::Label)
    );

    // Too narrow to sit beside the slider, it takes a row of its own.
    let narrow = picker_lines(&picker, 50);
    assert!(
        narrow.contains("    Ultracode  on · Tab to toggle"),
        "{narrow}"
    );

    let event = picker.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        flow::advance(&mut picker, event),
        Step::Done(UiCommand::Slash {
            command: argonauts_core::input::SlashCommand::Effort {
                role: None,
                level: None,
                ultracode: Some(true),
            }
        })
    );

    // Moved too, the level goes with it; flipped back, only the level does.
    let mut picker = flow::effort_picker("effort", slider("xhigh").with_ultracode(true));
    for code in [KeyCode::Tab, KeyCode::Left] {
        let event = picker.handle_key(KeyEvent::from(code));
        flow::advance(&mut picker, event);
    }
    assert!(!picker_lines(&picker, 80).contains("Ultracode:"));
    let event = picker.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        flow::advance(&mut picker, event),
        Step::Done(UiCommand::Slash {
            command: argonauts_core::input::SlashCommand::Effort {
                role: None,
                level: Some("high".into()),
                ultracode: Some(false),
            }
        })
    );
    let mut picker = flow::effort_picker("effort", slider("xhigh").with_ultracode(true));
    for _ in 0..2 {
        let event = picker.handle_key(KeyEvent::from(KeyCode::Tab));
        flow::advance(&mut picker, event);
    }
    let event = picker.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        flow::advance(&mut picker, event),
        Step::Done(UiCommand::Slash {
            command: argonauts_core::input::SlashCommand::Effort {
                role: None,
                level: Some("xhigh".into()),
                ultracode: None,
            }
        })
    );
}

/// With nothing to slide, Ultracode takes a row of its own under what the model runs at: a
/// level fixed by the model, or none at all.
#[test]
fn effort_ultracode_without_a_slider() {
    let fixed = EffortSlider::new(
        None,
        "antigravity-cli/gemini-3.8-flash-high".parse().unwrap(),
        "Gemini 3.8 Flash (High)",
        vec![EffortStop {
            level: "high".into(),
            description: String::new(),
        }],
        None,
        None,
    )
    .unwrap();
    let none =
        EffortSlider::without_levels(None, "claude-code/haiku".parse().unwrap(), "Haiku 4.5");
    for (name, slider) in [("fixed", fixed), ("none", none)] {
        for (state, on) in [("off", false), ("on", true)] {
            let picker = flow::effort_picker("effort", slider.clone().with_ultracode(on));
            let shown = picker_lines(&picker, 100);
            insta::assert_snapshot!(format!("effort_ultracode_{name}_{state}_100"), shown);
            assert!(
                shown.contains(&format!("    Ultracode  {state} · Tab to toggle")),
                "{shown}"
            );
        }
    }
}

const CONTEXTS: [&str; 4] = [
    "\"fresh\"",
    "\"note\"",
    "\"note+transcript\"",
    "\"note+summary\"",
];

/// `/fallback`'s rows once note+summary is picked, which brings the summarizer row in.
fn fallback_rows() -> Vec<SettingRow> {
    vec![
        row(
            "fallback.mode",
            "\"auto\"",
            "built-in",
            &["\"ask\"", "\"auto\"", "\"wait\""],
            "When an agent stops, such as at a usage limit: ask you, go to the next \
             fallback, or wait for the agent. A role's own setting wins",
        ),
        row(
            "fallback.broken_work",
            "\"continue\"",
            "built-in",
            &["\"continue\"", "\"restart\""],
            "What the new worker does with broken work in progress: continue from it, or \
             restart. A role's own setting wins",
        ),
        row(
            "fallback.context",
            "\"note+summary\"",
            "project",
            &CONTEXTS,
            "What the fallback is told: the task only (fresh), the fallback note, the note \
             and the old session's transcript, or the note and a summary of it. A role's own \
             setting wins",
        ),
        row(
            "fallback.summarizer",
            "",
            "built-in",
            &[],
            "Who summarizes the old session for note+summary: cli/model or an endpoint. \
             Unset uses the fallback's CLI with its cheapest model. A role's own \
             setting wins",
        ),
    ]
}

/// The fallback rows with the fallback list, empty as it is unless you set one.
fn fallback_rows_with_fallback() -> Vec<SettingRow> {
    let mut rows = fallback_rows();
    rows.insert(
        1,
        row(
            "fallback.order",
            "[]",
            "built-in",
            &[],
            "The fallbacks, in order: an agent, or a table with its own rules. With no list \
             anywhere, the other installed and logged-in CLIs. A role's own setting wins",
        ),
    );
    rows
}

#[test]
fn fallback_picker() {
    let mut picker = flow::settings_picker("fallback", fallback_rows());
    check_picker("fallback_picker", &picker);
    let shown = picker_lines(&picker, 120);
    // Names, as /permissions has them, with the value and where it's set.
    assert!(
        shown.contains("What the fallback is told        note+summary · this project"),
        "{shown}"
    );
    assert!(shown.contains("Who summarizes the old session"), "{shown}");
    assert!(!shown.contains("summarizer"), "{shown}");

    // The four values, each with what the fallback gets, the one in effect marked.
    press(&mut picker, KeyCode::Char('3'));
    check_picker("fallback_context_values", &picker);
    let values = picker_lines(&picker, 120);
    assert!(
        values.contains("fresh") && values.contains("The task only"),
        "{values}"
    );
    assert!(values.contains("note+summary ✓"), "{values}");
    assert!(values.contains("Reset"), "{values}");

    // Picking one asks where to save it, this project first, like every fallback row.
    press(&mut picker, KeyCode::Char('2'));
    let scope = picker_lines(&picker, 120);
    assert!(scope.contains("fallback.context = note"), "{scope}");
    let Step::Done(UiCommand::SetSetting { key, value, scope }) =
        press(&mut picker, KeyCode::Enter)
    else {
        panic!("the scope answer saves the value");
    };
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("fallback.context", "\"note\"", SettingScope::Project)
    );

    // The summarizer takes any worker, typed.
    let mut picker = flow::settings_picker("fallback", fallback_rows());
    press(&mut picker, KeyCode::Char('4'));
    assert_eq!(picker.input.as_deref(), Some(""));
    check_picker("fallback_summarizer", &picker);
}

#[test]
fn fallback_picker_shows_the_autonomous_defaults() {
    let picker = flow::settings_picker("fallback", fallback_rows_with_fallback());
    check_picker("fallback_picker_defaults", &picker);
    // The next fallback is picked on its own, from the other CLIs.
    let shown = picker_lines(&picker, 120);
    assert!(
        shown.contains("When an agent stops              auto · built-in"),
        "{shown}"
    );
    assert!(
        shown.contains("Who it falls back to             other logged-in CLIs · built-in"),
        "{shown}"
    );
}

/// Types each character into the picker, one key at a time.
fn type_text(picker: &mut Picker, text: &str) {
    for ch in text.chars() {
        press(picker, KeyCode::Char(ch));
    }
}

#[test]
fn fallback_picker_with_vim_keys() {
    let vim_picker = || {
        let mut picker = flow::settings_picker("fallback", fallback_rows());
        picker.vim = true;
        picker
    };
    let mut picker = vim_picker();
    let shown = picker_lines(&picker, 120);
    assert!(shown.contains("j/k to move"), "{shown}");

    // Letters move and never filter: `j` reaches the context row and `l` opens it, with the
    // pointer on the value in effect.
    press(&mut picker, KeyCode::Char('c'));
    assert_eq!(picker.filter, "");
    press(&mut picker, KeyCode::Char('j'));
    press(&mut picker, KeyCode::Char('j'));
    press(&mut picker, KeyCode::Char('l'));
    assert_eq!(picker.title, "What the fallback is told");
    assert_eq!(picker.pointed(), Some(3));
    let mut moves = Vec::new();
    for key in ['k', 'g', 'G', 'k', 'k', 'k'] {
        press(&mut picker, KeyCode::Char(key));
        moves.push(picker.pointed());
    }
    let reset = Some(4);
    assert_eq!(moves, [Some(2), Some(0), reset, Some(3), Some(2), Some(1)]);

    // `l` chooses `note` and then the scope under the pointer, this project.
    press(&mut picker, KeyCode::Char('l'));
    let scope = picker_lines(&picker, 120);
    assert!(scope.contains("fallback.context = note"), "{scope}");
    let Step::Done(UiCommand::SetSetting { key, value, scope }) =
        press(&mut picker, KeyCode::Char('l'))
    else {
        panic!("`l` saves the value where the pointer is");
    };
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("fallback.context", "\"note\"", SettingScope::Project)
    );

    // `h` goes back to the keys, as ← does, and closes from them without saving anything.
    let mut picker = vim_picker();
    type_text(&mut picker, "jjl");
    assert_eq!(press(&mut picker, KeyCode::Char('h')), Step::Stay);
    assert_eq!(picker.pointed(), Some(2), "back on the key it opened");
    assert!(matches!(
        press(&mut picker, KeyCode::Char('h')),
        Step::Cancel { .. }
    ));

    // The summarizer is typed, so `hjkl` in a worker's name are text, and the scope keeps its
    // letter.
    let mut picker = vim_picker();
    type_text(&mut picker, "Gl");
    assert_eq!(picker.input.as_deref(), Some(""));
    type_text(&mut picker, "claude-code/haiku");
    assert_eq!(picker.input.as_deref(), Some("claude-code/haiku"));
    press(&mut picker, KeyCode::Enter);
    let scope = picker_lines(&picker, 120);
    assert!(
        scope.contains("fallback.summarizer = claude-code/haiku"),
        "{scope}"
    );
    let Step::Done(UiCommand::SetSetting { key, value, scope }) =
        press(&mut picker, KeyCode::Char('2'))
    else {
        panic!("a scope's number saves the value");
    };
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        (
            "fallback.summarizer",
            "\"claude-code/haiku\"",
            SettingScope::ProjectLocal
        )
    );

    // `/` filters the rows, Esc leaves the filter and keeps the picker open, and Enter picks
    // what the filter kept.
    let mut picker = vim_picker();
    press(&mut picker, KeyCode::Char('/'));
    type_text(&mut picker, "xyz");
    assert!(picker.shown().is_empty());
    assert!(!matches!(
        press(&mut picker, KeyCode::Esc),
        Step::Cancel { .. }
    ));
    assert_eq!((picker.filtering, picker.filter.as_str()), (false, ""));
    press(&mut picker, KeyCode::Char('/'));
    type_text(&mut picker, "summarizes");
    assert_eq!(picker.shown().len(), 1);
    press(&mut picker, KeyCode::Enter);
    assert_eq!(picker.input.as_deref(), Some(""));
}

fn config_rows() -> Vec<SettingRow> {
    vec![
        row(
            "workflow",
            "\"council\"",
            "project",
            &[],
            "How the crew works together",
        ),
        row(
            "crew.main",
            "\"claude-code/opus\"",
            "global",
            &[],
            "Conversation, the estimate, and Captain work",
        ),
        row(
            "ui.theme",
            "\"dark\"",
            "global",
            &["\"auto\"", "\"dark\"", "\"light\""],
            "Light or dark colors, or follow the terminal",
        ),
        row(
            "ui.keys",
            "\"vim\"",
            "session",
            &["\"standard\"", "\"vim\""],
            "Standard or vim keys in the input",
        ),
        row(
            "ui.accent",
            "",
            "built-in",
            &[],
            "A hex color for the accent",
        ),
        row(
            "limits.spend_per_run",
            "30.0",
            "project-local",
            &[],
            "The most one run may spend",
        ),
        row(
            "approvals.merge.mode",
            "\"auto_preview\"",
            "flag",
            &["\"supervised\"", "\"auto_preview\"", "\"auto\""],
            "Who approves a merge",
        ),
        row(
            "roles.reviewer.effort",
            "\"high\"",
            "workflow",
            &[],
            "How hard the reviewer thinks",
        ),
    ]
}

fn ctx<'a>(theme: &'a argonauts_tui::Theme, state: &'a AppState) -> ViewCtx<'a> {
    ViewCtx {
        theme,
        state,
        now: t0(),
    }
}

fn draw(view: &mut dyn View, width: u16, height: u16, state: &AppState) -> Buffer {
    let theme = theme();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.render(area, frame.buffer_mut(), &ctx(&theme, state));
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn screen(view: &mut dyn View, width: u16, height: u16, state: &AppState) -> String {
    let buf = draw(view, width, height, state);
    assert_frames(&buf);
    buffer_text(&buf)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(view: &mut dyn View, code: KeyCode, state: &AppState) -> ViewOutcome {
    let theme = theme();
    view.handle_key(KeyEvent::from(code), &ctx(&theme, state))
}

#[test]
fn config_view() {
    let state = AppState::default();
    for width in WIDTHS {
        screen(&mut ConfigView::new(config_rows()), width, 24, &state);
    }
    for (width, height) in SIZES {
        let mut view = ConfigView::new(config_rows());
        insta::assert_snapshot!(
            format!("config_view_{width}"),
            screen(&mut view, width, height, &state)
        );
    }

    let mut view = ConfigView::new(config_rows());
    let shown = screen(&mut view, 120, 32, &state);
    for (key, layer) in [
        ("ui.theme", "everywhere (global)"),
        ("ui.accent", "built-in"),
        ("limits.spend_per_run", "just you (project-local)"),
        ("ui.keys", "this session"),
        ("approvals.merge.mode", "--set"),
        ("roles.reviewer.effort", "workflow"),
        ("workflow", "this project"),
    ] {
        let line = shown.lines().find(|line| line.contains(key)).unwrap();
        assert!(line.contains(layer), "{key} shows {layer}: {line:?}");
    }
    assert!(shown.contains("! approvals.merge.mode"), "{shown}");

    // Search, edit, then save.
    for ch in "theme".chars() {
        key(&mut view, KeyCode::Char(ch), &state);
    }
    let found = screen(&mut view, 120, 32, &state);
    assert!(found.contains("1 of 8 match"), "{found}");
    assert!(!found.contains("ui.keys"), "{found}");
    key(&mut view, KeyCode::Enter, &state);
    let editing = screen(&mut view, 120, 32, &state);
    assert!(editing.contains("dark ✓"), "{editing}");
    assert!(editing.contains("Reset"), "{editing}");
    key(&mut view, KeyCode::Char('3'), &state);
    let outcome = key(&mut view, KeyCode::Char('2'), &state);
    let ViewOutcome::Send(command) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        command,
        UiCommand::SetSetting {
            key: "ui.theme".into(),
            value: "\"light\"".into(),
            scope: SettingScope::ProjectLocal
        }
    );

    // Esc leaves an editor for the list, then clears the search, then closes.
    key(&mut view, KeyCode::Enter, &state);
    assert!(matches!(
        key(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Consumed
    ));
    assert!(matches!(
        key(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Consumed
    ));
    assert!(view.filter.is_empty());
    assert!(matches!(
        key(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Close
    ));
}

#[test]
fn config_view_opens_an_editor_with_right_and_left_goes_back() {
    let state = AppState::default();
    let mut view = ConfigView::new(config_rows());
    for ch in "theme".chars() {
        key(&mut view, KeyCode::Char(ch), &state);
    }
    key(&mut view, KeyCode::Right, &state);
    let editing = screen(&mut view, 120, 32, &state);
    assert!(editing.contains("dark ✓"), "{editing}");
    // → on a value saves nothing; that waits for Enter or its number.
    assert!(matches!(
        key(&mut view, KeyCode::Right, &state),
        ViewOutcome::Consumed
    ));
    key(&mut view, KeyCode::Char('3'), &state);
    assert!(screen(&mut view, 120, 32, &state).contains("Where to save it"));
    key(&mut view, KeyCode::Left, &state);
    assert!(screen(&mut view, 120, 32, &state).contains("dark ✓"));
    assert!(matches!(
        key(&mut view, KeyCode::Left, &state),
        ViewOutcome::Consumed
    ));
    let list = screen(&mut view, 120, 32, &state);
    assert!(!list.contains("dark ✓"), "← leaves the editor\n{list}");
    assert!(list.contains("1 of 8 match"), "{list}");
}

#[test]
fn config_view_lists_the_fallback_context_and_summarizer() {
    let mut rows = config_rows();
    rows.extend([
        row(
            "fallback.context",
            "\"note+summary\"",
            "project",
            &CONTEXTS,
            "What the fallback is told",
        ),
        row(
            "fallback.summarizer",
            "\"claude-code/haiku\"",
            "project-local",
            &[],
            "Who summarizes the old session for note+summary",
        ),
        row(
            "fallback.main.context",
            "",
            "built-in",
            &CONTEXTS,
            "What the fallback is told",
        ),
    ]);
    let state = AppState::default();
    for (width, height) in SIZES {
        let mut view = ConfigView::new(rows.clone());
        for ch in "fallback".chars() {
            key(&mut view, KeyCode::Char(ch), &state);
        }
        let shown = screen(&mut view, width, height, &state);
        insta::assert_snapshot!(format!("config_view_fallback_{width}"), shown);
        for (key, value, layer) in [
            ("fallback.context", "note+summary", "this project"),
            (
                "fallback.summarizer",
                "claude-code/haiku",
                "just you (project-local)",
            ),
            ("fallback.main.context", "unset", "built-in"),
        ] {
            let line = shown
                .lines()
                .find(|line| line.contains(&format!(" {key} ")))
                .unwrap_or_else(|| panic!("no {key} in {shown}"));
            assert!(line.contains(value) && line.contains(layer), "{line:?}");
        }
    }
}

#[test]
fn config_view_edits_the_fallback_summarizer_with_vim_keys() {
    let mut rows = config_rows();
    rows.push(row(
        "fallback.summarizer",
        "\"claude-code/haiku\"",
        "project-local",
        &[],
        "Who summarizes the old session for note+summary",
    ));
    let mut state = AppState::default();
    state.ui = Some(UiSettings {
        keys: KeyScheme::Vim,
        ..UiSettings::default()
    });
    let theme = theme();
    let mut view = ConfigView::new(rows);
    let type_text = |view: &mut ConfigView, text: &str| {
        for ch in text.chars() {
            key(view, KeyCode::Char(ch), &state);
        }
    };

    // The search starts with `/`; Enter opens the row it finds. The value is set in a file, so
    // `l` picks "Type a new value" over Reset, and from there letters are the new value.
    type_text(&mut view, "/summarizer");
    key(&mut view, KeyCode::Enter, &state);
    key(&mut view, KeyCode::Char('l'), &state);
    let clear = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    view.handle_key(clear, &ctx(&theme, &state));
    type_text(&mut view, "ollama/llama3.2");
    key(&mut view, KeyCode::Enter, &state);
    let outcome = key(&mut view, KeyCode::Char('2'), &state);
    let ViewOutcome::Send(UiCommand::SetSetting { key, value, scope }) = outcome else {
        panic!("the scope's number saves the value: {outcome:?}");
    };
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        (
            "fallback.summarizer",
            "\"ollama/llama3.2\"",
            SettingScope::ProjectLocal
        )
    );
}

fn approvals_state() -> AppState {
    let mut state = AppState::default();
    let id = RunId::new("run-01").unwrap();
    let mut run = RunState::new(id.clone());
    run.title = "Fix the flaky export test".into();
    run.spot_checks = vec![ApprovalId::numbered(3)];
    state.runs.insert(id, run);
    state
}

fn approvals() -> ApprovalsView {
    let data = ApprovalsData {
        points: vec![ApprovalRow {
            kind: ApprovalKind::Merge,
            mode: ApprovalMode::AutoPreview,
            decide_with: DecideWith::Rules,
            matches_in_a_row: 12,
            misses: 1,
            suggest_auto: true,
        }],
        spot_checks_waiting: 1,
        judge: Some("codex/gpt-5.6".parse().unwrap()),
        warnings: Vec::new(),
    };
    let rows = vec![
        row(
            "approvals.merge.mode",
            "\"auto_preview\"",
            "project",
            &["\"supervised\"", "\"auto_preview\"", "\"auto\""],
            "Who approves a merge",
        ),
        row(
            "approvals.merge.decide_with",
            "\"rules\"",
            "built-in",
            &["\"rules\"", "\"model\""],
            "What decides in auto",
        ),
        row(
            "approvals.rules.merge.min_test_coverage_of_changed_lines",
            "0.8",
            "built-in",
            &[],
            "Coverage a merge needs",
        ),
        row(
            "approvals.merge.otherwise",
            "\"ask\"",
            "built-in",
            &["\"ask\"", "\"stop\""],
            "When auto doesn't approve",
        ),
        row(
            "approvals.decides",
            "\"codex/gpt-5.6\"",
            "global",
            &[],
            "Who settles what your rules can't decide",
        ),
        row(
            "approvals.judge.model_rules",
            "\"~/.argonauts/judge.md\"",
            "global",
            &[],
            "The judge's rules file",
        ),
        row(
            "approvals.spot_check_sample",
            "0.1",
            "built-in",
            &[],
            "Share of auto calls to check",
        ),
    ];
    let mut view = ApprovalsView::new(data);
    view.rows = rows;
    view
}

#[test]
fn approvals_view() {
    let state = approvals_state();
    for width in WIDTHS {
        screen(&mut approvals(), width, 32, &state);
    }
    for (width, height) in SIZES {
        let mut view = approvals();
        insta::assert_snapshot!(
            format!("approvals_view_{width}"),
            screen(&mut view, width, height, &state)
        );
    }

    let mut view = approvals();
    let shown = screen(&mut view, 120, 40, &state);
    assert!(
        shown.contains("Merge · auto preview · your rules decide"),
        "{shown}"
    );
    assert!(
        shown.contains("12 matches in a row · 1 miss · ready for auto"),
        "{shown}"
    );
    // Rows go by name, with no `!` on each, since every key here is a safety key.
    assert!(shown.contains("Rules file"), "{shown}");
    assert!(!shown.contains("model_rules"), "{shown}");
    assert!(!shown.contains(" ! "), "{shown}");
    assert!(
        shown.contains("Who decides in auto           your rules"),
        "{shown}"
    );
    assert!(
        shown.contains("Fix the flaky export test · approval-003"),
        "{shown}"
    );

    // Down to the spot check, then mark it wrong.
    for _ in 0..7 {
        key(&mut view, KeyCode::Down, &state);
    }
    let outcome = key(&mut view, KeyCode::Char('n'), &state);
    let ViewOutcome::Send(command) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        command,
        UiCommand::SpotCheck {
            run: RunId::new("run-01").unwrap(),
            id: ApprovalId::numbered(3),
            correct: false
        }
    );
    let marked = screen(&mut view, 120, 40, &state);
    assert!(marked.contains("marked wrong"), "{marked}");

    // An edit goes through the scope question. The merge's mode is past who checks.
    key(&mut view, KeyCode::Home, &state);
    for _ in 0..2 {
        key(&mut view, KeyCode::Down, &state);
    }
    key(&mut view, KeyCode::Enter, &state);
    key(&mut view, KeyCode::Char('3'), &state);
    let question = screen(&mut view, 120, 40, &state);
    assert!(question.contains("Where to save it"), "{question}");
    let outcome = key(&mut view, KeyCode::Char('3'), &state);
    let ViewOutcome::Send(command) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        command,
        UiCommand::SetSetting {
            key: "approvals.merge.mode".into(),
            value: "\"auto\"".into(),
            scope: SettingScope::Project
        }
    );
}

fn paste(view: &mut dyn View, text: &str, state: &AppState) -> ViewOutcome {
    let theme = theme();
    view.handle_paste(text, &ctx(&theme, state))
}

#[test]
fn pastes_reach_the_settings_editors() {
    let state = AppState::default();
    let mut view = ConfigView::new(config_rows());
    // Without an editor, a paste searches as typing does.
    paste(&mut view, "accent\n", &state);
    assert_eq!(view.filter, "accent");
    key(&mut view, KeyCode::Enter, &state);
    // In the editor's typed line, one row: the line break goes.
    assert!(matches!(
        paste(&mut view, "#ff8800\n", &state),
        ViewOutcome::Consumed
    ));
    let editing = screen(&mut view, 120, 32, &state);
    assert!(editing.contains("#ff8800"), "{editing}");
    key(&mut view, KeyCode::Enter, &state);
    let outcome = key(&mut view, KeyCode::Char('2'), &state);
    let ViewOutcome::Send(UiCommand::SetSetting {
        key: saved, value, ..
    }) = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(saved, "ui.accent");
    assert!(value.contains("#ff8800"), "{value}");

    let state = approvals_state();
    let mut view = approvals();
    // The list has no line to paste into.
    assert!(matches!(
        paste(&mut view, "0.95", &state),
        ViewOutcome::Ignored
    ));
    key(&mut view, KeyCode::Home, &state);
    for _ in 0..7 {
        key(&mut view, KeyCode::Up, &state);
    }
    // Past who checks, the judge and its rules file, to the merge's coverage.
    for _ in 0..4 {
        key(&mut view, KeyCode::Down, &state);
    }
    key(&mut view, KeyCode::Enter, &state);
    let editing = screen(&mut view, 120, 40, &state);
    assert!(
        editing.contains("Merge · Changed lines tests cover"),
        "{editing}"
    );
    paste(&mut view, "0.95", &state);
    let pasted = screen(&mut view, 120, 40, &state);
    assert!(pasted.contains("0.95"), "{pasted}");
}

const BOOL: [&str; 2] = ["true", "false"];
const SANDBOX_MODES: [&str; 4] = ["\"os\"", "\"container\"", "\"tool\"", "\"none\""];

/// What `/permissions` answers: every agent's sandbox at its defaults, the daily cap set
/// everywhere, and three roles, the worker with its own rules set in this project.
fn permission_rows() -> Vec<SettingRow> {
    let safety = |mut row: SettingRow| {
        row.safety = true;
        row
    };
    let mut rows = vec![
        row(
            "sandbox.mode",
            "\"os\"",
            "built-in",
            &SANDBOX_MODES,
            "Whose rules guard the agents: the OS sandbox, a container, each AI tool's own, or \
             none at all. A role's own setting wins",
        ),
        row(
            "sandbox.network",
            "true",
            "built-in",
            &BOOL,
            "Whether commands agents run can reach the network. A role's own setting wins",
        ),
        row(
            "sandbox.agents_see_other_worktrees",
            "false",
            "built-in",
            &BOOL,
            "Whether an agent can read the other agents' worktrees of the repo, not only its own. \
             A role's own setting wins",
        ),
        row(
            "sandbox.agents_commit",
            "true",
            "built-in",
            &BOOL,
            "Whether agents commit their own work. Off, Argonauts Code commits for them. A role's \
             own setting wins",
        ),
        row(
            "sandbox.fail_closed",
            "true",
            "built-in",
            &BOOL,
            "Refuse to start a CLI session when no sandbox works",
        ),
        row(
            "limits.spend_per_day",
            "50.0",
            "global",
            &[],
            "The most you spend a day across every run and repo, in dollars",
        ),
    ];
    let own = [
        ("worker", "mode", "\"tool\""),
        ("worker", "network", "false"),
    ];
    for role in ["main", "worker", "reviewer"] {
        for (leaf, allowed) in [
            ("mode", &SANDBOX_MODES[..]),
            ("network", &BOOL[..]),
            ("agents_see_other_worktrees", &BOOL[..]),
            ("agents_commit", &BOOL[..]),
        ] {
            let set = own
                .iter()
                .find(|(who, name, _)| *who == role && *name == leaf)
                .map(|(.., value)| *value);
            let description = rows
                .iter()
                .find(|row| row.key == format!("sandbox.{leaf}"))
                .map(|row| row.description.replace(". A role's own setting wins", ""))
                .unwrap_or_default();
            rows.push(row(
                &format!("roles.{role}.sandbox.{leaf}"),
                set.unwrap_or(""),
                if set.is_some() { "project" } else { "built-in" },
                allowed,
                &description,
            ));
        }
    }
    rows.into_iter().map(safety).collect()
}

#[test]
fn permissions_picker() {
    let mut picker = flow::settings_picker("permissions", permission_rows());
    check_picker("permissions_picker", &picker);
    let shown = picker_lines(&picker, 120);
    for words in [
        "Whose rules guard the agents",
        "Argonauts Code's (OS sandbox) · built-in",
        "Internet for agents' commands",
        "Agents see each other's worktrees",
        "Agents make their own commits",
        "Daily spend cap",
        "Main agent",
        "Its own: rules each AI tool's own · internet off",
        "same as every agent",
    ] {
        assert!(shown.contains(words), "{words}:\n{shown}");
    }

    // Whose rules: each choice in words, the one in effect marked, then where to save it.
    press(&mut picker, KeyCode::Char('1'));
    check_picker("permissions_mode_values", &picker);
    let values = picker_lines(&picker, 120);
    assert!(
        values.contains("Argonauts Code's (OS sandbox) ✓"),
        "{values}"
    );
    assert!(
        values.contains("each AI tool's own")
            && values.contains("No protection at all, neither Argonauts Code's nor the tool's"),
        "{values}"
    );
    press(&mut picker, KeyCode::Char('3'));
    let scope = picker_lines(&picker, 120);
    assert!(scope.contains("sandbox.mode = tool"), "{scope}");
    let Step::Done(UiCommand::SetSetting { key, value, scope }) =
        press(&mut picker, KeyCode::Enter)
    else {
        panic!("the scope answer saves the value");
    };
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        ("sandbox.mode", "\"tool\"", SettingScope::Project)
    );

    // A role opens its own keys. What it leaves unset follows every agent.
    let mut picker = flow::settings_picker("permissions", permission_rows());
    press(&mut picker, KeyCode::Char('9'));
    check_picker("permissions_role", &picker);
    let role = picker_lines(&picker, 120);
    assert!(role.contains("Permissions for reviewer"), "{role}");
    assert!(
        role.contains("same as every agent: Argonauts Code's (OS sandbox)"),
        "{role}"
    );
    press(&mut picker, KeyCode::Char('2'));
    let network = picker_lines(&picker, 120);
    assert!(
        network.contains("Internet for agents' commands · reviewer"),
        "{network}"
    );
    assert!(
        network.contains("Unset, so the same as every agent: on"),
        "{network}"
    );
    assert!(!network.contains("Reset"), "{network}");
    press(&mut picker, KeyCode::Char('2'));
    let Step::Done(UiCommand::SetSetting { key, value, scope }) =
        press(&mut picker, KeyCode::Enter)
    else {
        panic!("the scope answer saves the value");
    };
    assert_eq!(
        (key.as_str(), value.as_str(), scope),
        (
            "roles.reviewer.sandbox.network",
            "false",
            SettingScope::Project
        )
    );

    // The worker's own rules can be reset back to every agent's.
    let mut picker = flow::settings_picker("permissions", permission_rows());
    press(&mut picker, KeyCode::Char('8'));
    press(&mut picker, KeyCode::Char('1'));
    let mode = picker_lines(&picker, 120);
    assert!(mode.contains("each AI tool's own ✓"), "{mode}");
    assert!(mode.contains("Reset"), "{mode}");
}

#[test]
fn permissions_for_one_role_open_it_at_once() {
    let rows: Vec<SettingRow> = permission_rows()
        .into_iter()
        .filter(|row| {
            row.key.starts_with("roles.worker.")
                || (row.key.starts_with("sandbox.") && row.key != "sandbox.fail_closed")
        })
        .collect();
    let picker = flow::settings_picker("permissions", rows);
    let shown = picker_lines(&picker, 120);
    assert!(shown.contains("Permissions for worker"), "{shown}");
    assert!(
        shown.contains("Whose rules guard the agents")
            && shown.contains("each AI tool's own · this project"),
        "{shown}"
    );
    assert!(shown.contains("same as every agent: no"), "{shown}");
    assert_eq!(picker.rows.len(), 4, "{shown}");
}

/// `/approvals` at the built-in settings: the approvals decide on their own, rules and a
/// reviewer from another provider together, merges by rules alone, and you're asked otherwise.
fn autonomous_approvals() -> ApprovalsView {
    let point = |kind, decide_with| ApprovalRow {
        kind,
        mode: ApprovalMode::Auto,
        decide_with,
        matches_in_a_row: 0,
        misses: 0,
        suggest_auto: false,
    };
    let data = ApprovalsData {
        points: vec![
            point(ApprovalKind::Understand, DecideWith::RulesAndModel),
            point(ApprovalKind::Merge, DecideWith::Rules),
        ],
        spot_checks_waiting: 0,
        judge: None,
        warnings: Vec::new(),
    };
    let modes = ["\"supervised\"", "\"auto_preview\"", "\"auto\""];
    let deciders = [
        "\"rules\"",
        "\"model\"",
        "\"rules_and_model\"",
        "\"rules_or_model\"",
    ];
    let rows = vec![
        row(
            "approvals.understand.mode",
            "\"auto\"",
            "built-in",
            &modes,
            "Goal: who approves",
        ),
        row(
            "approvals.understand.decide_with",
            "\"rules_and_model\"",
            "built-in",
            &deciders,
            "Goal: who decides in auto mode",
        ),
        row(
            "approvals.understand.otherwise",
            "\"ask\"",
            "built-in",
            &["\"ask\"", "\"send_back\"", "\"stop\""],
            "Goal: what happens when auto doesn't approve",
        ),
        row(
            "approvals.merge.mode",
            "\"auto\"",
            "built-in",
            &modes,
            "Merge: who approves",
        ),
        row(
            "approvals.merge.decide_with",
            "\"rules\"",
            "built-in",
            &deciders,
            "Merge: who decides in auto mode",
        ),
        row(
            "approvals.merge.otherwise",
            "\"ask\"",
            "built-in",
            &[
                "\"ask\"",
                "\"send_back\"",
                "\"stop\"",
                "\"open_pull_request\"",
            ],
            "Merge: what happens when auto doesn't approve",
        ),
        row(
            "approvals.decides",
            "\"judge\"",
            "built-in",
            &[],
            "Who settles what your rules can't decide: judge, or a model you pick",
        ),
    ];
    let mut view = ApprovalsView::new(data);
    view.rows = rows;
    view
}

#[test]
fn approvals_view_shows_the_autonomous_defaults() {
    let state = AppState::default();
    for width in WIDTHS {
        screen(&mut autonomous_approvals(), width, 32, &state);
    }
    for (width, height) in SIZES {
        insta::assert_snapshot!(
            format!("approvals_view_defaults_{width}"),
            screen(&mut autonomous_approvals(), width, height, &state)
        );
    }
    let shown = screen(&mut autonomous_approvals(), 120, 40, &state);
    for words in [
        "Understand · auto · rules and judge both agree",
        "Merge · auto · your rules decide",
        "the cheapest model from another provider",
        "cheapest, other provider",
    ] {
        assert!(shown.contains(words), "{words}:\n{shown}");
    }
}

/// Rows go by name, so names and values keep their width and a description takes only the
/// room left, left out rather than cut to a stub when there's no room for a few words.
#[test]
fn approvals_view_gives_descriptions_the_room_left() {
    let state = AppState::default();
    let view = || {
        let mut view = autonomous_approvals();
        view.rows.extend([
            row(
                "approvals.judge.model_rules",
                "\"~/.argonauts/judge-rules.md\"",
                "everywhere (global)",
                &[],
                "The judge's rules, in your words",
            ),
            row(
                "approvals.pause_auto_when_spot_checks_waiting",
                "5",
                "built-in",
                &[],
                "Pause auto approvals while this many spot checks wait",
            ),
            row(
                "approvals.refuse_ancestor_env",
                "[\"OPENCLAW_SHELL\", \"HERMES_SESSION_ID\"]",
                "built-in",
                &[],
                "Variables that mark another agent's shell",
            ),
        ]);
        view
    };
    for width in [80, 100] {
        let shown = screen(&mut view(), width, 32, &state);
        insta::assert_snapshot!(format!("approvals_view_long_rows_{width}"), shown);
    }
    let shown = screen(&mut view(), 100, 32, &state);
    for words in [
        "Spot checks that pause auto",
        "Shells refused approval",
        "When auto doesn't approve",
        "cheapest, other provider",
        "Goal: what happen…",
    ] {
        assert!(shown.contains(words), "{words}:\n{shown}");
    }
}

/// `/fallback`'s rows for every agent, then the main agent's and the worker's own.
fn fallback_rows_with_roles() -> Vec<SettingRow> {
    let modes = ["\"ask\"", "\"auto\"", "\"wait\""];
    let mut rows = fallback_rows_with_fallback();
    rows.push(row(
        "fallback.main.mode",
        "",
        "built-in",
        &modes,
        "When the main agent stops",
    ));
    rows.push(row(
        "fallback.worker.mode",
        "\"ask\"",
        "project",
        &modes,
        "When a worker stops",
    ));
    rows
}

#[test]
fn fallback_goes_by_name_with_a_row_per_role() {
    let mut picker = flow::settings_picker("fallback", fallback_rows_with_roles());
    let theme = theme();
    for width in [80, 100, 120, 200] {
        assert_lines(&picker.lines(&still(&theme, width)), width);
        insta::assert_snapshot!(
            format!("fallback_by_name_{width}"),
            picker_lines(&picker, width)
        );
    }
    let shown = picker_lines(&picker, 120);
    assert!(!shown.contains("fallback."), "{shown}");
    assert!(
        shown.contains("Main agent                       same as every agent"),
        "{shown}"
    );
    assert!(
        shown.contains("workers                          Its own: When an agent stops: ask"),
        "{shown}"
    );

    // A role's row opens its own keys, an unset one saying what every agent has.
    press(&mut picker, KeyCode::Char('7'));
    assert_eq!(picker.title, "Fallback for workers");
    let worker = picker_lines(&picker, 120);
    assert!(
        worker.contains("When an agent stops   ask · this project"),
        "{worker}"
    );
    press(&mut picker, KeyCode::Enter);
    assert_eq!(picker.title, "When an agent stops · workers");

    let mut picker = flow::settings_picker("fallback", fallback_rows_with_roles());
    press(&mut picker, KeyCode::Char('6'));
    let default = picker_lines(&picker, 120);
    assert!(
        default.contains("When an agent stops   same as every agent: auto"),
        "{default}"
    );
}

/// `/approvals` as the catalog words it: long descriptions, and values such as
/// `rules_and_model` that read in plain words.
fn approvals_like_the_catalog() -> ApprovalsView {
    let point = |kind, decide_with| ApprovalRow {
        kind,
        mode: ApprovalMode::Auto,
        decide_with,
        matches_in_a_row: 0,
        misses: 0,
        suggest_auto: false,
    };
    let data = ApprovalsData {
        points: vec![
            point(ApprovalKind::Plan, DecideWith::RulesAndModel),
            point(ApprovalKind::Merge, DecideWith::Rules),
        ],
        spot_checks_waiting: 0,
        judge: None,
        warnings: Vec::new(),
    };
    let approval = |name: &str, label: &str, decide_with: &str| {
        vec![
            row(
                &format!("approvals.{name}.mode"),
                "\"auto\"",
                "global",
                &["\"supervised\"", "\"auto_preview\"", "\"auto\""],
                &format!(
                    "{label}: supervised waits for you, auto preview also records what auto would \
                     do, auto lets your settings decide"
                ),
            ),
            row(
                &format!("approvals.{name}.decide_with"),
                decide_with,
                "built-in",
                &[
                    "\"rules\"",
                    "\"model\"",
                    "\"rules_and_model\"",
                    "\"rules_or_model\"",
                ],
                &format!("{label}: who decides in auto mode, your rules, your judge, or both"),
            ),
            row(
                &format!("approvals.{name}.otherwise"),
                "\"ask\"",
                "built-in",
                &["\"ask\"", "\"send_back\"", "\"stop\""],
                &format!("{label}: what happens when auto doesn't approve"),
            ),
            row(
                &format!("approvals.{name}.at_hard_limit"),
                "\"stop\"",
                "built-in",
                &["\"stop\"", "\"send_back\"", "\"wait\""],
                &format!("{label}: what a hard limit does on autopilot"),
            ),
        ]
    };
    // Autopilot first: the one switch and who answers in your place.
    let mut rows = vec![
        row(
            "approvals.autopilot",
            "false",
            "built-in",
            &["true", "false"],
            "Autopilot for every approval a run opens",
        ),
        row(
            "approvals.answers",
            "\"codex/gpt-5.6\"",
            "global",
            &[],
            "Who answers the crew's questions in your place",
        ),
    ];
    rows.extend(approval("plan", "The plan approval", "\"rules_and_model\""));
    rows.push(row(
        "approvals.rules.plan.max_score_spread",
        "1.0",
        "built-in",
        &[],
        "Plan rules: the largest gap between one judge's two scores, out of 10",
    ));
    rows.extend(approval("merge", "The merge approval", "\"rules\""));
    rows.push(row(
        "approvals.rules.merge.allow_debt",
        "false",
        "built-in",
        &["true", "false"],
        "Merge rules: whether auto may merge a task whose debt no strong enough model reviewed",
    ));
    rows.push(row(
        "approvals.decides",
        "\"judge\"",
        "built-in",
        &[],
        "Who settles what your rules can't decide: judge, or a model you pick",
    ));
    rows.push(row(
        "approvals.judge.second_model",
        "",
        "built-in",
        &[],
        "A second judge, from another agent, that must also approve",
    ));
    let mut view = ApprovalsView::new(data);
    view.rows = rows;
    view
}

#[test]
fn approvals_go_by_name_without_a_mark_on_every_row() {
    let state = AppState::default();
    for width in [80, 100, 120, 200] {
        let mut view = approvals_like_the_catalog();
        let shown = screen(&mut view, width, 32, &state);
        insta::assert_snapshot!(format!("approvals_by_name_{width}"), &shown);
        assert!(!shown.contains("decide_with"), "{shown}");
        assert!(!shown.contains("rules_and_mod"), "{shown}");
        assert!(!shown.contains(" ! "), "{shown}");
        // Values read whole, even at 80 columns.
        assert!(shown.contains("both agree"), "{shown}");
        assert!(shown.contains("cheapest, other provider"), "{shown}");
        assert!(shown.contains("GPT 5.6 on Codex"), "{shown}");
        assert!(shown.contains("stop safely"), "{shown}");
    }
}
