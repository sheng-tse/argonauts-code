//! `/model`'s pictures, drawn from their own example at their width of 118 columns and compared
//! cell for cell with `tests/golden/<name>.txt`, and their colours with the `.html`: the picker
//! in the input's place, OpenCode's level listing what OpenCode can run, and a model with
//! levels beside one with none.
//!
//! The example is OpenCode 2.0.32's `model.list` on a Mac with Ollama: ten free OpenCode Zen
//! models and seven Ollama tags, three of them with no tool use, and Nemotron 3.5 Super VL Free
//! as what Default runs. A picture's captions are prose around its frames, so only the frames
//! are compared, each by its rows.

use std::time::Instant;

use argonauts_core::app::{ModelChoice, SettingRow};
use argonauts_core::limits::{CostEstimate, PlanUsageReading, PlanUsageSource, PlanUsageStatus};
use argonauts_core::model_picker::{Fit, ListedFacts, MachineMemory, ModelMemory, ModelSource};
use argonauts_core::names::AgentId;
use argonauts_core::registry::ModelAccess;
use argonauts_core::settings::SettingScope;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use time::OffsetDateTime;

use super::flow;
use crate::bottom::{BottomState, Picker};
use crate::bridge::transcript::picture_check::{Unlike, assert_rows_match};
use crate::render::{Clock, RenderCtx};
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

const WIDTH: u16 = 118;
const FRESH: &str = "Model lists refreshed just now.";

/// A picture's rows without the blanks that pad them to its width, and its colours, with the
/// question colour the pictures write as its value named as the role it stands for.
fn picture(txt: &'static str, html: &'static str) -> (String, String) {
    let rows: Vec<&str> = txt.lines().map(str::trim_end).collect();
    (
        rows.join("\n"),
        html.replace("color:#b1b9f9", "color:var(--term-acc)"),
    )
}

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The picker as the bottom of the screen draws it, in the input's place.
fn drawn(picker: &Picker) -> Vec<Line<'static>> {
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let mut bottom = BottomState::new();
    bottom.composer.label = Some("Opus 5.5".to_owned());
    bottom.picker = Some(picker.clone());
    bottom.render(&ctx).lines
}

fn compare(
    name: &str,
    (txt, html): &(String, String),
    rows: std::ops::Range<usize>,
    lines: &[Line<'static>],
) {
    assert_rows_match(
        name,
        lines,
        (txt, html),
        rows,
        &theme(),
        &gemma_unlike(lines),
    );
}

/// `gemma4:e2b` reads "Gemma4 E2B" now, where the pictures still show the rule they fix.
fn gemma_unlike(lines: &[Line<'static>]) -> Vec<Unlike> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| {
            let text = plain_text(std::slice::from_ref(line));
            let col = text.find("Gemma4 E2B")?;
            let col = text[..col].chars().count() + "Gemma4 E2".len();
            Some(Unlike {
                row,
                cols: (col, col),
                why: "a size with a letter before it takes capitals, the fix the picture names",
            })
        })
        .collect()
}

fn key(picker: &mut Picker, code: KeyCode) {
    let event = picker.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    let _ = flow::advance(picker, event);
}

fn choice(worker: &str, access: ModelAccess, cost: CostEstimate) -> ModelChoice {
    ModelChoice {
        worker: worker.parse().expect("a worker"),
        provider: String::new(),
        access,
        cost,
        plan_usage: None,
        efforts: Vec::new(),
        strengths: Vec::new(),
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
        listed: None,
    }
}

fn used(agent: &str, share: f64) -> Option<PlanUsageReading> {
    Some(PlanUsageReading {
        agent_id: AgentId(agent.to_owned()),
        status: PlanUsageStatus::Allowed,
        used: Some(share),
        resets_at: None,
        window: Some("five_hour".into()),
        source: PlanUsageSource::RateLimitEvent,
        observed_at: OffsetDateTime::UNIX_EPOCH,
        detail: None,
        wrap_up: false,
    })
}

/// One OpenCode model as `model.list` gives it: its route, OpenCode's name ("" for a tag
/// OpenCode only found), its window, its variants, and whether it has tool use.
struct Listed {
    route: &'static str,
    name: &'static str,
    window: u64,
    variants: &'static [&'static str],
    tools: bool,
}

