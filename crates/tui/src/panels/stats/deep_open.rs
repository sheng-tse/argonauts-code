//! "Open models", the list Summarize with opens: every model on this computer, as `/model` on
//! OpenCode lists them with what each takes of its memory, then up to three that would fit it.
//! It leaves out the abilities and the cost, which a summary doesn't need. A server installed
//! here that isn't running heads it with the answer that starts it.

use argonauts_core::app::ModelChoice;
use argonauts_core::behaviors::deep_map::OpenModels;
use argonauts_core::look::logos::model_logo;
use ratatui::text::{Line, Span};

use crate::base::kit::Kit;
use crate::base::kit::logo::logo_before;
use crate::base::text::{At, cut, str_width};
use crate::base::theme::Role;
use crate::bottom::PickerCell;
use crate::views::settings::memory_rows;

const INDENT: &str = "    ";
/// The table's columns after the name: their headings and widths, numbers to the right.
const CONTEXT: (&str, usize) = ("context", 9);
const MEMORY: (&str, usize) = ("memory", 9);
const DOWNLOAD: (&str, usize) = ("download", 11);
const BLANKS_BEFORE_FIT: usize = 4;
/// The cells of the gauge's track.
const GAUGE_CELLS: usize = 40;

/// One row of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRow {
    /// Start the server installed here, then list its models.
    Start,
    /// The model at this place in the offer's list.
    Model(usize),
}

/// The rows `filter` leaves, the start first.
pub fn rows(open: &OpenModels, filter: &str) -> Vec<OpenRow> {
    let wanted = filter.trim().to_lowercase();
    let start = open.stopped.as_ref().map(|_| OpenRow::Start);
    let models = open
        .models
        .iter()
        .enumerate()
        .filter(|(_, choice)| {
            wanted.is_empty()
                || choice.display_name().to_lowercase().contains(&wanted)
                || choice.worker.to_string().to_lowercase().contains(&wanted)
        })
        .map(|(at, _)| OpenRow::Model(at));
    start.into_iter().chain(models).collect()
}

/// A model not installed yet, which asks before it downloads.
pub fn download(choice: &ModelChoice) -> Option<u64> {
    choice.listed.as_ref()?.memory.as_ref()?.download
}

/// The answer that starts the stopped server, as the list and `u` word it.
pub fn start_words(open: &OpenModels) -> Option<String> {
    let stopped = open.stopped.as_ref()?;
    Some(format!("Start {} and list its models", stopped.name))
}

