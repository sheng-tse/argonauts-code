//! `/model`'s effort line, `/effort`'s slider, and ←/→ between levels in the other pickers.

use argonauts_core::model_picker::{EffortLine, EffortSlider, EffortStop};

use super::*;

fn picker(screen: &Screen) -> &crate::bottom::Picker {
    screen
        .model
        .bottom
        .picker
        .as_ref()
        .expect("a picker is open")
}

fn effort(screen: &Screen) -> Option<EffortLine> {
    picker(screen).flow.effort.clone()
}

fn shown(screen: &Screen, width: u16) -> String {
    let theme = Theme::dark();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    plain_text(&picker(screen).lines(&ctx))
}

fn vim_screen() -> Screen {
    let mut screen = Screen::new();
    screen.model.bottom.mode.vim = Some(crate::bottom::VimMode::Insert);
    screen
}

fn picked(out: &Effects) -> (Option<RoleName>, Option<WorkerRef>, Option<String>) {
    match out.commands.as_slice() {
        [
            UiCommand::Slash {
                command:
                    SlashCommand::Model {
                        role,
                        worker,
                        effort,
                    },
            },
        ] => (*role, worker.clone(), effort.clone()),
        other => panic!("not a model pick: {other:?}"),
    }
}

#[test]
fn the_arrows_set_the_highlighted_models_effort_and_enter_saves_both() {
    let mut screen = Screen::new();
    screen.answer(models(None, claude_and_codex()));
    screen.press(KeyCode::Enter);
    let line = effort(&screen).expect("Default takes the CLI's levels");
    assert_eq!(line.level(), None, "the CLI decides until you choose");
    assert!(
        shown(&screen, 80).contains("◉ the CLI's own effort  ←/→ to adjust"),
        "{}",
        shown(&screen, 80)
    );

    screen.press(KeyCode::Down);
    let line = effort(&screen).unwrap();
    assert_eq!(
        line.levels,
        levels(&["low", "medium", "high", "xhigh", "max"])
    );
    assert_eq!(line.level(), Some("xhigh"));
    let text = shown(&screen, 80);
    assert!(
        text.contains("◉ xhigh effort (default)  ←/→ to adjust"),
        "{text}"
    );
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|line| line.contains("◉")).unwrap();
    assert!(
        lines[at - 2].contains("Other model…") && lines[at + 2].contains("Enter to choose"),
        "the line sits between the list and the hints\n{text}"
    );

    screen.press(KeyCode::Right);
    assert!(
        shown(&screen, 80).contains("◉ max effort (default xhigh)"),
        "{}",
        shown(&screen, 80)
    );
    screen.press(KeyCode::Right);
    assert_eq!(
        effort(&screen).unwrap().level(),
        Some("max"),
        "max is the end"
    );

    // The level carries over to the next model, and a model without levels hides the line.
    screen.press(KeyCode::Down);
    assert_eq!(pointed_label(&screen), "Opus 4.8");
    assert_eq!(effort(&screen).unwrap().level(), Some("max"));
    screen.press(KeyCode::Down);
    assert_eq!(effort(&screen), None);
    assert!(!shown(&screen, 80).contains("to adjust"));
    screen.press(KeyCode::Up);
    screen.press(KeyCode::Left);
    screen.press(KeyCode::Left);
    let out = screen.press(KeyCode::Enter);
    assert!(screen.model.bottom.picker.is_none());
    assert_eq!(
        picked(&out),
        (
            None,
            Some("claude-code/claude-opus-4-8".parse().unwrap()),
            Some("high".to_owned())
        )
    );
}

#[test]
fn an_untouched_effort_is_saved_only_when_the_new_model_cant_keep_the_one_set() {
    let set = |level: &str| {
        let mut choices = claude_and_codex();
        for choice in &mut choices {
            choice.effort = Some(level.to_owned());
        }
        choices[4].efforts = levels(&["low", "medium", "high"]);
        choices
    };
    let pick = |choices: Vec<ModelChoice>, row: usize| {
        let mut screen = Screen::new();
        screen.answer(models(Some(RoleName::Reviewer), choices));
        screen.press(KeyCode::Enter);
        for _ in 0..row {
            screen.press(KeyCode::Down);
        }
        picked(&screen.press(KeyCode::Enter))
    };
    let (role, worker, effort) = pick(set("max"), 1);
    assert_eq!(role, Some(RoleName::Reviewer));
    assert_eq!(worker, Some("claude-code/claude-opus-5-5".parse().unwrap()));
    assert_eq!(effort, None, "Opus 5.5 takes max, so nothing changes");
    let (_, _, effort) = pick(set("max"), 2);
    assert_eq!(
        effort.as_deref(),
        Some("high"),
        "Opus 4.8 here stops at high, its default"
    );
    let (_, _, effort) = pick(set("medium"), 2);
    assert_eq!(effort, None);
}

