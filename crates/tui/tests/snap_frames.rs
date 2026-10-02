//! The frame checker over every view, card, picker, live region, and the transcript overlay,
//! at every width from 40 to 200 columns, with CJK, emoji, long unbroken strings, long paths,
//! and empty content. Each view keeps its instance across the widths, as a resize would.

mod pty_support;

use std::time::Instant;

use argonauts_core::app::{AppChoice, AppEvent, ModelChoice, SettingRow, ViewData};
use argonauts_core::events::checkpoint::CheckpointOpened;
use argonauts_core::events::{RunEventKind, checkpoint::CheckpointEvent};
use argonauts_core::input::MentionTarget;
use argonauts_core::limits::CostEstimate;
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::registry::ModelAccess;
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::ui::{Glyphs as GlyphChoice, UiSettings};
use argonauts_core::{CardId, RunId};
use argonauts_tui::HistoryCell;
use argonauts_tui::app::TranscriptOverlay;
use argonauts_tui::bottom::{BottomState, CardPrompt, OptionKind, Picker, PickerRow, WorkingLine};
use argonauts_tui::bridge::transcript::Transcript;
use argonauts_tui::bridge::{Cx, Effects};
use argonauts_tui::cards::{
    about_cells, checkpoint_card, choice_card, confirm_take_over, spot_check_card,
};
use argonauts_tui::keymap::Keymap;
use argonauts_tui::mentions::MentionPicker;
use argonauts_tui::model::ViewModel;
use argonauts_tui::render::{Clock, RenderCtx};
use argonauts_tui::stability::{
    UNSTABLE_GLYPHS, WIDTHS, assert_frames, assert_lines, buffer_text, check, check_glyphs,
    check_line_glyphs, check_lines, samples,
};
use argonauts_tui::state::{AppState, RunState};
use argonauts_tui::text::{plain_text, width_with};
use argonauts_tui::theme::{Glyphs, Theme};
use argonauts_tui::views::agent::AgentView;
use argonauts_tui::views::context::ContextView;
use argonauts_tui::views::crew::CrewView;
use argonauts_tui::views::help::HelpView;
use argonauts_tui::views::pager::Pager;
use argonauts_tui::views::resume::ResumePicker;
use argonauts_tui::views::session::SessionView;
use argonauts_tui::views::settings::approvals::ApprovalsView;
use argonauts_tui::views::settings::config::ConfigView;
use argonauts_tui::views::settings::flow::{self, Step};
use argonauts_tui::views::workflow::WorkflowView;
use argonauts_tui::views::{View, ViewCtx};
use crossterm::event::{KeyCode, KeyEvent};
use pty_support::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use serde_json::{Value, json};

const HEIGHT: u16 = 30;

/// Keys whose strings are text a person or an agent wrote, or a name: the sample goes after
/// them. Ids, keys, and enum values stay as they are, so the fixture still parses.
const TEXT_KEYS: [&str; 15] = [
    "title",
    "name",
    "label",
    "description",
    "summary",
    "text",
    "prompt",
    "outcome",
    "repo",
    "reason",
    "note",
    "detail",
    "tool",
    "family",
    "branch",
];

/// Keys whose arrays of strings get the sample as one more line.
const LINE_KEYS: [&str; 5] = ["body", "lines", "strengths", "attempts", "agreements"];

fn hostile(value: &mut Value, sample: &str) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                match value {
                    Value::String(text) if TEXT_KEYS.contains(&key.as_str()) => {
                        *text = format!("{text} {sample}").trim().to_owned();
                    }
                    Value::Array(items)
                        if LINE_KEYS.contains(&key.as_str())
                            && items.first().is_some_and(Value::is_string) =>
                    {
                        items.push(json!(sample));
                    }
                    other => hostile(other, sample),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| hostile(item, sample)),
        _ => {}
    }
}

/// `event` with `sample` in its text.
fn with_sample(event: &AppEvent, sample: &str) -> AppEvent {
    let mut value = serde_json::to_value(event).expect("an event serializes");
    hostile(&mut value, sample);
    serde_json::from_value(value.clone()).unwrap_or_else(|error| panic!("{error}: {value}"))
}

fn view_data(event: &AppEvent) -> ViewData {
    match event {
        AppEvent::View { data } => (**data).clone(),
        other => panic!("not a view: {other:?}"),
    }
}