/// The list's title, body and banner rows, its rows with `pointer` on one, and its hint.
pub fn lines(
    kit: &Kit<'_>,
    open: &OpenModels,
    shown: &[OpenRow],
    pointer: usize,
    filter: Option<&str>,
    here: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("  ◆ ", kit.style(Role::Question)),
            kit.strong("Open models"),
        ]),
        Line::from(vec![
            Span::raw(INDENT),
            kit.dim(cut(
                &format!(
                    "Every model on this {here}, free, and nothing leaves it; then models that \
                     would fit it."
                ),
                width.saturating_sub(INDENT.len()),
                At::Word,
            )),
        ]),
    ];
    if let Some(stopped) = &open.stopped {
        let words = match stopped.tried {
            true => format!("{} didn't answer within 10 seconds.", stopped.name),
            false => format!("{} is installed but isn't running.", stopped.name),
        };
        lines.push(Line::from(vec![
            Span::raw(INDENT),
            kit.span(words, Role::Warn),
        ]));
    }
    if let Some(machine) = &open.machine {
        let (said, rest) = memory_rows::banner(machine);
        let room = width.saturating_sub(INDENT.len() + str_width(&said) + 1);
        lines.push(Line::from(vec![
            Span::raw(INDENT),
            kit.span(said, Role::Question),
            Span::raw(" "),
            kit.dim(cut(&rest, room, At::Char)),
        ]));
        let pointed = match shown.get(pointer) {
            Some(OpenRow::Model(at)) => open.models.get(*at),
            _ => None,
        };
        let gauge = memory_rows::gauge(pointed, machine);
        let mut spans = vec![
            Span::raw(INDENT),
            kit.dim(gauge.label.clone()),
            Span::raw("  "),
        ];
        spans.extend(gauge.spans(kit, GAUGE_CELLS));
        spans.push(Span::raw("  "));
        spans.push(kit.span(gauge.words.clone(), Role::Text));
        if let Some(short) = gauge.short {
            spans.push(kit.dim(" · "));
            spans.push(kit.span(short, Role::Warn));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    let name_width = name_column(kit, open, shown);
    let mut group: Option<String> = None;
    for (at, row) in shown.iter().enumerate() {
        let pointed = at == pointer;
        match row {
            OpenRow::Start => {
                let words = start_words(open).unwrap_or_default();
                lines.push(numbered(kit, (at, shown.len()), pointed, Vec::new(), words));
                lines.push(Line::default());
            }
            OpenRow::Model(model) => {
                let choice = &open.models[*model];
                let heading = heading(choice);
                if group.as_ref() != Some(&heading) {
                    if group.is_some() {
                        lines.push(Line::default());
                    }
                    lines.push(heading_line(kit, &heading, name_width));
                    group = Some(heading);
                }
                lines.push(model_line(
                    kit,
                    open,
                    choice,
                    (at, shown.len()),
                    pointed,
                    name_width,
                ));
            }
        }
    }
    if shown.is_empty() {
        lines.push(Line::from(vec![
            Span::raw(INDENT),
            kit.dim("No model's name has that in it."),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![Span::raw(INDENT), kit.dim(hint(filter))]));
    lines
}

/// The hint: what the keys do, or what you're filtering by.
pub fn hint(filter: Option<&str>) -> String {
    match filter {
        Some(typed) => format!("/{typed} · Enter to choose · Esc to clear the filter"),
        None => "Enter to choose · one not installed asks before it downloads · / to filter · \
                 Esc to go back"
            .to_owned(),
    }
}

/// The group a model is listed under, such as `Installed through Ollama`.
fn heading(choice: &ModelChoice) -> String {
    match choice.listed.as_ref() {
        Some(listed) => memory_rows::heading(listed.group.as_deref().unwrap_or_default(), listed),
        None => String::new(),
    }
}

/// The name column's width after the margin: the longest number, logo and name, or heading,
/// and two blanks.
fn name_column(kit: &Kit<'_>, open: &OpenModels, shown: &[OpenRow]) -> usize {
    let number = number(0, shown.len()).len();
    shown
        .iter()
        .filter_map(|row| match row {
            OpenRow::Model(at) => open.models.get(*at),
            OpenRow::Start => None,
        })
        .map(|choice| {
            let named = number + logo_cells(kit, choice) + str_width(&choice.display_name());
            named.max(str_width(&heading(choice)))
        })
        .max()
        .unwrap_or(0)
        + 2
}

fn logo_spans(kit: &Kit<'_>, choice: &ModelChoice) -> Vec<Span<'static>> {
    logo_before(
        &model_logo(&choice.worker.agent, choice.worker.model.as_deref()),
        kit.theme,
    )
}

fn logo_cells(kit: &Kit<'_>, choice: &ModelChoice) -> usize {
    logo_spans(kit, choice)
        .iter()
        .map(|span| str_width(&span.content))
        .sum()
}

/// ` 8. `: the row's number, as wide as the list's last.
fn number(at: usize, of: usize) -> String {
    format!("{:>w$}. ", at + 1, w = of.max(1).to_string().len())
}

fn heading_line(kit: &Kit<'_>, heading: &str, name_width: usize) -> Line<'static> {
    let mut text = heading.to_owned();
    text.push_str(&" ".repeat(name_width.saturating_sub(str_width(heading))));
    text.push_str(&format!("{:>w$}", CONTEXT.0, w = CONTEXT.1));
    text.push_str(&format!("{:>w$}", MEMORY.0, w = MEMORY.1));
    if heading.starts_with("Not installed") {
        text.push_str(&format!("{:>w$}", DOWNLOAD.0, w = DOWNLOAD.1));
    } else {
        text.push_str(&" ".repeat(DOWNLOAD.1));
    }
    text.push_str(&" ".repeat(BLANKS_BEFORE_FIT));
    text.push_str("fit");
    Line::from(vec![Span::raw(INDENT), kit.span(text, Role::Text)])
}

/// `❯ 1. ` and what follows it: a logo where one draws, then the words. Numbers are right
/// aligned to the most the list has, given as `(at, of)`.
fn numbered(
    kit: &Kit<'_>,
    (at, of): (usize, usize),
    pointed: bool,
    logo: Vec<Span<'static>>,
    words: String,
) -> Line<'static> {
    let lit = kit.style(Role::Question);
    let style = if pointed { lit } else { kit.style(Role::Text) };
    let mut spans = vec![if pointed {
        Span::styled("  ❯ ", lit)
    } else {
        Span::raw(INDENT)
    }];
    spans.push(Span::styled(number(at, of), style));
    spans.extend(logo);
    spans.push(Span::styled(words, style));
    Line::from(spans)
}

fn model_line(
    kit: &Kit<'_>,
    open: &OpenModels,
    choice: &ModelChoice,
    (at, of): (usize, usize),
    pointed: bool,
    name_width: usize,
) -> Line<'static> {
    let logo = logo_spans(kit, choice);
    let used = number(at, of).len() + logo_cells(kit, choice);
    let name = choice.display_name();
    let pad = name_width.saturating_sub(used + str_width(&name));
    let mut line = numbered(kit, (at, of), pointed, logo, name);
    let cells = match choice.listed.as_ref() {
        Some(listed) => memory_rows::cells(choice, listed, open.machine.as_ref(), true),
        None => Vec::new(),
    };
    let cell = |at: usize| cells.get(at).cloned().unwrap_or_default();
    let mut text = " ".repeat(pad);
    text.push_str(&format!("{:>w$}", cell(0).text, w = CONTEXT.1));
    text.push_str(&format!("{:>w$}", cell(1).text, w = MEMORY.1));
    text.push_str(&format!("{:>w$}", cell(4).text, w = DOWNLOAD.1));
    text.push_str(&" ".repeat(BLANKS_BEFORE_FIT));
    line.spans.push(kit.dim(text));
    let fit: PickerCell = cell(5);
    line.spans.push(match fit.role {
        Some(role) => kit.span(fit.text, role),
        None => kit.dim(fit.text),
    });
    line
}

#[cfg(test)]
pub(crate) mod sample {
    use argonauts_core::app::model_picker::{Fit, ListedFacts, MachineMemory, ModelMemory};
    use argonauts_core::base::names::ModelRef;
    use argonauts_core::behaviors::deep_map::{DeepOffer, StoppedServer};
    use argonauts_core::models::limits::CostEstimate;
    use argonauts_core::models::registry::ModelAccess;

    use super::*;

    const GB: u64 = 1 << 30;

    fn model(
        tag: &str,
        name: &str,
        window: u64,
        gb: u64,
        fit: Fit,
        download: Option<u64>,
    ) -> ModelChoice {
        let group = match download {
            Some(_) => "Not installed · fits this computer",
            None => "Ollama",
        };
        ModelChoice {
            worker: ModelRef::new("ollama", Some(tag.to_owned())),
            provider: String::new(),
            access: ModelAccess::Local,
            cost: CostEstimate::Free,
            plan_usage: None,
            efforts: Vec::new(),
            strengths: Vec::new(),
            current: false,
            note: None,
            older: false,
            name: name.to_owned(),
            description: None,
            recommended: false,
            new: false,
            default_effort: None,
            effort: None,
            alias: false,
            runs: None,
            listed: Some(ListedFacts {
                group: Some(group.to_owned()),
                window: Some(window),
                no_tools: false,
                thinking: false,
                hosted_by: None,
                memory: Some(ModelMemory {
                    ram: gb * GB,
                    fit,
                    download: download.map(|gb| gb * GB),
                }),
            }),
        }
    }

    /// The page's computer: 64 GB, 52 GB for models, seven Ollama models and three that would
    /// fit.
    pub fn open_offer() -> DeepOffer {
        let mut offer = crate::panels::stats::deep_map::sample::page_offer();
        let mut models = vec![
            model(
                "sample-coder:30b-64k",
                "Sample Coder 30B (64k)",
                65_536,
                25,
                Fit::Loaded,
                None,
            ),
            model("gemma4:e2b", "Gemma4 E2B", 131_072, 8, Fit::Fits, None),
            model(
                "llama3.2:latest",
                "Llama3.2",
                131_072,
                24,
                Fit::Tight { at: None },
                None,
            ),
            model(
                "qwen3-coder:30b",
                "Qwen3 Coder 30B",
                262_144,
                45,
                Fit::FitsAlone,
                None,
            ),
            model("gemma2:9b", "Gemma2 9B", 8_192, 6, Fit::Fits, None),
            model("gemma3:12b", "Gemma3 12B", 131_072, 17, Fit::Fits, None),
            model(
                "sample-gemma:12b-32k",
                "Sample Gemma 12B (32k)",
                32_768,
                11,
                Fit::Fits,
                None,
            ),
            model(
                "qwen3.5:27b",
                "Qwen3.5 27B",
                65_536,
                21,
                Fit::Tight { at: None },
                Some(17),
            ),
            model(
                "devstral:24b",
                "Devstral 24B",
                65_536,
                18,
                Fit::Fits,
                Some(14),
            ),
            model(
                "gpt-oss:20b",
                "GPT-OSS 20B",
                65_536,
                16,
                Fit::Fits,
                Some(14),
            ),
        ];
        models[0].recommended = true;
        offer.open = OpenModels {
            machine: Some(MachineMemory {
                limit: 52 * GB,
                total: 64 * GB,
                in_use: 25 * GB,
                free: 18 * GB,
            }),
            models,
            stopped: None,
        };
        offer
    }

    /// Ollama installed here but not running, so nothing on this computer answered.
    pub fn stopped_offer() -> DeepOffer {
        let mut offer = crate::panels::stats::deep_map::sample::page_offer();
        offer.summarizers.retain(|offer| !offer.local());
        offer.open = OpenModels {
            machine: None,
            models: Vec::new(),
            stopped: Some(StoppedServer {
                runtime: "ollama".to_owned(),
                name: "Ollama".to_owned(),
                tried: false,
            }),
        };
        offer
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::behaviors::deep_map::{DeepScope, DeepStart, MadeBy};
    use crossterm::event::KeyCode;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Widget};

    use super::sample::{open_offer, stopped_offer};
    use crate::panels::stats::deep_map::{DeepAnswer, DeepQuestions};
    use crate::testing::pictures::golden::theme;

    const WIDTH: u16 = 118;

    fn text(questions: &DeepQuestions, picture: &str) -> Vec<String> {
        let theme = theme();
        let kit = crate::base::kit::Kit::new(&theme);
        let lines = questions.lines(&kit, usize::from(WIDTH));
        let area = Rect::new(0, 0, WIDTH, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines).render(area, &mut buf);
        crate::testing::pictures::golden::keep(picture, &buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn at_summarize_with(offer: argonauts_core::behaviors::deep_map::DeepOffer) -> DeepQuestions {
        let mut questions = DeepQuestions::new(offer);
        assert_eq!(questions.key(KeyCode::Enter), None);
        questions
    }

    fn started(answer: Option<DeepAnswer>) -> DeepStart {
        match answer {
            Some(DeepAnswer::Start(start)) => start,
            other => panic!("no start: {other:?}"),
        }
    }

    #[test]
    fn open_models_comes_after_the_two_local_models_and_before_the_hosted_ones() {
        let rows = text(&at_summarize_with(open_offer()), "d3_summarize_with");
        let rows: Vec<&str> = rows[3..8].iter().map(|row| row.trim()).collect();
        assert!(
            rows[0].starts_with("❯ 1. Qwen3 Coder 30B on Ollama (recommended)"),
            "{rows:?}"
        );
        assert!(rows[1].starts_with("2. Gemma 3 12B on Ollama"), "{rows:?}");
        assert!(rows[2].starts_with("3. Open models"), "{rows:?}");
        assert!(
            rows[2].ends_with("7 on this Mac · 3 more would fit it"),
            "{rows:?}"
        );
        assert!(
            rows[3].starts_with("4. Haiku 5.5 on Claude Code"),
            "{rows:?}"
        );
        assert!(rows[4].starts_with("5. GPT 6 Luna on Codex"), "{rows:?}");
    }

    #[test]
    fn the_list_shows_every_model_here_what_it_takes_and_the_ones_that_would_fit() {
        let mut questions = at_summarize_with(open_offer());
        assert_eq!(questions.key(KeyCode::Char('3')), None);
        let rows = text(&questions, "d3_open_models");
        let at = |words: &str| {
            rows.iter()
                .find(|row| row.contains(words))
                .unwrap_or_else(|| panic!("no row with {words}: {rows:#?}"))
                .clone()
        };
        assert!(
            at("Models can use").contains("up to 52 GB of this computer's 64 GB · 18 GB free now")
        );
        assert!(at("this pick").ends_with("25 of 52 GB · loaded now"));
        let installed = at("Installed through Ollama");
        assert!(installed.contains("context") && installed.ends_with("fit"));
        let qwen = at("1. Sample Coder 30B (64k)");
        assert!(qwen.contains(" 64k    25 GB ") && qwen.ends_with(" loaded · recommended"));
        assert!(at("3. Llama3.2").ends_with("fits once apps free 6 GB"));
        assert!(at("Not installed · fits this computer").contains("download"));
        assert!(at(" 8. Qwen3.5 27B").contains("21 GB      17 GB"));
        assert!(at("10. GPT-OSS 20B").starts_with("    10. GPT-OSS"));
        assert!(
            at("Enter to choose · one not installed").ends_with("/ to filter · Esc to go back")
        );
    }

    #[test]
    fn slash_filters_the_list_and_esc_clears_it_then_goes_back() {
        let mut questions = at_summarize_with(open_offer());
        questions.key(KeyCode::Char('3'));
        for key in ['/', '1', '2', 'b'] {
            assert_eq!(questions.key(KeyCode::Char(key)), None);
        }
        let rows = text(&questions, "d3_filtered");
        let numbered: Vec<&String> = rows.iter().filter(|row| row.contains(". ")).collect();
        assert_eq!(numbered.len(), 2, "{rows:#?}");
        assert!(
            rows.last()
                .unwrap()
                .contains("/12b · Enter to choose · Esc to clear")
        );
        assert_eq!(questions.key(KeyCode::Esc), None);
        assert_eq!(questions.key(KeyCode::Esc), None);
        let rows = text(&questions, "d3_back");
        assert!(
            rows.iter().any(|row| row.contains("❯ 3. Open models")),
            "{rows:#?}"
        );
    }

    #[test]
    fn enter_on_a_model_here_summarizes_with_it_and_one_not_here_asks_before_it_downloads() {
        let mut questions = at_summarize_with(open_offer());
        questions.key(KeyCode::Char('3'));
        // The embedder's download is asked after, as for any summarizer.
        assert_eq!(questions.key(KeyCode::Char('2')), None);
        let start = started(questions.key(KeyCode::Enter));
        assert_eq!(start.summarizer.to_string(), "ollama/gemma4:e2b");
        assert!(!start.download_summarizer);

        let mut questions = at_summarize_with(open_offer());
        questions.key(KeyCode::Char('3'));
        for _ in 0..7 {
            questions.key(KeyCode::Down);
        }
        assert_eq!(questions.key(KeyCode::Enter), None);
        let rows = text(&questions, "d3_download");
        assert!(
            rows[0].contains("Download Qwen3.5 27B with Ollama (17 GB download)?"),
            "{rows:#?}"
        );
        assert_eq!(questions.key(KeyCode::Enter), None);
        let start = started(questions.key(KeyCode::Enter));
        assert_eq!(start.summarizer.to_string(), "ollama/qwen3.5:27b");
        assert!(start.download_summarizer);
        assert_eq!(start.scope, DeepScope::Everything);
    }

    #[test]
    fn a_stopped_ollama_is_said_and_open_models_starts_it_then_lists_its_models() {
        let mut questions = at_summarize_with(stopped_offer());
        let rows = text(&questions, "d3_stopped");
        assert!(rows.iter().any(|row| {
            row.contains("Ollama is installed but isn't running; Open models starts it.")
        }));
        assert!(
            rows.iter().any(|row| row.contains("1. Open models")
                && row.ends_with("Ollama isn't running · this starts it")),
            "{rows:#?}"
        );
        assert_eq!(questions.key(KeyCode::Enter), None);
        let rows = text(&questions, "d3_stopped_list");
        assert!(
            rows.iter()
                .any(|row| row.contains("❯ 1. Start Ollama and list its models")),
            "{rows:#?}"
        );
        assert_eq!(
            questions.key(KeyCode::Enter),
            Some(DeepAnswer::StartServer("ollama".to_owned()))
        );
        assert!(questions.starting());
        let rows = text(&questions, "d3_starting");
        assert!(
            rows.last()
                .unwrap()
                .contains("Starting Ollama and waiting up to 10 seconds")
        );
        let again = questions.restart(open_offer());
        let rows = text(&again, "d3_started");
        assert!(
            rows.iter()
                .any(|row| row.contains("Installed through Ollama")),
            "{rows:#?}"
        );
    }

    #[test]
    fn u_on_a_map_whose_ollama_is_stopped_offers_to_start_it_never_a_redo() {
        let mut offer = stopped_offer();
        offer.made_by = Some(MadeBy {
            model: "ollama/sample-coder:30b-64k".parse().unwrap(),
            name: "Sample Coder 30B on Ollama".to_owned(),
            actions: 562,
            new_actions: 12,
            prompt_changed: false,
        });
        let mut questions = DeepQuestions::update(offer);
        let rows = text(&questions, "d3_update_stopped");
        assert!(
            rows.iter()
                .any(|row| row.contains("❯ 1. Start Ollama and add the new actions")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("2. Keep the map as it is"))
        );
        assert_eq!(
            questions.key(KeyCode::Enter),
            Some(DeepAnswer::StartServer("ollama".to_owned()))
        );
        assert!(questions.updating());
    }
}