#[test]
fn with_vim_keys_h_and_l_set_the_effort_where_the_line_shows() {
    let mut screen = vim_screen();
    screen.answer(models(None, claude_and_codex()));
    screen.press(KeyCode::Char('l'));
    assert_eq!(screen.title(), "Claude Code");
    screen.press(KeyCode::Char('j'));
    screen.press(KeyCode::Char('l'));
    assert_eq!(effort(&screen).unwrap().level(), Some("max"));
    screen.press(KeyCode::Char('h'));
    screen.press(KeyCode::Char('h'));
    assert_eq!(effort(&screen).unwrap().level(), Some("high"));
    assert!(
        shown(&screen, 80).contains("h/l to adjust"),
        "{}",
        shown(&screen, 80)
    );
    assert!(screen.model.bottom.picker.is_some());

    // Where no line shows, h goes back a level as ← does.
    screen.press(KeyCode::Char('G'));
    assert_eq!(effort(&screen), None);
    screen.press(KeyCode::Char('h'));
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);
}

#[test]
fn without_vim_keys_no_hint_names_h_or_l() {
    let mut screen = Screen::new();
    screen.answer(models(None, claude_and_codex()));
    screen.press(KeyCode::Enter);
    screen.press(KeyCode::Down);
    let text = shown(&screen, 120);
    assert!(text.contains("←/→ to adjust") && text.contains("↑↓ to move"));
    assert!(!text.contains("h/l") && !text.contains("j/k"), "{text}");
}

#[test]
fn esc_from_a_roles_models_goes_back_to_the_roles() {
    let mut screen = Screen::new();
    screen.answer(settings(
        "model",
        vec![
            row("default_agent", "\"claude-code/opus\"", "built-in", &[]),
            row("roles.reviewer.workers", "[\"codex\"]", "project", &[]),
        ],
    ));
    assert_eq!(screen.title(), "Default agent");
    screen.answer(models(None, claude_and_codex()));
    assert_eq!(screen.labels(), ["Claude Code", "Codex", "Roles…"]);
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Down);
    let out = screen.press(KeyCode::Right);
    assert!(out.commands.is_empty(), "→ opens the roles as Enter does");
    assert_eq!(screen.title(), "Roles");
    let out = screen.press(KeyCode::Right);
    assert_eq!(
        out.commands,
        [UiCommand::Request {
            view: ViewRequest::Models {
                role: Some(RoleName::Reviewer)
            }
        }],
        "→ opens the role's models as Enter does"
    );
    screen.answer(models(Some(RoleName::Reviewer), claude_and_codex()));
    assert_eq!(screen.labels(), ["Claude Code", "Codex"]);
    let text = shown(&screen, 100);
    assert!(text.contains("Esc to go back"), "{text}");

    screen.press(KeyCode::Esc);
    assert_eq!(screen.title(), "Roles");
    assert_eq!(pointed_label(&screen), "reviewer");
    // A late answer for the left role's models stays out of the roles.
    screen.answer(models(Some(RoleName::Reviewer), claude_and_codex()));
    assert_eq!(screen.title(), "Roles");

    screen.press(KeyCode::Enter);
    screen.answer(models(Some(RoleName::Reviewer), claude_and_codex()));
    screen.press(KeyCode::Left);
    assert_eq!(screen.title(), "Roles", "← goes back as Esc does");
    let out = screen.press(KeyCode::Left);
    assert_eq!(
        out.commands,
        [UiCommand::Request {
            view: ViewRequest::Models { role: None }
        }],
        "← on the roles goes up to the default agent's CLIs"
    );
    assert_eq!(screen.title(), "Default agent");
    screen.answer(models(None, claude_and_codex()));
    screen.press(KeyCode::Left);
    assert_eq!(
        screen.title(),
        "Default agent",
        "← on the first level stays"
    );
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    assert!(
        screen.model.bottom.picker.is_none(),
        "Esc closes from the first"
    );
}