fn opened(event: &AppEvent) -> CheckpointOpened {
    match event {
        AppEvent::Run { event } => match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => (**opened).clone(),
            other => panic!("not an opened checkpoint: {other:?}"),
        },
        other => panic!("not a run event: {other:?}"),
    }
}

fn app_choice(event: &AppEvent) -> AppChoice {
    match event {
        AppEvent::Choice { choice } => choice.clone(),
        other => panic!("not a choice: {other:?}"),
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("a fixture parses")
}

fn run_id() -> RunId {
    parse(json!(RUN))
}

fn theme(glyphs: Glyphs) -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = glyphs;
    theme
}

/// A crew run of three agents, with `sample` in its titles and text, folded into a state.
fn crew_state(sample: &str) -> (AppState, Vec<Agent>) {
    let (_, crew, events) = crew_run(3);
    let mut state = AppState::default();
    for event in events {
        state.apply(&with_sample(&event, sample));
    }
    state.apply(&with_sample(&crew_panel_view(sample), sample));
    (state, crew)
}

/// The crew panel's rows from the controller, with `sample` in their text: a conversation, a
/// CLI session, and a run in a repo with a long path.
fn crew_panel_view(sample: &str) -> AppEvent {
    let panel = json!({
        "repo": "~/code/export",
        "rows": [
            {"target": {"kind": "conversation", "chat": "chat-001"}, "state": "ready",
             "title": "How does the export pick a format?", "repo": "~/code/export",
             "worker": "claude-code/opus", "latest": sample, "updated": "2026-09-30T11:00:00Z",
             "details": {"last_message": sample, "prompt": "Pick a format"}},
            {"target": {"kind": "vendor_session", "session": {"vendor": "codex", "id": "019a2f"}},
             "state": "ready", "title": "Explain the input handling", "repo": "~/code/export",
             "worker": "codex/gpt-5.6", "latest": sample, "updated": "2026-09-29T11:00:00Z",
             "details": {"branch": "main"}},
            {"target": {"kind": "run", "run": "run-07"}, "state": "needs_you",
             "title": "Fix the login redirect", "repo": samples::LONG_PATH,
             "run": {"id": "run-07", "title": "Fix the login redirect"},
             "latest": sample, "updated": "2026-09-29T06:00:00Z"}
        ]
    });
    AppEvent::View {
        data: Box::new(ViewData::Crew {
            panel: serde_json::from_value(panel).expect("a crew panel"),
        }),
    }
}

fn context_data(sample: &str) -> Value {
    json!({"rows": [
        {"run": RUN, "session": "s-001", "label": format!("owner · card-002 {sample}"),
         "worker": "claude-code/opus", "used": 42_000, "window": 200_000, "cap": 160_000,
         "tool_calls": 12, "spent": spend(9000, 0.46), "rotations": 1},
        {"run": null, "session": "chat", "label": sample, "worker": "codex/gpt-5.6",
         "used": 0, "window": null, "cap": null, "tool_calls": 0, "spent": spend(0, 0.0),
         "rotations": 0}
    ]})
}

fn approvals_data() -> Value {
    json!({"points": [
        {"kind": "merge", "mode": "supervised", "decide_with": "rules", "matches_in_a_row": 4,
         "misses": 0, "suggest_auto": true},
        {"kind": "plan", "mode": "auto", "decide_with": "rules_and_model",
         "matches_in_a_row": 0, "misses": 2, "suggest_auto": false}
    ], "spot_checks_waiting": 3, "approver": "claude-code/opus"})
}