const fn zen(
    route: &'static str,
    name: &'static str,
    window: u64,
    variants: &'static [&'static str],
) -> Listed {
    Listed {
        route,
        name,
        window,
        variants,
        tools: true,
    }
}

const fn tag(route: &'static str, name: &'static str, window: u64, tools: bool) -> Listed {
    Listed {
        route,
        name,
        window,
        variants: &[],
        tools,
    }
}

const NEMOTRON_VL: &str = "opencode/nemotron-3.5-super-vl-free";
const NEMOTRON_VL_LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];
const UP_TO_MAX: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const THREE: &[&str] = &["low", "high", "max"];

/// OpenCode's models in the order the controller sends them: Ollama's with tool use first,
/// then by name, those without last; then OpenCode Zen's, newest first. gemma3:12b-32k's
/// window is the 32,768 its tag holds, under OpenCode's 131,072.
const OPENCODE: &[Listed] = &[
    tag("ollama/gemma4:e2b", "", 131_072, true),
    tag("ollama/llama3.2:latest", "", 131_072, true),
    tag("ollama/qwen3-coder:30b", "", 262_144, true),
    tag(
        "ollama/qwen3-coder:30b-64k",
        "Qwen3 Coder 30B (64k)",
        65_536,
        true,
    ),
    tag("ollama/gemma2:9b", "", 8_192, false),
    tag("ollama/gemma3:12b", "", 131_072, false),
    tag("ollama/gemma3:12b-32k", "", 32_768, false),
    zen(
        NEMOTRON_VL,
        "Nemotron 3.5 Super VL Free",
        1_048_576,
        NEMOTRON_VL_LEVELS,
    ),
    zen("opencode/beam-free", "Beam Free", 1_048_576, UP_TO_MAX),
    zen(
        "opencode/longcat-2.5-preview-free",
        "LongCat 2.5 Preview Free",
        1_000_000,
        &[],
    ),
    zen(
        "opencode/space-bunny-free",
        "Space Bunny Free",
        1_048_576,
        UP_TO_MAX,
    ),
    zen(
        "opencode/mimo-v2.6-flash-free",
        "MiMo-V2.6-Flash Free",
        200_000,
        &[],
    ),
    zen(
        "opencode/deepseek-v4.1-flash-free",
        "DeepSeek V4.1 Flash Free",
        200_000,
        THREE,
    ),
    zen(
        "opencode/glm-5.3-flash-free",
        "GLM-5.3-Flash Free",
        200_000,
        THREE,
    ),
    zen(
        "opencode/nemotron-3.5-lightning-free",
        "Nemotron 3.5 Lightning Free",
        262_144,
        &[],
    ),
    zen(
        "opencode/nemotron-3-ultra-free",
        "Nemotron 3 Ultra Free",
        1_000_000,
        &[],
    ),
    zen("opencode/big-pickle", "Big Pickle", 200_000, &[]),
];

fn levels(names: &[&str]) -> Vec<String> {
    names.iter().map(|&name| name.to_owned()).collect()
}

fn opencode_choice(listed: &Listed) -> ModelChoice {
    let (provider, _) = listed.route.split_once('/').expect("a route");
    let local = provider == "ollama";
    let (access, group, hosted_by) = if local {
        (ModelAccess::Local, "Ollama", None)
    } else {
        (
            ModelAccess::Subscription,
            "OpenCode Zen",
            Some("OpenCode Zen".to_owned()),
        )
    };
    let mut choice = choice(
        &format!("opencode/{}", listed.route),
        access,
        CostEstimate::Free,
    );
    choice.name = listed.name.to_owned();
    choice.efforts = levels(listed.variants);
    choice.listed = Some(ListedFacts {
        group: Some(group.to_owned()),
        window: Some(listed.window),
        no_tools: !listed.tools,
        thinking: false,
        hosted_by,
        memory: None,
    });
    choice
}