fn budget_rows() -> Vec<SettingRow> {
    vec![
        row("budget.run", "5", "built-in", &["5", "10", "20"]),
        row("budget.day", "20", "built-in", &["20", "50"]),
    ]
}

#[test]
fn left_goes_back_a_level_and_right_opens_one() {
    let mut screen = Screen::new();
    screen.answer(settings("budget", budget_rows()));
    let keys = screen.labels();
    assert_eq!(keys.len(), 2);
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Right);
    assert_eq!(screen.title(), "budget.day");
    let values = screen.labels();

    // → on a value waits for Enter, so it saves nothing.
    let out = screen.press(KeyCode::Right);
    assert!(out.commands.is_empty());
    assert_eq!(screen.labels(), values);

    screen.press(KeyCode::Left);
    assert_eq!(screen.labels(), keys);
    assert_eq!(pointed_label(&screen), keys[1], "back on the key it opened");
    screen.press(KeyCode::Left);
    assert!(
        screen.model.bottom.picker.is_some(),
        "the first level stays"
    );

    // From the scope question, ← goes back to the values.
    screen.press(KeyCode::Enter);
    screen.press(KeyCode::Enter);
    assert_eq!(screen.title(), "Where to save it");
    screen.press(KeyCode::Left);
    assert_eq!(screen.labels(), values);
    screen.press(KeyCode::Left);
    assert_eq!(screen.labels(), keys);
}

#[test]
fn with_vim_keys_h_goes_back_and_closes_from_the_first_level() {
    let mut screen = vim_screen();
    screen.answer(settings("budget", budget_rows()));
    screen.press(KeyCode::Char('l'));
    assert_eq!(screen.title(), "budget.run");
    screen.press(KeyCode::Char('h'));
    assert_eq!(screen.labels().len(), 2);
    screen.press(KeyCode::Char('h'));
    assert!(
        screen.model.bottom.picker.is_none(),
        "h closes, as Esc does"
    );
}

fn slider(role: Option<RoleName>, current: Option<&str>) -> EffortSlider {
    let stops = ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .map(|&level| EffortStop {
            level: level.into(),
            description: format!("What {level} means"),
        })
        .collect();
    EffortSlider::new(
        role,
        "claude-code/claude-opus-5-5".parse().unwrap(),
        "Opus 5.5",
        stops,
        Some("xhigh"),
        current,
    )
    .unwrap()
}

fn effort_answer(slider: EffortSlider) -> ViewData {
    ViewData::Effort { slider }
}

#[test]
fn the_effort_slider_adjusts_and_sends_the_level() {
    let mut screen = Screen::new();
    screen.answer(effort_answer(slider(None, None)));
    assert_eq!(screen.title(), "Effort");
    let text = shown(&screen, 80);
    for part in [
        "How long Opus 5.5 thinks before it answers, for the default agent",
        "Faster",
        "Smarter",
        "▲",
        "What xhigh means. The model's default",
        "←/→ to adjust · Enter to confirm · Esc to cancel",
    ] {
        assert!(text.contains(part), "{part:?} in\n{text}");
    }
    let lines: Vec<&str> = text.lines().collect();
    let marker = lines.iter().find(|line| line.contains('▲')).unwrap();
    let levels = lines.iter().find(|line| line.contains("medium")).unwrap();
    let column = |line: &str, part: &str| line[..line.find(part).unwrap()].chars().count();
    let xhigh = column(levels, "xhigh");
    assert!(
        (xhigh..xhigh + 5).contains(&column(marker, "▲")),
        "the marker sits over the chosen level\n{text}"
    );

    screen.press(KeyCode::Left);
    screen.press(KeyCode::Left);
    assert!(shown(&screen, 80).contains("What medium means"));
    assert!(!shown(&screen, 80).contains("The model's default"));
    // Typing goes nowhere while the slider is open.
    screen.press(KeyCode::Char('x'));
    let out = screen.press(KeyCode::Enter);
    assert!(screen.model.bottom.picker.is_none());
    assert_eq!(
        out.commands,
        [UiCommand::Slash {
            command: SlashCommand::Effort {
                role: None,
                level: Some("medium".into()),
                all_hands: None,
            },
        }]
    );
    assert!(screen.transcript().contains("/effort"));
}