/// Every full-screen view, filled with `sample`.
fn every_view(
    sample: &str,
    state: &AppState,
    crew: &[Agent],
) -> Vec<(&'static str, Box<dyn View>)> {
    let event = |event: AppEvent| view_data(&with_sample(&event, sample));
    let workflow = match event(workflow_view(
        crew,
        Some(nested_workflow_row("wf-review", "Review loop")),
    )) {
        ViewData::Workflow { view } => *view,
        other => panic!("{other:?}"),
    };
    let agent = match event(agent_view(&crew[0])) {
        ViewData::Agent { view } => *view,
        other => panic!("{other:?}"),
    };
    let runs = vec![
        run_summary(RUN, "Add a --json flag to export", "running", false),
        run_summary("run-02", "Fix the flaky export test", "parked", true),
    ];
    let resume = match event(runs_view(runs.clone())) {
        ViewData::Runs { runs, .. } => runs,
        other => panic!("{other:?}"),
    };
    let diff = match event(diff_view(
        &crew[0],
        "diff --git a/src/export.rs b/src/export.rs\n@@ -1,2 +1,2 @@\n-self.write_csv(out)\n+match self.format {\n",
    )) {
        ViewData::Diff { diff } => diff,
        other => panic!("{other:?}"),
    };
    let raw = match event(raw_view(
        &crew[0],
        &[r#"{"type":"assistant","message":"hi"}"#, sample],
    )) {
        ViewData::Raw { raw } => raw,
        other => panic!("{other:?}"),
    };
    let rows = match event(settings_view(
        "config",
        vec![
            setting_row(
                "ui.theme",
                "\"auto\"",
                "built-in",
                &["\"auto\"", "\"dark\""],
            ),
            setting_row("agents.codex.model", sample, "project", &[]),
            setting_row("narrator.model", "", "built-in", &[]),
        ],
    )) {
        ViewData::Settings { rows, .. } => rows,
        other => panic!("{other:?}"),
    };
    let key: SessionKey = parse(json!(crew[0].session));
    let mention: MentionTarget = parse(json!({"target": "worker", "value": "codex/gpt-5.6"}));
    let mut context: Value = context_data(sample);
    hostile(&mut context, sample);
    vec![
        (
            "workflow",
            Box::new(WorkflowView::new(workflow)) as Box<dyn View>,
        ),
        ("agent", Box::new(AgentView::new(agent))),
        ("context", Box::new(ContextView::new(parse(context)))),
        ("resume", Box::new(ResumePicker::new(resume))),
        ("diff", Box::new(Pager::diff(diff))),
        ("raw", Box::new(Pager::raw(raw))),
        (
            "pager",
            Box::new(Pager::text(
                format!("Notes {sample}"),
                vec![sample.to_owned(), String::new(), "done".to_owned()],
            )),
        ),
        ("crew", Box::new(CrewView::new(state))),
        (
            "crew details",
            Box::new(crew_view(state, |view| view.details = true)),
        ),
        (
            "crew keys",
            Box::new(crew_view(state, |view| view.keys = true)),
        ),
        ("help", Box::new(HelpView::new(Keymap::default()))),
        (
            "help with the CLI's commands",
            Box::new(HelpView::new(Keymap::default()).with_commands(Some(cli_commands(sample)))),
        ),
        ("session", Box::new(SessionView::new(run_id(), key))),
        ("config", Box::new(ConfigView::new(rows))),
        (
            "approvals",
            Box::new(ApprovalsView::new(parse(approvals_data()))),
        ),
        (
            "mention",
            Box::new(MentionPicker::new(
                format!("codex{sample}"),
                sample,
                vec![
                    (format!("codex {sample}"), mention.clone()),
                    ("codex".into(), mention),
                ],
            )),
        ),
    ]
}

fn crew_view(state: &AppState, set: impl Fn(&mut CrewView)) -> CrewView {
    let mut view = CrewView::new(state);
    set(&mut view);
    view
}

fn draw(view: &mut dyn View, width: u16, state: &AppState, theme: &Theme) -> Buffer {
    let area = Rect::new(0, 0, width, HEIGHT);
    let mut buf = Buffer::empty(area);
    let ctx = ViewCtx {
        theme,
        state,
        now: Instant::now(),
    };
    view.render(area, &mut buf, &ctx);
    buf
}

/// Every card and what it puts in the conversation, filled with `sample`, drawn `width` wide.
fn every_card(sample: &str, width: u16, theme: &Theme) -> Vec<(String, Vec<Line<'static>>)> {
    let mut run = RunScript::new(RUN);
    let agent = Agent::numbered(1);
    let run_state = RunState::new(run_id());
    let ctx = RenderCtx::new(width, theme, Clock::still(Instant::now()));
    let mut cards = Vec::new();
    let checkpoints = [
        ("merge", run.merge_checkpoint("cp-001")),
        ("question", run.question_checkpoint("cp-002", &agent)),
        ("plan", run.plan_checkpoint("cp-003")),
        ("take over return", run.take_over_return("cp-004", &agent)),
    ];
    for (name, event) in checkpoints {
        let opened = opened(&with_sample(&event, sample));
        let mut lines = Vec::new();
        for cell in about_cells(&opened, Some(&run_state)) {
            lines.extend(cell.lines(&ctx));
        }
        let mut prompt = checkpoint_card(1, &opened, Some(&run_state));
        prompt.focus();
        lines.extend(prompt.lines(&ctx));
        cards.push((format!("{name} card"), lines));
        let spot = spot_check_card(2, &opened, None, Some(&run_id()), &Default::default());
        cards.push((format!("{name} spot check"), spot.lines(&ctx)));
    }
    let choices = [
        ("start work", start_work_choice("ch-001")),
        (
            "scope",
            scope_choice("ch-002", "Save theme where?", "global", Some(sample)),
        ),
    ];
    for (name, event) in choices {
        let choice = app_choice(&with_sample(&event, sample));
        let mut prompt = choice_card(3, &choice);
        prompt.focus();
        cards.push((format!("{name} choice"), prompt.lines(&ctx)));
    }
    let card: CardId = parse(json!(format!("card-002")));
    let agent_name = format!("claude-code/opus {sample}");
    cards.push((
        "take over".into(),
        confirm_take_over(4, &run_id(), &card, &agent_name).lines(&ctx),
    ));
    let working = WorkingLine::new(format!("Checking {sample}"), Instant::now());
    cards.push(("working line".into(), vec![working.line(&ctx)]));
    cards
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

fn setting(key: &str, value: &str, allowed: &[String], description: &str) -> SettingRow {
    SettingRow {
        key: key.to_owned(),
        value: value.to_owned(),
        layer: "project".to_owned(),
        preset_from: None,
        default_scope: SettingScope::Global,
        safety: false,
        allowed: allowed.to_vec(),
        description: description.to_owned(),
    }
}

fn model(worker: &str, sample: &str, current: bool) -> ModelChoice {
    ModelChoice {
        worker: worker.parse().expect("a worker"),
        family: sample.to_owned(),
        access: ModelAccess::ApiKey,
        cost: CostEstimate::Usd(0.38),
        quota: None,
        efforts: Vec::new(),
        strengths: vec![sample.to_owned(), "tests".to_owned()],
        current,
        note: Some(format!("newest {sample}")),
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

/// Every kind of picker, with `sample` in its values, descriptions, and warnings.
fn every_picker(sample: &str) -> Vec<(&'static str, Picker)> {
    let value = json!(sample).to_string();
    let theme_row = setting(
        "ui.theme",
        "\"dark\"",
        &["\"auto\"".into(), "\"dark\"".into(), value.clone()],
        &format!("Light or dark colors {sample}"),
    );
    let values = flow::settings_picker("theme", vec![theme_row]);
    // The third value is the sample, and the project file sets the key, so it asks where.
    let mut scope = values.clone();
    let event = scope.handle_key(key(KeyCode::Char('3')));
    assert_eq!(flow::advance(&mut scope, event), Step::Stay);
    let keys = flow::settings_picker(
        "ui",
        vec![
            setting(
                "ui.theme",
                &value,
                &["\"auto\"".into(), value.clone()],
                sample,
            ),
            setting("narrator.model", &value, &[], sample),
            setting("ui.glyphs", "\"unicode\"", &[], ""),
        ],
    );
    let typed = flow::edit_picker(setting("agents.codex.model", &value, &[], sample));
    let models = flow::models_picker(
        "model reviewer",
        Some(RoleName::Reviewer),
        &[
            model("claude-code/opus", sample, true),
            ModelChoice {
                older: true,
                ..model("claude-code/claude-opus-4-8", sample, false)
            },
            model("codex/gpt-5.6", sample, false),
        ],
    );
    let mut model_list = models.clone();
    let event = model_list.handle_key(key(KeyCode::Enter));
    assert_eq!(flow::advance(&mut model_list, event), Step::Stay);
    let mut other_model = model_list.clone();
    let event = other_model.handle_key(key(KeyCode::End));
    flow::advance(&mut other_model, event);
    let event = other_model.handle_key(key(KeyCode::Enter));
    assert_eq!(flow::advance(&mut other_model, event), Step::Stay);
    let mut rows = Picker::new(format!("Pick {sample}"), sample);
    rows.set_rows(vec![
        PickerRow {
            current: true,
            key: Some('a'),
            warning: Some(format!("It won't change {sample}")),
            ..PickerRow::new(sample, sample)
        },
        PickerRow {
            heading: Some(format!("Older {sample}")),
            ..PickerRow::new(format!("second {sample}"), "")
        },
        PickerRow {
            reset: true,
            ..PickerRow::new("Reset", format!("remove it {sample}"))
        },
    ]);
    rows.warning = Some(format!("Refused {sample}"));
    let mut empty = Picker::new(sample, "");
    empty.placeholder = format!("Nothing yet {sample}");
    vec![
        ("values picker", values),
        ("scope question", scope),
        ("keys picker", keys),
        ("typed value picker", typed),
        ("models picker", models),
        ("model list picker", model_list),
        ("other model picker", other_model),
        ("picker rows", rows),
        ("empty picker", empty),
    ]
}

/// The live region with a card in each state it can take, and with a picker open, filled
/// with `sample`.
fn every_bottom(sample: &str) -> Vec<(&'static str, BottomState)> {
    let mut run = RunScript::new(RUN);
    let agent = Agent::numbered(1);
    let run_state = RunState::new(run_id());
    let card = |event: AppEvent| -> CardPrompt {
        let opened = opened(&with_sample(&event, sample));
        let mut card = checkpoint_card(1, &opened, Some(&run_state));
        card.focus();
        card
    };
    let merge = card(run.merge_checkpoint("cp-001"));
    let question = card(run.question_checkpoint("cp-002", &agent));
    let option_of = |card: &CardPrompt, kind: OptionKind| {
        card.options
            .iter()
            .position(|option| option.kind == kind)
            .unwrap_or_else(|| panic!("a {kind:?} option on {}", card.title))
    };
    let number = |option: usize| char::from_digit(option as u32 + 1, 10).expect("one digit");

    // Tab on an option, then a pasted note.
    let mut noted = merge.clone();
    noted.allow_note = true;
    noted.handle_key(key(KeyCode::Down));
    noted.handle_key(key(KeyCode::Tab));
    assert!(noted.handle_paste(sample), "the note is open");
    // The checklist open with some rows ticked and one more row.
    let mut checklist = merge.clone();
    let option = option_of(&checklist, OptionKind::Checklist);
    checklist.options[option].items.push(sample.to_owned());
    checklist.handle_key(key(KeyCode::Char(number(option))));
    let open = checklist.checklist.as_mut().expect("the checklist opens");
    open.checked
        .iter_mut()
        .step_by(2)
        .for_each(|row| *row = true);
    open.cursor = open.checked.len() - 1;
    // Your own answer typed in.
    let mut answer = question.clone();
    let option = option_of(&answer, OptionKind::FreeText);
    answer.handle_key(key(KeyCode::Char(number(option))));
    assert!(answer.handle_paste(sample), "the answer line is open");
    // An irreversible option waiting for its second press.
    let mut armed = merge.clone();
    let option = option_of(&armed, OptionKind::Irreversible);
    armed.handle_key(key(KeyCode::Char(number(option))));
    let mut pinned = merge.clone();
    pinned.pinned = true;
    pinned.focused = false;
    let mut waiting = merge;
    waiting.focused = false;

    let idle = || {
        let mut bottom = BottomState::new();
        bottom.composer.label = Some(format!("peer-crew {sample}"));
        bottom
    };
    let with_card = |card: CardPrompt| {
        let mut bottom = idle();
        bottom.card = Some(card);
        bottom
    };
    let mut under_work = with_card(waiting);
    under_work.working = Some(WorkingLine::new(
        format!("Building {sample}"),
        Instant::now(),
    ));
    under_work.run_takes_text = true;
    let mut picking = idle();
    picking.composer.set_text(&format!("/theme {sample}"));
    picking.picker = every_picker(sample)
        .into_iter()
        .find(|(name, _)| *name == "scope question")
        .map(|(_, picker)| picker);
    vec![
        ("card with a note", with_card(noted)),
        ("card checklist", with_card(checklist)),
        ("card own answer", with_card(answer)),
        ("card armed", with_card(armed)),
        ("card pinned", with_card(pinned)),
        ("card waiting", under_work),
        ("picker open", picking),
    ]
}

/// The history a crew run and a chat turn leave, with `sample` in their text.
fn transcript_model(sample: &str) -> ViewModel {
    let (mut run, crew, mut events) = crew_run(3);
    events.push(run.message(&crew[0], &format!("Wrote the writer {sample}")));
    events.push(run.note("warn", sample));
    events.push(turn_started("t-001", sample));
    events.push(chat_tool_started(
        "t-001",
        "tool-1",
        "Read",
        json!({"file_path": samples::LONG_PATH}),
    ));
    events.push(chat_tool_finished("t-001", "tool-1", sample));
    events.push(turn_finished("t-001", &format!("Done. {sample}"), 3_000));
    events.push(notice("warn", sample));
    let mut model = ViewModel::default();
    let mut state = AppState::default();
    let settings = UiSettings::default();
    let mut transcript = Transcript::default();
    for event in events {
        let event = with_sample(&event, sample);
        state.apply(&event);
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut model,
            state: &state,
            settings: &settings,
            now: Instant::now(),
            out: &mut out,
        };
        transcript.on_event(&event, &mut cx);
    }
    model
}

/// The Ctrl+O transcript, and the overlay a card opens to show what it asks about.
fn every_overlay(sample: &str) -> Vec<(&'static str, TranscriptOverlay)> {
    let mut run = RunScript::new(RUN);
    let opened = opened(&with_sample(&run.merge_checkpoint("cp-001"), sample));
    let cells = about_cells(&opened, Some(&RunState::new(run_id())));
    vec![
        ("transcript overlay", TranscriptOverlay::new()),
        (
            "card overlay",
            TranscriptOverlay::view(format!("the diff {sample}"), cells),
        ),
    ]
}

/// Rows drawn inline must fit and line up, named for the failure.
fn assert_inline(name: &str, lines: &[Line<'_>], width: u16, sample: &str) {
    let issues = check_lines(lines, width);
    assert!(
        issues.is_empty(),
        "{name} at {width} columns with {:?}: {issues:?}\n{}",
        sample.chars().take(24).collect::<String>(),
        plain_text(lines)
    );
}

/// A main agent's commands whose descriptions and names carry `sample`.
fn cli_commands(sample: &str) -> argonauts_core::vendor_commands::VendorCommandSet {
    use argonauts_core::names::VendorId;
    use argonauts_core::vendor_commands::{
        BuiltinCommand, BuiltinKind, CommandKind, CommandScope, CommandSource, VendorCommand,
        VendorCommandSet,
    };
    const TABLE: &[BuiltinCommand] = &[
        BuiltinCommand {
            name: "batch",
            aliases: &[],
            args: "<instruction>",
            help: "One change across many files, in parallel",
            kind: BuiltinKind::Vendor,
        },
        BuiltinCommand {
            name: "voice",
            aliases: &[],
            args: "[hold|tap|off]",
            help: "Talk instead of typing",
            kind: BuiltinKind::ScreenOnly,
        },
    ];
    let custom = VendorCommand {
        name: format!("ship-{}", sample.split_whitespace().next().unwrap_or("x")),
        aliases: Vec::new(),
        args: "[branch]".into(),
        help: format!("Ship it {sample}"),
        kind: CommandKind::Vendor,
        source: CommandSource::Custom {
            scope: CommandScope::Project,
            path: "/repo/.claude/commands/ship.md".into(),
        },
    };
    VendorCommandSet::new(
        VendorId("claude-code".into()),
        "Claude Code",
        TABLE,
        vec![custom],
    )
}

#[test]
fn frame_checker_every_view_40_to_200_columns() {
    let theme = theme(Glyphs::UNICODE);
    for sample in samples::all() {
        let (state, crew) = crew_state(&sample);
        for (name, mut view) in every_view(&sample, &state, &crew) {
            // Wide to narrow and back, the way a resize goes.
            for width in WIDTHS.iter().rev().chain(WIDTHS.iter()) {
                let buf = draw(view.as_mut(), *width, &state, &theme);
                let result = std::panic::catch_unwind(|| assert_frames(&buf));
                assert!(
                    result.is_ok(),
                    "{name} at {width} columns with {:?}",
                    sample.chars().take(24).collect::<String>()
                );
            }
        }
    }
}

#[test]
fn frames_aligned_all_views() {
    let theme = theme(Glyphs::UNICODE);
    for sample in samples::all() {
        for width in WIDTHS {
            for (name, lines) in every_card(&sample, width, &theme) {
                let result = std::panic::catch_unwind(|| assert_lines(&lines, width));
                assert!(
                    result.is_ok(),
                    "{name} at {width} columns with {:?}",
                    sample.chars().take(24).collect::<String>()
                );
                assert!(!lines.is_empty(), "{name} draws something");
            }
        }
        // Every view's rows line up on the screen as the checker reads them, at each width.
        let (state, crew) = crew_state(&sample);
        for (_, mut view) in every_view(&sample, &state, &crew) {
            for width in WIDTHS {
                let buf = draw(view.as_mut(), width, &state, &theme);
                assert_eq!(buf.area.width, width);
                assert_frames(&buf);
            }
        }
        // Pickers, the live region, and the overlays keep one instance across the widths too.
        let still = |width| RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        let pickers = every_picker(&sample);
        let bottoms = every_bottom(&sample);
        let model = transcript_model(&sample);
        assert!(!model.history.cells().is_empty(), "the transcript has rows");
        let overlays = every_overlay(&sample);
        for width in WIDTHS.iter().rev().chain(WIDTHS.iter()).copied() {
            let ctx = still(width);
            for (name, picker) in &pickers {
                assert_inline(name, &picker.lines(&ctx), width, &sample);
            }
            for (name, bottom) in &bottoms {
                let view = bottom.render(&ctx);
                assert!(!view.lines.is_empty(), "{name} draws something");
                assert_inline(name, &view.lines, width, &sample);
            }
            for (name, overlay) in &overlays {
                let lines = overlay.render(&model.history, &ctx, HEIGHT);
                assert_eq!(lines.len(), usize::from(HEIGHT), "{name} fills the screen");
                assert_inline(name, &lines, width, &sample);
            }
        }
    }
}

fn glyph_table(glyphs: &Glyphs) -> String {
    glyphs
        .named()
        .iter()
        .map(|(name, glyph)| {
            format!(
                "{name:<16} {glyph:<8} {} {}",
                width_with(glyph, 1),
                width_with(glyph, 2)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ascii_glyphs_and_ambiguous_width() {
    insta::assert_snapshot!("unicode_glyphs", glyph_table(&Glyphs::UNICODE));
    insta::assert_snapshot!("ascii_glyphs", glyph_table(&Glyphs::ASCII));
    // Where ambiguous glyphs take two cells, the ASCII set is the one in use.
    for choice in [GlyphChoice::Unicode, GlyphChoice::Ascii] {
        assert_eq!(Glyphs::for_settings(choice, 2), Glyphs::ASCII);
    }
    for (name, glyph) in Glyphs::ASCII.named() {
        assert_eq!(width_with(glyph, 1), width_with(glyph, 2), "{name}");
    }

    // With it, no frame or card holds a glyph whose width depends on the terminal.
    let theme = theme(Glyphs::for_settings(GlyphChoice::Unicode, 2));
    let mut found = Vec::new();
    for sample in [samples::EMPTY.to_owned(), samples::CJK.to_owned()] {
        let (state, crew) = crew_state(&sample);
        for (name, mut view) in every_view(&sample, &state, &crew) {
            for width in [40, 120] {
                let buf = draw(view.as_mut(), width, &state, &theme);
                let mut issues = check_glyphs(&buf, &UNSTABLE_GLYPHS);
                issues.extend(check(&buf));
                if !issues.is_empty() {
                    found.push(format!(
                        "{name} at {width}: {issues:?}\n{}",
                        buffer_text(&buf)
                    ));
                }
            }
        }
        for width in [40, 120] {
            for (name, lines) in every_card(&sample, width, &theme) {
                let issues = check_line_glyphs(&lines, &UNSTABLE_GLYPHS);
                if !issues.is_empty() {
                    found.push(format!("{name} at {width}: {issues:?}"));
                }
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}