/// Default, running Nemotron 3.5 Super VL Free, then the seventeen.
fn opencode_choices() -> Vec<ModelChoice> {
    let mut default = choice("opencode", ModelAccess::Subscription, CostEstimate::Unknown);
    default.runs = Some("Nemotron 3.5 Super VL Free".to_owned());
    default.efforts = levels(NEMOTRON_VL_LEVELS);
    std::iter::once(default)
        .chain(OPENCODE.iter().map(opencode_choice))
        .collect()
}

/// The CLIs as the picture's screen has them: Claude Code current on its default, Codex's seven
/// models, agy's six, and OpenCode's.
fn every_choice() -> Vec<ModelChoice> {
    let mut claude = choice(
        "claude-code",
        ModelAccess::Subscription,
        CostEstimate::PlanUsage(0.04),
    );
    claude.current = true;
    claude.runs = Some("Opus 5.5".to_owned());
    claude.plan_usage = used("claude-code", 0.92);
    let models = |cli: &str, count: usize, share: f64| -> Vec<ModelChoice> {
        (1..=count)
            .map(|n| {
                let mut model = choice(
                    &format!("{cli}/model-{n}"),
                    ModelAccess::Subscription,
                    CostEstimate::PlanUsage(0.01),
                );
                model.plan_usage = used(cli, share);
                model
            })
            .collect()
    };
    std::iter::once(claude)
        .chain(models("codex", 7, 0.02))
        .chain(models("antigravity-cli", 6, 0.0))
        .chain(opencode_choices())
        .collect()
}

fn setting(key: &str, value: &str) -> SettingRow {
    SettingRow {
        key: key.to_owned(),
        value: value.to_owned(),
        layer: "global".to_owned(),
        workflow_from: None,
        default_scope: SettingScope::Global,
        safety: false,
        allowed: Vec::new(),
        description: String::new(),
    }
}

/// `/model` as it opens for the main agent, with vim keys on as the picture has them.
fn model_picker(choices: &[ModelChoice], freshness: &str) -> Picker {
    let mut picker = Picker {
        vim: true,
        ..Picker::default()
    };
    picker.flow.command = "model".to_owned();
    picker.flow.settings = vec![
        setting("crew.main", "\"claude-code\""),
        setting("roles.reviewer.models", "[\"codex/model-1\"]"),
    ];
    flow::fill_models(&mut picker, None, choices);
    flow::show_freshness(&mut picker, Some(freshness));
    picker
}

/// Enter on OpenCode's row of the first level.
fn opencode_level(choices: &[ModelChoice], freshness: &str) -> Picker {
    let mut picker = model_picker(choices, freshness);
    key(&mut picker, KeyCode::Char('4'));
    assert_eq!(picker.title, "OpenCode");
    picker
}

fn pointed_label(picker: &Picker) -> &str {
    let index = picker.pointed().expect("a row under the pointer");
    &picker.rows[index].label
}

/// Down with `j` until the pointer is on `label`.
fn down_to(picker: &mut Picker, label: &str) {
    for _ in 0..picker.rows.len() {
        if pointed_label(picker) == label {
            return;
        }
        key(picker, KeyCode::Char('j'));
    }
    panic!("no row {label:?}");
}

#[test]
fn the_picker_takes_the_inputs_place_with_opencode_counted() {
    let picture = picture(
        include_str!("../../../tests/golden/model_picker_after.txt"),
        include_str!("../../../tests/golden/model_picker_after.html"),
    );
    let picker = model_picker(&every_choice(), FRESH);
    let lines = drawn(&picker);
    // The picture's rule is row 23; the rows above it are the conversation's.
    compare("model_picker_after", &picture, 22..33, &lines);
}

#[test]
fn opencodes_level_lists_what_opencode_can_run_in_its_groups() {
    let picture = picture(
        include_str!("../../../tests/golden/opencode_models_after.txt"),
        include_str!("../../../tests/golden/opencode_models_after.html"),
    );
    let mut picker = opencode_level(&every_choice(), FRESH);
    down_to(&mut picker, "Qwen3 Coder 30B (64k)");
    compare("opencode_models_after", &picture, 2..20, &drawn(&picker));

    // The same list scrolled to its end.
    key(&mut picker, KeyCode::Char('G'));
    assert_eq!(pointed_label(&picker), "Other model…");
    compare("opencode_models_after", &picture, 23..37, &drawn(&picker));
}