#[test]
fn the_effort_slider_for_a_role_says_so_and_esc_keeps_the_level() {
    let mut screen = vim_screen();
    screen.answer(effort_answer(slider(Some(RoleName::Reviewer), Some("max"))));
    assert_eq!(screen.title(), "Effort for reviewer");
    assert!(shown(&screen, 80).contains("for the reviewer role"));
    assert!(shown(&screen, 80).contains("h/l to adjust"));
    screen.press(KeyCode::Char('h'));
    assert!(shown(&screen, 80).contains("What xhigh means"));
    screen.press(KeyCode::Char('l'));
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    let transcript = screen.transcript();
    assert!(transcript.contains("/effort reviewer"), "{transcript}");
    assert!(
        transcript.contains("Kept reviewer's effort as max"),
        "{transcript}"
    );
}

/// A CLI whose model names no default, with no level set, uses its own effort, as `/model`
/// says. The slider only starts in the middle, so Esc keeps no level.
#[test]
fn with_no_level_set_the_slider_says_the_cli_picks_and_esc_keeps_that() {
    let own = || {
        let stops = ["low", "medium", "high"]
            .iter()
            .map(|&level| EffortStop {
                level: level.into(),
                description: format!("What {level} means"),
            })
            .collect();
        EffortSlider::new(
            None,
            "antigravity-cli".parse().unwrap(),
            "Antigravity CLI's default model",
            stops,
            None,
            None,
        )
        .unwrap()
    };
    let mut screen = Screen::new();
    screen.answer(effort_answer(own()));
    for width in [80, 100, 120, 200] {
        let text = shown(&screen, width);
        assert!(
            text.contains("The CLI's own effort, until you pick a level"),
            "{text}"
        );
        assert!(!text.contains("What medium means"), "{text}");
    }
    screen.press(KeyCode::Esc);
    let transcript = screen.transcript();
    assert!(
        transcript.contains("Kept the CLI's own effort"),
        "{transcript}"
    );
    assert!(!transcript.contains("as medium"), "{transcript}");

    // Moving it picks a level like any other.
    let mut screen = Screen::new();
    screen.answer(effort_answer(own()));
    screen.press(KeyCode::Right);
    assert!(shown(&screen, 80).contains("What high means"));
}

/// Tab flips All hands on the default agent's slider, and Esc leaves it as it was. A role's
/// slider doesn't offer it, since it's about every task rather than how one role thinks.
#[test]
fn tab_flips_all_hands_on_the_default_agents_slider_only() {
    let mut screen = Screen::new();
    screen.answer(effort_answer(slider(None, None).with_all_hands(false)));
    assert!(shown(&screen, 80).contains("All hands  off"));
    screen.press(KeyCode::Tab);
    let text = shown(&screen, 80);
    assert!(text.contains("All hands  on"), "{text}");
    assert!(
        text.contains("All hands: every task goes to the crew"),
        "{text}"
    );
    screen.press(KeyCode::Tab);
    assert!(shown(&screen, 80).contains("All hands  off"));
    screen.press(KeyCode::Tab);
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    let transcript = screen.transcript();
    assert!(transcript.contains("Kept effort as xhigh"), "{transcript}");

    let mut screen = Screen::new();
    screen.answer(effort_answer(slider(Some(RoleName::Reviewer), None)));
    screen.press(KeyCode::Tab);
    let text = shown(&screen, 80);
    assert!(!text.contains("All hands"), "{text}");
    let out = screen.press(KeyCode::Enter);
    assert_eq!(
        out.commands,
        [UiCommand::Slash {
            command: SlashCommand::Effort {
                role: Some(RoleName::Reviewer),
                level: Some("xhigh".into()),
                all_hands: None,
            },
        }]
    );
}

#[test]
fn the_effort_slider_fits_every_width() {
    let mut screen = Screen::new();
    screen.answer(effort_answer(slider(None, Some("low"))));
    let theme = Theme::dark();
    for width in crate::stability::WIDTHS {
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        crate::stability::assert_lines(&picker(&screen).lines(&ctx), width);
    }
    let narrow = shown(&screen, 40);
    assert!(
        narrow.contains("Faster") && narrow.contains("max"),
        "{narrow}"
    );
}