#[test]
fn a_model_offers_its_own_levels_and_one_with_none_says_so() {
    let picture = picture(
        include_str!("../../../tests/golden/opencode_effort_after.txt"),
        include_str!("../../../tests/golden/opencode_effort_after.html"),
    );
    let mut picker = opencode_level(&every_choice(), FRESH);
    down_to(&mut picker, "DeepSeek V4.1 Flash Free");
    // Up from the CLI's own effort to max, then back to high, among low, high and max.
    key(&mut picker, KeyCode::Char('l'));
    key(&mut picker, KeyCode::Char('h'));
    let line = picker.flow.effort.clone().expect("the effort line");
    assert_eq!(line.levels, ["low", "high", "max"]);
    assert_eq!(line.level(), Some("high"));
    compare("opencode_effort_after", &picture, 0..18, &drawn(&picker));

    // Nemotron 3 Ultra Free has no variants: Ultra is its size.
    down_to(&mut picker, "Nemotron 3 Ultra Free");
    let lines = drawn(&picker);
    let none = lines
        .iter()
        .find(|line| plain_text(std::slice::from_ref(line)).contains("Effort not supported"))
        .cloned()
        .expect("the line for a model with no levels");
    compare("opencode_effort_after", &picture, 22..23, &[none]);
}

/// Typing matches a model's id too, and the group headings hide while a filter is typed.
#[test]
fn typing_an_id_finds_its_row_and_hides_the_headings() {
    let headings = |picker: &Picker| -> Vec<String> {
        drawn(picker)
            .iter()
            .map(|line| plain_text(std::slice::from_ref(line)).trim().to_owned())
            .filter(|text| text == "Ollama" || text == "OpenCode Zen")
            .collect()
    };
    let mut picker = opencode_level(&every_choice(), FRESH);
    assert_eq!(headings(&picker), ["Ollama"], "the first rows' heading");
    picker.filter = "qwen3-coder:30b".to_owned();
    let mut shown: Vec<&str> = picker
        .shown()
        .into_iter()
        .map(|index| picker.rows[index].label.as_str())
        .collect();
    shown.sort_unstable();
    assert_eq!(shown, ["Qwen3 Coder 30B", "Qwen3 Coder 30B (64k)"]);
    assert_eq!(headings(&picker), Vec::<String>::new());
}

/// The lists were read at 21:40 and `/model` opens at 21:43, in UTC here.
fn at(hour: u8, minute: u8) -> OffsetDateTime {
    time::macros::datetime!(2026-10-18 00:00 UTC)
        .replace_time(time::Time::from_hms(hour, minute, 0).expect("a time"))
}

/// Codex, Antigravity CLI and OpenCode, the CLIs that list their models by running, each
/// read at `listed`, and the ones in `asking` being asked again.
fn sources(listed: OffsetDateTime, asking: &[&str], failed: &[&str]) -> Vec<ModelSource> {
    ["codex", "antigravity-cli", "opencode"]
        .into_iter()
        .map(|cli| ModelSource {
            cli: cli.to_owned(),
            listed: Some(listed),
            asking: asking.contains(&cli),
            failed: failed.contains(&cli),
            note: None,
        })
        .collect()
}

/// What the controller tells the picker of its sources at `now`.
fn told(picker: &mut Picker, sources: Vec<ModelSource>, now: OffsetDateTime) {
    picker.flow.sources = sources;
    picker.flow.told_at = Some((now, time::UtcOffset::UTC));
}

/// `/model` as it opens at 21:43 from the lists read at 21:40.
fn opened_from_the_last_lists(choices: &[ModelChoice], asking: &[&str]) -> Picker {
    let mut picker = model_picker(choices, FRESH);
    told(&mut picker, sources(at(21, 40), asking, &[]), at(21, 43));
    flow::fill_models(&mut picker, None, choices);
    picker
}

/// devstral:24b, pulled since 21:40, as OpenCode names a tag it found itself.
fn with_devstral(mut choices: Vec<ModelChoice>) -> Vec<ModelChoice> {
    let devstral = opencode_choice(&tag("ollama/devstral:24b", "", 131_072, true));
    let at = choices
        .iter()
        .position(|choice| choice.worker.to_string() == "opencode/ollama/gemma4:e2b")
        .expect("the first Ollama model");
    choices.insert(at, devstral);
    choices
}

#[test]
fn the_first_level_says_which_lists_are_being_asked_and_takes_each_in_place() {
    let picture = picture(
        include_str!("../../../tests/golden/model_picker_live_top.txt"),
        include_str!("../../../tests/golden/model_picker_live_top.html"),
    );
    let asked = ["codex", "antigravity-cli", "opencode"];
    let mut picker = opened_from_the_last_lists(&every_choice(), &asked);
    compare("model_picker_live_top", &picture, 5..16, &drawn(&picker));

    // About 8 s later every answer is in, OpenCode has the model pulled since, and Claude
    // Code's usage moved on.
    let mut answered = with_devstral(every_choice());
    answered[0].plan_usage = used("claude-code", 0.93);
    told(&mut picker, sources(at(21, 43), &[], &[]), at(21, 43));
    super::models::refresh(&mut picker, &answered);
    assert_eq!(pointed_label(&picker), "Claude Code");
    compare("model_picker_live_top", &picture, 20..31, &drawn(&picker));
}

#[test]
fn opencodes_level_keeps_the_pointer_on_its_model_as_a_new_one_comes_in() {
    let picture = picture(
        include_str!("../../../tests/golden/model_picker_live_opencode.txt"),
        include_str!("../../../tests/golden/model_picker_live_opencode.html"),
    );
    let asked = ["codex", "antigravity-cli", "opencode"];
    let mut picker = opened_from_the_last_lists(&every_choice(), &asked);
    key(&mut picker, KeyCode::Char('4'));
    down_to(&mut picker, "Qwen3 Coder 30B (64k)");
    compare(
        "model_picker_live_opencode",
        &picture,
        5..23,
        &drawn(&picker),
    );

    // OpenCode answered: Devstral 24B comes in as new, and the pointer, the effort line and
    // the window stay on Qwen3 Coder 30B (64k), now row 6.
    told(&mut picker, sources(at(21, 43), &[], &[]), at(21, 43));
    super::models::refresh(&mut picker, &with_devstral(every_choice()));
    assert_eq!(pointed_label(&picker), "Qwen3 Coder 30B (64k)");
    assert_eq!((picker.selected, picker.offset), (5, 0));
    compare(
        "model_picker_live_opencode",
        &picture,
        27..45,
        &drawn(&picker),
    );

    // Had the server listed nothing for 15 s, the rows would stay under this line.
    told(
        &mut picker,
        sources(at(21, 40), &[], &["opencode"]),
        at(21, 43),
    );
    super::models::refresh(&mut picker, &every_choice());
    let about = drawn(&picker)[2].clone();
    compare("model_picker_live_opencode", &picture, 48..49, &[about]);
}

const GIB: u64 = 1 << 30;

/// The pictured Mac: Ollama gives models up to 52 GB of its 64 GB, the loaded Qwen3 Coder 30B
/// (64k) holds 25 GB, and 18 GB is free beside the other apps.
const MAC_64: MachineMemory = MachineMemory {
    limit: 52 * GIB,
    total: 64 * GIB,
    in_use: 25 * GIB,
    free: 18 * GIB,
    mac: true,
};

/// What each local tag takes at its window, in GB, and whether it thinks.
const LOCAL_RAM: &[(&str, u64, bool)] = &[
    ("ollama/qwen3-coder:30b-64k", 25, false),
    ("ollama/gemma4:e2b", 8, true),
    ("ollama/llama3.2:latest", 24, false),
    ("ollama/qwen3-coder:30b", 45, false),
    ("ollama/gemma2:9b", 6, false),
    ("ollama/gemma3:12b", 17, false),
    ("ollama/gemma3:12b-32k", 11, false),
];