/// agy's models as the controller sends them: its default takes agy's levels, a model named
/// for one takes that one only, and a model named without one takes none.
fn agy() -> Vec<ModelChoice> {
    let usual = levels(&["low", "medium", "high", "max"]);
    let fixed = |worker: &str, name: &str, level: &str| ModelChoice {
        name: name.into(),
        efforts: levels(&[level]),
        default_effort: Some(level.into()),
        ..noted(worker, None, false)
    };
    vec![
        ModelChoice {
            efforts: usual.clone(),
            ..noted("antigravity-cli", None, false)
        },
        fixed(
            "antigravity-cli/gemini-3.8-flash-high",
            "Gemini 3.8 Flash (High)",
            "high",
        ),
        fixed(
            "antigravity-cli/gemini-3.8-flash-low",
            "Gemini 3.8 Flash (Low)",
            "low",
        ),
        ModelChoice {
            name: "Claude Opus 4.6 (Thinking)".into(),
            ..noted("antigravity-cli/claude-opus-4-6-thinking", None, false)
        },
    ]
}

#[test]
fn a_model_fixed_at_one_level_shows_it_with_nothing_to_adjust() {
    let mut screen = Screen::new();
    screen.answer(models(None, agy()));
    screen.press(KeyCode::Enter);
    screen.press(KeyCode::Down);
    assert_eq!(pointed_label(&screen), "Gemini 3.8 Flash (High)");
    let text = shown(&screen, 80);
    assert!(
        text.contains("◉ high effort (fixed by the model)"),
        "{text}"
    );
    assert!(!text.contains("to adjust"), "{text}");
    screen.press(KeyCode::Left);
    screen.press(KeyCode::Right);
    assert_eq!(effort(&screen).unwrap().level(), Some("high"));

    screen.press(KeyCode::Down);
    assert!(shown(&screen, 80).contains("◉ low effort (fixed by the model)"));
    // One named without a level takes none, so it has no effort line.
    screen.press(KeyCode::Down);
    assert_eq!(pointed_label(&screen), "Claude Opus 4.6 (Thinking)");
    let text = shown(&screen, 80);
    assert!(!text.contains('◉'), "{text}");
    // Nothing carries over from a fixed level, so agy's own default starts as it would.
    for _ in 0..3 {
        screen.press(KeyCode::Up);
    }
    let text = shown(&screen, 80);
    assert!(
        text.contains("◉ the CLI's own effort  ←/→ to adjust"),
        "{text}"
    );
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Down);
    let out = screen.press(KeyCode::Enter);
    assert_eq!(
        picked(&out),
        (
            None,
            Some("antigravity-cli/gemini-3.8-flash-low".parse().unwrap()),
            None
        ),
        "a fixed level is the model's own, so nothing more is saved"
    );

    // A level set before that the model can't run is replaced by the one it runs.
    let mut choices = agy();
    for choice in &mut choices {
        choice.effort = Some("medium".into());
    }
    let mut screen = Screen::new();
    screen.answer(models(None, choices));
    screen.press(KeyCode::Enter);
    screen.press(KeyCode::Down);
    let (_, worker, effort) = picked(&screen.press(KeyCode::Enter));
    assert_eq!(
        worker,
        Some("antigravity-cli/gemini-3.8-flash-high".parse().unwrap())
    );
    assert_eq!(effort.as_deref(), Some("high"));
}

fn fixed_slider() -> EffortSlider {
    let stops = vec![EffortStop {
        level: "high".into(),
        description: String::new(),
    }];
    EffortSlider::new(
        None,
        "antigravity-cli/gemini-3.8-flash-high".parse().unwrap(),
        "Gemini 3.8 Flash (High)",
        stops,
        Some("high"),
        None,
    )
    .unwrap()
}

#[test]
fn effort_on_a_model_fixed_at_one_level_shows_it_instead_of_a_slider() {
    let mut screen = Screen::new();
    screen.answer(effort_answer(fixed_slider()));
    let text = shown(&screen, 100);
    for part in [
        "How long Gemini 3.8 Flash (High) thinks before it answers, for the default agent",
        "◉ high effort (fixed by the model)",
        "To change it, pick another model with /model.",
        "Esc to close",
    ] {
        assert!(text.contains(part), "{part:?} in\n{text}");
    }
    for absent in ["Faster", "Smarter", "▲", "to adjust", "Enter to confirm"] {
        assert!(!text.contains(absent), "{absent:?} in\n{text}");
    }
    screen.press(KeyCode::Right);
    assert_eq!(shown(&screen, 100), text);

    let out = screen.press(KeyCode::Enter);
    assert!(out.commands.is_empty(), "{:?}", out.commands);
    assert!(screen.model.bottom.picker.is_none());
    let transcript = screen.transcript();
    assert!(transcript.contains("Kept effort as high"), "{transcript}");
}