/// A model not installed from the short list kept with the catalog: its route, name,
/// window, RAM there and download size in GB, and whether it thinks.
fn suggestion(
    route: &str,
    name: &str,
    window: u64,
    (ram, download): (u64, u64),
    thinking: bool,
) -> ModelChoice {
    let mut choice = choice(
        &format!("opencode/{route}"),
        ModelAccess::Local,
        CostEstimate::Free,
    );
    choice.name = name.to_owned();
    choice.listed = Some(ListedFacts {
        group: Some("Not installed · fits this Mac".to_owned()),
        window: Some(window),
        no_tools: false,
        thinking,
        hosted_by: None,
        // The controller lists one only where it fits beside the loaded models.
        memory: Some(ModelMemory {
            ram: ram * GIB,
            fit: Fit::Fits,
            download: Some(download * GIB),
        }),
    });
    choice
}

/// OpenCode's Default as it reads with memory known: the window and host of the model it
/// runs.
fn hosted_default() -> ModelChoice {
    let mut default = opencode_choices().remove(0);
    default.listed = Some(ListedFacts {
        window: Some(1_048_576),
        hosted_by: Some("OpenCode Zen".to_owned()),
        ..ListedFacts::default()
    });
    default
}

/// With a local tag's RAM at its window and where it stands on `machine`.
fn with_memory(mut choice: ModelChoice, machine: &MachineMemory) -> ModelChoice {
    let route = choice.worker.model.clone().unwrap_or_default();
    if let Some(&(_, ram, thinking)) = LOCAL_RAM.iter().find(|(at, ..)| *at == route) {
        let ram = ram * GIB;
        let fit = if route == "ollama/qwen3-coder:30b-64k" {
            Fit::Loaded
        } else {
            machine.fit(ram)
        };
        let listed = choice.listed.as_mut().expect("OpenCode's facts");
        listed.thinking = thinking;
        listed.memory = Some(ModelMemory {
            ram,
            fit,
            download: None,
        });
    } else if let Some(listed) = choice.listed.as_mut() {
        // A hosted model thinks where it takes a level.
        listed.thinking = !choice.efforts.is_empty();
    }
    choice
}

/// OpenCode's level on the pictured Mac, in the controller's order: the recommended model
/// first among Ollama's, then the models not installed that would fit, then OpenCode Zen's.
fn ram_choices() -> Vec<ModelChoice> {
    let opencode: Vec<ModelChoice> = opencode_choices()
        .into_iter()
        .skip(1)
        .map(|choice| with_memory(choice, &MAC_64))
        .collect();
    let (local, zen): (Vec<ModelChoice>, Vec<ModelChoice>) = opencode
        .into_iter()
        .partition(|choice| choice.access == ModelAccess::Local);
    let mut local = local;
    let at = local
        .iter()
        .position(|choice| choice.display_name() == "Qwen3 Coder 30B (64k)")
        .expect("the recommended model");
    let mut recommended = local.remove(at);
    recommended.recommended = true;
    local.insert(0, recommended);
    let suggested = [
        ("ollama/qwen3.5:27b", "Qwen3.5 27B", (21, 17), true),
        ("ollama/devstral:24b", "Devstral 24B", (18, 14), false),
        ("ollama/gpt-oss:20b", "GPT-OSS 20B", (16, 14), true),
    ]
    .into_iter()
    .map(|(route, name, sizes, thinking)| suggestion(route, name, 65_536, sizes, thinking));
    let mut claude = every_choice();
    claude.retain(|choice| choice.worker.agent != "opencode");
    claude
        .into_iter()
        .chain(std::iter::once(hosted_default()))
        .chain(local)
        .chain(suggested)
        .chain(zen)
        .collect()
}

#[test]
fn opencodes_level_knows_this_machines_memory() {
    let picture = picture(
        include_str!("../../../tests/golden/ram_models_after.txt"),
        include_str!("../../../tests/golden/ram_models_after.html"),
    );
    let choices = ram_choices();
    let mut picker = model_picker(&choices, FRESH);
    picker.flow.memory = Some(MAC_64);
    key(&mut picker, KeyCode::Char('4'));
    down_to(&mut picker, "Qwen3 Coder 30B (64k)");
    compare("ram_models_after", &picture, 2..21, &drawn(&picker));

    // Scrolled on to OpenCode Zen's third, then back to the first model not installed, where
    // Enter asks before anything downloads.
    down_to(&mut picker, "LongCat 2.5 Preview Free");
    for _ in 0..5 {
        key(&mut picker, KeyCode::Char('k'));
    }
    assert_eq!(pointed_label(&picker), "Qwen3.5 27B");
    key(&mut picker, KeyCode::Enter);
    assert!(picker.input.is_none() && picker.flow.download_asked.is_some());
    compare("ram_models_after", &picture, 24..44, &drawn(&picker));
    // Esc keeps looking; Enter twice pulls it with Ollama, and the list stays open on it.
    key(&mut picker, KeyCode::Esc);
    assert_eq!(picker.flow.download_asked, None);
    assert_eq!(picker.title, "OpenCode");
    key(&mut picker, KeyCode::Enter);
    let event = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        flow::advance(&mut picker, event),
        flow::Step::Send(argonauts_core::app::UiCommand::Shell {
            command: "ollama pull qwen3.5:27b".to_owned()
        })
    );
    assert_eq!(picker.flow.download_asked, None);
    assert_eq!(pointed_label(&picker), "Qwen3.5 27B");
}

/// The memory reading comes in the background after /model opens, and again with each later
/// list: a level opened before it gets its line and its rows' RAM form once it lands, and the
/// line says each newer reading's figures.
#[test]
fn a_memory_reading_after_the_level_opened_fills_its_line() {
    let picture = picture(
        include_str!("../../../tests/golden/ram_models_after.txt"),
        include_str!("../../../tests/golden/ram_models_after.html"),
    );
    let choices = ram_choices();
    let mut picker = model_picker(&choices, FRESH);
    key(&mut picker, KeyCode::Char('4'));
    down_to(&mut picker, "Qwen3 Coder 30B (64k)");
    assert_eq!(picker.banner, None);

    picker.flow.memory = Some(MAC_64);
    super::models::refresh(&mut picker, &choices);
    compare("ram_models_after", &picture, 2..21, &drawn(&picker));

    picker.flow.memory = Some(MachineMemory {
        in_use: 12 * GIB,
        free: 31 * GIB,
        ..MAC_64
    });
    super::models::refresh(&mut picker, &choices);
    let line: String = drawn(&picker)[3]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(
        line.trim_end(),
        "  RAM for models: up to 52 GB of this Mac's 64 GB · 12 GB in use · 31 GB free now"
    );
}

#[test]
fn a_smaller_mac_warns_and_suggests_a_model_that_fits() {
    let picture = picture(
        include_str!("../../../tests/golden/ram_models_after.txt"),
        include_str!("../../../tests/golden/ram_models_after.html"),
    );
    let mac_16 = MachineMemory {
        limit: 11 * GIB,
        total: 16 * GIB,
        in_use: 0,
        free: 8 * GIB,
        mac: true,
    };
    let local = |route: &'static str, name: &'static str, window: u64, ram: u64, fit: Fit| {
        let mut choice = opencode_choice(&tag(route, name, window, true));
        let listed = choice.listed.as_mut().expect("facts");
        listed.memory = Some(ModelMemory {
            ram: ram * GIB,
            fit,
            download: None,
        });
        choice
    };
    let mut qwen8 = suggestion("ollama/qwen3:8b", "Qwen3 8B", 32_768, (7, 6), true);
    qwen8.recommended = true;
    let opencode = vec![
        hosted_default(),
        local(
            "ollama/qwen3-coder:30b-64k",
            "Qwen3 Coder 30B (64k)",
            65_536,
            25,
            Fit::TooBig { at: None },
        ),
        local(
            "ollama/llama3.2:latest",
            "",
            131_072,
            24,
            Fit::TooBig {
                at: Some((32_768, 4 * GIB)),
            },
        ),
        qwen8,
        suggestion(
            "ollama/qwen2.5-coder:7b",
            "Qwen2.5 Coder 7B",
            32_768,
            (6, 5),
            false,
        ),
    ];
    let mut choices = every_choice();
    choices.retain(|choice| choice.worker.agent != "opencode");
    choices.extend(opencode);
    let mut picker = model_picker(&choices, FRESH);
    picker.flow.memory = Some(mac_16);
    key(&mut picker, KeyCode::Char('4'));
    down_to(&mut picker, "Qwen3 Coder 30B (64k)");
    let lines = drawn(&picker);
    compare("ram_models_after", &picture, 48..65, &lines);
}