#[test]
fn a_fixed_level_fits_every_width() {
    let mut screen = Screen::new();
    screen.answer(effort_answer(fixed_slider()));
    let theme = Theme::dark();
    for width in crate::stability::WIDTHS {
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        crate::stability::assert_lines(&picker(&screen).lines(&ctx), width);
    }
}

/// The default agent on a model fixed at one level still offers All hands: Tab flips it, Enter
/// saves only it, and with the toggle back as it was Enter closes as Esc does.
#[test]
fn tab_then_enter_on_a_fixed_slider_saves_all_hands_alone() {
    let mut screen = Screen::new();
    screen.answer(effort_answer(fixed_slider().with_all_hands(false)));
    let text = shown(&screen, 100);
    for part in [
        "◉ high effort (fixed by the model)",
        "All hands  off · Tab to toggle",
        "To change it, pick another model with /model.",
        "Enter to confirm · Esc to cancel",
    ] {
        assert!(text.contains(part), "{part:?} in\n{text}");
    }
    assert!(!text.contains("every task goes to the crew"), "{text}");
    screen.press(KeyCode::Tab);
    let text = shown(&screen, 100);
    assert!(text.contains("All hands  on · Tab to toggle"), "{text}");
    assert!(
        text.contains("All hands: every task goes to the crew"),
        "{text}"
    );
    let out = screen.press(KeyCode::Enter);
    assert_eq!(
        out.commands,
        [UiCommand::Slash {
            command: SlashCommand::Effort {
                role: None,
                level: None,
                all_hands: Some(true),
            },
        }]
    );

    let mut screen = Screen::new();
    screen.answer(effort_answer(fixed_slider().with_all_hands(true)));
    screen.press(KeyCode::Tab);
    screen.press(KeyCode::Tab);
    let out = screen.press(KeyCode::Enter);
    assert!(out.commands.is_empty(), "{:?}", out.commands);
    assert!(screen.model.bottom.picker.is_none());
    let transcript = screen.transcript();
    assert!(transcript.contains("Kept effort as high"), "{transcript}");
}

fn no_levels() -> EffortSlider {
    EffortSlider::without_levels(None, "claude-code/haiku".parse().unwrap(), "Haiku 4.5")
}

/// Haiku takes no effort setting, so the default agent's `/effort` holds All hands alone:
/// the arrows do nothing, Tab flips it, and Enter saves it.
#[test]
fn a_model_without_levels_still_offers_all_hands() {
    let mut screen = Screen::new();
    screen.answer(effort_answer(no_levels().with_all_hands(false)));
    assert_eq!(screen.title(), "Effort");
    let text = shown(&screen, 100);
    for part in [
        "Haiku 4.5 takes no effort setting.",
        "All hands  off · Tab to toggle",
        "To set an effort, pick another model with /model.",
        "Enter to confirm · Esc to cancel",
    ] {
        assert!(text.contains(part), "{part:?} in\n{text}");
    }
    for absent in [
        "Faster",
        "Smarter",
        "▲",
        "◉",
        "to adjust",
        "fixed by the model",
    ] {
        assert!(!text.contains(absent), "{absent:?} in\n{text}");
    }
    screen.press(KeyCode::Right);
    screen.press(KeyCode::Left);
    assert_eq!(shown(&screen, 100), text);
    screen.press(KeyCode::Tab);
    assert!(shown(&screen, 100).contains("All hands  on"));
    let out = screen.press(KeyCode::Enter);
    assert_eq!(
        out.commands,
        [UiCommand::Slash {
            command: SlashCommand::Effort {
                role: None,
                level: None,
                all_hands: Some(true),
            },
        }]
    );

    let mut screen = Screen::new();
    screen.answer(effort_answer(no_levels().with_all_hands(true)));
    screen.press(KeyCode::Tab);
    let out = screen.press(KeyCode::Esc);
    assert!(out.commands.is_empty());
    let transcript = screen.transcript();
    assert!(transcript.contains("Nothing changed"), "{transcript}");
}

#[test]
fn all_hands_without_a_slider_fits_every_width() {
    let theme = Theme::dark();
    for slider in [fixed_slider(), no_levels()] {
        for on in [false, true] {
            let mut screen = Screen::new();
            screen.answer(effort_answer(slider.clone().with_all_hands(on)));
            for width in crate::stability::WIDTHS {
                let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
                crate::stability::assert_lines(&picker(&screen).lines(&ctx), width);
            }
        }
    }
}
