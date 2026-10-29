//! The deeper Behaviors map's questions, asked in the tab when you press `d`: what to
//! summarize, each answer with how long it takes; who summarizes, each with what it costs; and,
//! for a hosted model, once more, since it would read the other CLIs' actions too. A download
//! of the embedding model is asked last, and only once.

use argonauts_core::deep_map::{
    DeepOffer, DeepProgress, DeepScope, DeepStart, EmbedderNote, OfferPlace, ScopeOffer,
    SummarizerOffer,
};
use argonauts_core::model_picker::Fit;
use crossterm::event::KeyCode;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::kit::Kit;
use crate::theme::Role;

/// Where every row of a question starts, inside the tab's own margin.
const INDENT: &str = "    ";
/// The column each answer's description starts at, so the descriptions read down one edge.
const DESCRIPTION_AT: usize = 59;
/// The least room between an answer and its description.
const GAP: usize = 2;

const GB: f64 = 1_000_000_000.0;

/// Which question shows.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// `d` on the deeper map: add the actions it doesn't have yet.
    Update,
    Scope,
    Summarizer,
    /// A hosted summarizer was chosen; this asks once more.
    Hosted(usize),
    /// The map there was made by another summarizer or an older prompt; this would redo it.
    Redo(usize),
    /// The embedding model isn't on this machine yet.
    Download(usize),
    /// No model could summarize: no server on this machine answered, and no CLI is there.
    NoSummarizer,
}

/// What an answer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepAnswer {
    /// Summarize with these answers.
    Start(DeepStart),
    /// Not now: the map that shows stays as it is.
    NotNow,
    /// Back to the label map.
    LabelMap,
}

/// The questions, from the first to the answer.
#[derive(Debug, Clone, PartialEq)]
pub struct DeepQuestions {
    offer: DeepOffer,
    step: Step,
    pointer: usize,
    scope: usize,
    /// You said yes to redoing the map.
    redo: bool,
}

impl DeepQuestions {
    pub fn new(offer: DeepOffer) -> DeepQuestions {
        let step = if offer.summarizers.is_empty() {
            Step::NoSummarizer
        } else {
            Step::Scope
        };
        DeepQuestions {
            offer,
            step,
            pointer: 0,
            scope: 0,
            redo: false,
        }
    }

    /// What `d` on the deeper map asks: adding the actions it doesn't have yet, or going back
    /// to the label map.
    pub fn update(offer: DeepOffer) -> DeepQuestions {
        let questions = DeepQuestions::new(offer);
        if questions.step == Step::NoSummarizer {
            return questions;
        }
        DeepQuestions {
            step: Step::Update,
            ..questions
        }
    }

    /// The update `start` begins, as the tab says it until the app reports: the summarizer by
    /// its name, the actions it will read, and how long the offer said they take, where it
    /// said so for a summarizer of this kind (on this computer, or hosted).
    pub fn started(&self, start: &DeepStart) -> DeepProgress {
        let chosen = self
            .offer
            .summarizers
            .iter()
            .find(|offer| offer.model == start.summarizer);
        let summarizer =
            chosen.map_or_else(|| start.summarizer.to_string(), |offer| offer.name.clone());
        let everything = self.offer.scopes.first();
        let (actions, seconds) = match (&self.step, &self.offer.made_by) {
            (Step::Update, Some(made_by)) => {
                let new = made_by.new_actions;
                let seconds = everything.map_or(0, |scope| {
                    scope.seconds * new as u64 / scope.actions.max(1) as u64
                });
                (new, seconds)
            }
            _ => self
                .offer
                .scopes
                .iter()
                .find(|scope| scope.scope == start.scope)
                .map_or((0, 0), |scope| (scope.actions, scope.seconds)),
        };
        // The offer timed auto's summarizer, which is the first.
        let auto_local = self.offer.summarizers.first().map(SummarizerOffer::local);
        let timed = chosen.is_some_and(|offer| Some(offer.local()) == auto_local);
        DeepProgress {
            done: 0,
            total: actions,
            seconds_left: timed.then_some(seconds),
            summarizer,
        }
    }

    /// A key while the questions show. `None` while they go on.
    pub fn key(&mut self, code: KeyCode) -> Option<DeepAnswer> {
        let count = self.answers().len();
        if count == 0 {
            return matches!(code, KeyCode::Enter | KeyCode::Esc).then_some(DeepAnswer::NotNow);
        }
        match code {
            KeyCode::Up => {
                self.pointer = self.pointer.checked_sub(1).unwrap_or(count - 1);
                None
            }
            KeyCode::Down => {
                self.pointer = (self.pointer + 1) % count;
                None
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let at = digit as usize - '1' as usize;
                if at < count {
                    self.pointer = at;
                    return self.choose();
                }
                None
            }
            KeyCode::Enter => self.choose(),
            KeyCode::Esc => self.back(),
            _ => None,
        }
    }

    fn back(&mut self) -> Option<DeepAnswer> {
        let (step, pointer) = match self.step {
            Step::Update | Step::Scope | Step::NoSummarizer => return Some(DeepAnswer::NotNow),
            Step::Summarizer => (Step::Scope, self.scope),
            Step::Hosted(chosen) | Step::Redo(chosen) | Step::Download(chosen) => {
                (Step::Summarizer, chosen)
            }
        };
        self.step = step;
        self.pointer = pointer;
        None
    }

    fn choose(&mut self) -> Option<DeepAnswer> {
        match self.step.clone() {
            Step::Update => match self.pointer {
                0 => self.update_with_its_summarizer(),
                1 => Some(DeepAnswer::LabelMap),
                _ => Some(DeepAnswer::NotNow),
            },
            Step::Scope => {
                if self.pointer >= self.offer.scopes.len() {
                    return Some(DeepAnswer::NotNow);
                }
                self.scope = self.pointer;
                self.step = if self.offer.summarizers.is_empty() {
                    Step::NoSummarizer
                } else {
                    Step::Summarizer
                };
                self.pointer = 0;
                None
            }
            Step::Summarizer => self.summarize_with(self.pointer),
            Step::NoSummarizer => Some(DeepAnswer::NotNow),
            Step::Hosted(chosen) => match (self.pointer, self.first_local()) {
                (0, _) => self.chosen(chosen),
                (1, Some(local)) => self.chosen(local),
                _ => Some(DeepAnswer::NotNow),
            },
            Step::Redo(chosen) if self.pointer == 0 => {
                self.redo = true;
                self.chosen(chosen)
            }
            Step::Redo(_) => Some(DeepAnswer::NotNow),
            Step::Download(chosen) => Some(self.start(chosen, self.pointer == 0)),
        }
    }

    /// The summarizer is settled: redoing a map made by another is asked about, then the
    /// embedder's download, each when it's needed.
    fn chosen(&mut self, summarizer: usize) -> Option<DeepAnswer> {
        let model = &self.offer.summarizers[summarizer].model;
        let redoes = self
            .offer
            .made_by
            .as_ref()
            .is_some_and(|made_by| made_by.redone_by(model));
        if redoes && !self.redo {
            self.step = Step::Redo(summarizer);
            self.pointer = 0;
            return None;
        }
        if matches!(self.offer.embedder, EmbedderNote::Download { .. }) {
            self.step = Step::Download(summarizer);
            self.pointer = 0;
            return None;
        }
        Some(self.start(summarizer, false))
    }

    /// A summarizer chosen: a hosted one is asked about once more, then redoing a map made by
    /// another, then the embedder's download.
    fn summarize_with(&mut self, chosen: usize) -> Option<DeepAnswer> {
        if !self.offer.summarizers.get(chosen)?.local() {
            self.step = Step::Hosted(chosen);
            self.pointer = 0;
            return None;
        }
        self.chosen(chosen)
    }

    /// The map's own summarizer adds the new actions when it runs on this machine. A newer
    /// prompt, or another summarizer the settings now name, would make the map again, which
    /// asks first. A hosted summarizer asks again, from what to summarize, as it did when the
    /// map was made.
    fn update_with_its_summarizer(&mut self) -> Option<DeepAnswer> {
        let made_by = self.offer.made_by.as_ref()?;
        let wanted = self
            .offer
            .named
            .clone()
            .unwrap_or_else(|| made_by.model.clone());
        let offered = self
            .offer
            .summarizers
            .iter()
            .position(|offer| offer.model == wanted);
        if made_by.redone_by(&wanted)
            && let Some(at) = offered
        {
            return self.summarize_with(at);
        }
        let local = offered.is_some_and(|at| self.offer.summarizers[at].local());
        if local && !made_by.redone_by(&wanted) {
            return Some(DeepAnswer::Start(DeepStart {
                scope: DeepScope::Everything,
                summarizer: wanted,
                download_embedder: false,
                redo: false,
            }));
        }
        self.step = Step::Scope;
        self.pointer = 0;
        None
    }

    fn start(&self, summarizer: usize, download_embedder: bool) -> DeepAnswer {
        DeepAnswer::Start(DeepStart {
            scope: self.offer.scopes[self.scope].scope.clone(),
            summarizer: self.offer.summarizers[summarizer].model.clone(),
            download_embedder,
            redo: self.redo,
        })
    }

    fn first_local(&self) -> Option<usize> {
        first_local(&self.offer)
    }

    fn chosen_scope(&self) -> &ScopeOffer {
        &self.offer.scopes[self.scope]
    }

    /// The answers on the question that shows, each with its description.
    fn answers(&self) -> Vec<Answer> {
        match &self.step {
            Step::Update => {
                let new = self
                    .offer
                    .made_by
                    .as_ref()
                    .map_or(0, |made| made.new_actions);
                let everything = self.offer.scopes.first();
                let seconds = everything.map_or(0, |scope| {
                    scope.seconds * new as u64 / scope.actions.max(1) as u64
                });
                vec![
                    Answer::new(format!(
                        "Update: {}, {}",
                        plural(new, "new action"),
                        short_duration_words(seconds)
                    )),
                    Answer::new("Show the label map".to_owned()),
                    Answer::new("Not now".to_owned()),
                ]
            }
            Step::Scope => {
                let auto_local = self
                    .offer
                    .summarizers
                    .first()
                    .is_some_and(SummarizerOffer::local);
                let mut answers: Vec<Answer> = self
                    .offer
                    .scopes
                    .iter()
                    .enumerate()
                    .map(|(at, scope)| {
                        let mut took = duration_words(scope.seconds);
                        if at == 0 {
                            match self.offer.summarizers.first() {
                                Some(_) if auto_local => took.push_str(" with your local model"),
                                Some(auto) => took.push_str(&format!(" with {}", auto.short)),
                                None => {}
                            }
                        }
                        Answer::new(scope_words(scope)).described(took)
                    })
                    .collect();
                answers.push(Answer::new("Not now".to_owned()));
                answers
            }
            Step::Summarizer => {
                let descriptions = summarizer_descriptions(&self.offer, self.chosen_scope().tokens);
                self.offer
                    .summarizers
                    .iter()
                    .zip(descriptions)
                    .enumerate()
                    .map(|(at, (offer, description))| {
                        let answer = Answer::new(offer.name.clone()).described(description);
                        if at == 0 {
                            answer.noted("(recommended)")
                        } else {
                            answer
                        }
                    })
                    .collect()
            }
            Step::Hosted(chosen) => hosted_answers(&self.offer, *chosen)
                .into_iter()
                .map(|(words, description)| {
                    let answer = Answer::new(words);
                    match description {
                        Some(description) => answer.described(description),
                        None => answer,
                    }
                })
                .collect(),
            Step::Redo(chosen) => vec![
                Answer::new(format!(
                    "Yes, summarize them again with {}",
                    self.offer.summarizers[*chosen].short
                )),
                Answer::new("Keep the map as it is".to_owned()),
            ],
            Step::Download(_) => {
                let megabytes = match &self.offer.embedder {
                    EmbedderNote::Download { megabytes, .. } => *megabytes,
                    _ => 0,
                };
                vec![
                    Answer::new("Yes, download it".to_owned())
                        .described(format!("{megabytes} MB, once")),
                    Answer::new("Use word vectors instead".to_owned())
                        .described("placed by the summaries' words alone".to_owned()),
                ]
            }
            Step::NoSummarizer => Vec::new(),
        }
    }

    fn here(&self) -> &'static str {
        here(&self.offer)
    }

    fn title_and_body(&self) -> (String, Vec<String>) {
        match &self.step {
            Step::Update => {
                let (name, actions) = self
                    .offer
                    .made_by
                    .as_ref()
                    .map_or(("", 0), |made| (made.name.as_str(), made.actions));
                (
                    "Update the deeper map?".to_owned(),
                    vec![format!(
                        "{name} summarized its {actions} actions. Only the new ones are \
                         summarized, and the dots there stay where they are."
                    )],
                )
            }
            Step::Scope => (
                "Summarize your agents' actions?".to_owned(),
                vec![
                    "Each action gets one line from a model, embedded and mapped beside the \
                     others. It runs in the background and keeps every summary, so later runs \
                     only add their new actions."
                        .to_owned(),
                ],
            ),
            Step::Summarizer => (
                "Summarize with".to_owned(),
                vec![
                    "One model writes every summary, so the map compares the agents, not their \
                     summarizers."
                        .to_owned(),
                ],
            ),
            Step::Hosted(chosen) => {
                hosted_question(&self.offer, *chosen, Some(self.chosen_scope()))
            }
            Step::Redo(chosen) => {
                let offer = &self.offer.summarizers[*chosen];
                let (made_by, actions, prompt_changed) = match &self.offer.made_by {
                    Some(made_by) => (
                        made_by.name.as_str(),
                        made_by.actions,
                        made_by.prompt_changed,
                    ),
                    None => ("", 0, false),
                };
                let why = if prompt_changed {
                    format!(
                        "The deeper map's summaries were written with an older prompt. One map never \
                         mixes two, so its {actions} actions would be summarized again with {}.",
                        offer.name
                    )
                } else {
                    format!(
                        "The deeper map was made by {made_by}. One map never mixes two \
                         summarizers, so its {actions} actions would be summarized again with {}.",
                        offer.name
                    )
                };
                (
                    "Summarize the map again?".to_owned(),
                    vec![why, "The summaries already written are kept.".to_owned()],
                )
            }
            Step::NoSummarizer => (
                "No model to summarize with".to_owned(),
                vec![
                    "No server on this computer answered, and no CLI is installed. A model \
                     served here, such as one on Ollama, or a CLI on your plan can summarize."
                        .to_owned(),
                ],
            ),
            Step::Download(_) => {
                let (name, megabytes) = match &self.offer.embedder {
                    EmbedderNote::Download { name, megabytes } => (name.as_str(), *megabytes),
                    _ => ("", 0),
                };
                (
                    format!("Download {name} for the map?"),
                    vec![format!(
                        "It turns each summary into a vector, so summaries that mean the same \
                         land together. It's {megabytes} MB, downloaded once and kept on this {}.",
                        self.here()
                    )],
                )
            }
        }
    }

    fn hint(&self) -> String {
        match &self.step {
            Step::Update => "Enter to choose · Esc to keep this map".to_owned(),
            Step::Scope => "Enter to choose · Esc to keep the label map".to_owned(),
            Step::Summarizer => match &self.offer.embedder {
                EmbedderNote::Ready { name } => format!("Enter to choose · the embedder is {name}"),
                EmbedderNote::Download { name, megabytes } => {
                    format!("Enter to choose · the embedder is {name} ({megabytes} MB, asked once)")
                }
                EmbedderNote::Words => {
                    "Enter to choose · no embedding model, so summaries are placed by their words"
                        .to_owned()
                }
            },
            Step::Hosted(_) | Step::Redo(_) | Step::Download(_) => "Enter to choose".to_owned(),
            Step::NoSummarizer => "Esc to keep the label map".to_owned(),
        }
    }

    /// The question as a screen reader says it.
    pub fn spoken(&self) -> Vec<String> {
        let (title, body) = self.title_and_body();
        let mut said = vec![title];
        said.extend(body);
        for (at, answer) in self.answers().iter().enumerate() {
            let mut words = format!("{}. {}", at + 1, answer.words);
            if let Some(description) = &answer.description {
                words.push_str(&format!(", {description}"));
            }
            said.push(words);
        }
        said.push(self.hint());
        said
    }

    /// The question's rows, `width` cells wide, in the tab's margin.
    pub fn lines(&self, kit: &Kit<'_>, width: usize) -> Vec<Line<'static>> {
        let (title, body) = self.title_and_body();
        let mut lines = vec![Line::from(vec![
            Span::styled("  ◆ ", kit.style(Role::Question)),
            kit.strong(title),
        ])];
        let room = width.saturating_sub(INDENT.len()).max(1);
        for paragraph in &body {
            for row in crate::text::wrap_text(paragraph, kit.style(Role::Dim), room, &[], &[]) {
                let mut spans = vec![Span::raw(INDENT)];
                spans.extend(row.spans);
                lines.push(Line::from(spans));
            }
        }
        lines.push(Line::default());
        let answers = self.answers();
        for (at, answer) in answers.iter().enumerate() {
            lines.push(answer.line(kit, at, at == self.pointer, width));
        }
        if !answers.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![Span::raw(INDENT), kit.dim(self.hint())]));
        lines
    }
}

/// The first question, as a screen reader says it when the offer comes.
pub fn spoken(offer: &DeepOffer) -> Vec<String> {
    DeepQuestions::new(offer.clone()).spoken()
}

/// What each summarizer costs and where it runs, as "Summarize with" describes it: free on
/// this computer with what its memory allows, or about the tokens a hosted one would use of
/// `tokens`.
pub(crate) fn summarizer_descriptions(offer: &DeepOffer, tokens: u64) -> Vec<String> {
    let here = here(offer);
    let first_bytes = offer
        .summarizers
        .iter()
        .find_map(|offer| match offer.place {
            OfferPlace::ThisMachine { bytes, .. } => Some(bytes),
            OfferPlace::Plan { .. } => None,
        });
    offer
        .summarizers
        .iter()
        .enumerate()
        .map(|(at, offer)| match &offer.place {
            OfferPlace::ThisMachine { bytes, fit } => {
                let smaller = at > 0
                    && fits_now(*fit)
                    && matches!((bytes, first_bytes), (Some(b), Some(Some(first))) if b < &first);
                if smaller {
                    format!("free · on this {here} · faster, a little rougher")
                } else {
                    format!("free · on this {here}{}", memory_words(*bytes, *fit))
                }
            }
            OfferPlace::Plan { plan, .. } => {
                format!("your {plan} plan · about {} tokens", token_words(tokens))
            }
        })
        .collect()
}

/// The question a hosted summarizer asks once more, for the actions `scope` covers: its title
/// and body. With no scope, before any action is recorded, it asks for whatever is recorded
/// from then on.
pub(crate) fn hosted_question(
    offer: &DeepOffer,
    chosen: usize,
    scope: Option<&ScopeOffer>,
) -> (String, Vec<String>) {
    let summarizer = &offer.summarizers[chosen];
    let (cli, plan, provider) = match &summarizer.place {
        OfferPlace::Plan {
            cli,
            plan,
            provider,
        } => (cli.as_str(), plan.as_str(), provider.as_str()),
        OfferPlace::ThisMachine { .. } => ("", "", ""),
    };
    let Some(scope) = scope else {
        return (
            format!("Send your actions to {provider} for summaries?"),
            vec![
                format!(
                    "{} would read each action as it ran: commands, file paths and the lines \
                     they touched, from every CLI. None are recorded yet, so how much of your \
                     {plan} plan it uses isn't known until d asks.",
                    summarizer.name
                ),
                "Nothing is sent anywhere else.".to_owned(),
            ],
        );
    };
    let others: Vec<String> = scope
        .by_cli
        .iter()
        .filter(|count| count.cli != cli && count.actions > 0)
        .map(|count| format!("{} {}", count.actions, count.name))
        .collect();
    let reads = if others.is_empty() {
        String::new()
    } else {
        format!(", including the {} actions", and_list(&others))
    };
    (
        format!(
            "Send {} actions to {provider} for summaries?",
            scope.actions
        ),
        vec![
            format!(
                "{} would read each action as it ran: commands, file paths and the lines they \
                 touched{reads}. It uses about {} tokens of your {plan} plan.",
                summarizer.name,
                token_words(scope.tokens)
            ),
            "Nothing is sent anywhere else.".to_owned(),
        ],
    )
}

/// The hosted question's answers, each with its description: yes, the first model on this
/// computer instead where there is one, and not now.
pub(crate) fn hosted_answers(offer: &DeepOffer, chosen: usize) -> Vec<(String, Option<String>)> {
    let summarizer = &offer.summarizers[chosen];
    let mut answers = vec![(format!("Yes, summarize with {}", summarizer.short), None)];
    if let Some(local) = first_local(offer) {
        let local = &offer.summarizers[local];
        answers.push((
            format!("Use {} on this {} instead", local.short, here(offer)),
            Some("free".to_owned()),
        ));
    }
    answers.push(("Not now".to_owned(), None));
    answers
}

/// The first summarizer on this computer, which the hosted question offers instead.
pub(crate) fn first_local(offer: &DeepOffer) -> Option<usize> {
    offer.summarizers.iter().position(SummarizerOffer::local)
}

/// This computer as the questions call it.
pub(crate) fn here(offer: &DeepOffer) -> &'static str {
    if offer.mac { "Mac" } else { "computer" }
}

/// One answer: its words, a dim note after them, and its description at the column.
struct Answer {
    words: String,
    note: Option<&'static str>,
    description: Option<String>,
}

impl Answer {
    fn new(words: String) -> Answer {
        Answer {
            words,
            note: None,
            description: None,
        }
    }

    fn noted(mut self, note: &'static str) -> Answer {
        self.note = Some(note);
        self
    }

    fn described(mut self, description: String) -> Answer {
        self.description = Some(description);
        self
    }

    fn line(&self, kit: &Kit<'_>, at: usize, pointed: bool, width: usize) -> Line<'static> {
        let lit = kit.style(Role::Question);
        let mut spans = vec![if pointed {
            Span::styled(format!("{INDENT}❯ "), lit)
        } else {
            Span::raw(format!("{INDENT}  "))
        }];
        let words = format!("{}. {}", at + 1, self.words);
        let mut used = INDENT.len() + 2 + words.width();
        spans.push(if pointed {
            Span::styled(words, lit)
        } else {
            Span::styled(words, kit.style(Role::Text))
        });
        if let Some(note) = self.note {
            spans.push(kit.dim(format!(" {note}")));
            used += 1 + note.width();
        }
        if let Some(description) = &self.description {
            let start = DESCRIPTION_AT.max(used + GAP);
            let room = width.saturating_sub(start);
            if room > 0 {
                spans.push(Span::raw(" ".repeat(start - used)));
                spans.push(kit.dim(cut(description, room)));
            }
        }
        Line::from(spans)
    }
}

fn cut(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + 2 > room {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// `Everything: 562 actions in 6 projects`.
fn scope_words(scope: &ScopeOffer) -> String {
    let actions = plural(scope.actions, "action");
    match &scope.scope {
        DeepScope::Everything => {
            format!(
                "Everything: {actions} in {}",
                plural(scope.projects, "project")
            )
        }
        DeepScope::Project { name } => format!("This project only: {actions} in {name}"),
        DeepScope::Days { days: 1 } => format!("The last day: {actions}"),
        DeepScope::Days { days } => format!("The last {days} days: {actions}"),
        DeepScope::Since { at } => format!("Since {}: {actions}", at.date()),
    }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

/// `under a minute`, `about a minute`, `about 2 minutes`.
pub fn duration_words(seconds: u64) -> String {
    match seconds {
        0..45 => "under a minute".to_owned(),
        45..90 => "about a minute".to_owned(),
        _ => format!("about {} minutes", (seconds + 30) / 60),
    }
}

/// `about 10 seconds` under a minute, else as [`duration_words`] says it.
fn short_duration_words(seconds: u64) -> String {
    match seconds {
        0..5 => "a few seconds".to_owned(),
        5..45 => format!("about {} seconds", (seconds + 2) / 5 * 5),
        _ => duration_words(seconds),
    }
}

/// `55k`, or `900` under a thousand.
fn token_words(tokens: u64) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else {
        format!("{}k", (tokens + 500) / 1_000)
    }
}

/// `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn fits_now(fit: Option<Fit>) -> bool {
    matches!(fit, None | Some(Fit::Fits | Fit::Loaded))
}

/// ` · 18 GB, fits now`, from the model's weights and where it stands in memory.
fn memory_words(bytes: Option<u64>, fit: Option<Fit>) -> String {
    let size = bytes.map(|bytes| format!("{} GB", (bytes as f64 / GB).round() as u64));
    let fit = fit.map(|fit| match fit {
        Fit::Loaded => "loaded now",
        Fit::Fits => "fits now",
        Fit::Tight { .. } => "more than is free now",
        Fit::FitsAlone => "fits once other models are freed",
        Fit::TooBig { .. } => "too big for this machine",
    });
    match (size, fit) {
        (Some(size), Some(fit)) => format!(" · {size}, {fit}"),
        (Some(size), None) => format!(" · {size}"),
        (None, Some(fit)) => format!(" · {fit}"),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
pub(crate) mod sample {
    use argonauts_core::deep_map::CliCount;

    use super::*;

    const GB: u64 = 1_000_000_000;

    fn counts(claude: usize, codex: usize, agy: usize) -> Vec<CliCount> {
        [
            ("claude-code", "Claude Code", claude),
            ("codex", "Codex", codex),
            ("antigravity-cli", "agy", agy),
        ]
        .iter()
        .map(|(cli, name, actions)| CliCount {
            cli: (*cli).to_owned(),
            name: (*name).to_owned(),
            actions: *actions,
        })
        .collect()
    }

    /// The page's numbers: 562 actions in 6 projects, 124 of them in council and 301 in the
    /// last week, Qwen3 Coder 30B and Gemma 3 12B on Ollama, and Haiku 5.5 and GPT 6 Luna.
    pub fn page_offer() -> DeepOffer {
        let scope =
            |scope: DeepScope, actions: usize, projects: usize, seconds: u64, tokens: u64| {
                ScopeOffer {
                    scope,
                    actions,
                    projects,
                    by_cli: counts(428, 115, 19),
                    seconds,
                    tokens,
                }
            };
        let local = |model: &str, name: &str, short: &str, gb: u64| SummarizerOffer {
            model: model.parse().unwrap(),
            name: name.to_owned(),
            short: short.to_owned(),
            place: OfferPlace::ThisMachine {
                bytes: Some(gb * GB),
                fit: Some(Fit::Fits),
            },
        };
        let hosted =
            |model: &str, name: &str, short: &str, cli: &str, plan: &str, provider: &str| {
                SummarizerOffer {
                    model: model.parse().unwrap(),
                    name: name.to_owned(),
                    short: short.to_owned(),
                    place: OfferPlace::Plan {
                        cli: cli.to_owned(),
                        plan: plan.to_owned(),
                        provider: provider.to_owned(),
                    },
                }
            };
        DeepOffer {
            scopes: vec![
                scope(DeepScope::Everything, 562, 6, 112, 55_000),
                scope(
                    DeepScope::Project {
                        name: "council".to_owned(),
                    },
                    124,
                    1,
                    25,
                    12_000,
                ),
                scope(DeepScope::Days { days: 7 }, 301, 4, 60, 29_000),
            ],
            summarizers: vec![
                local(
                    "ollama/qwen3-coder:30b-64k",
                    "Qwen3 Coder 30B on Ollama",
                    "Qwen3 Coder 30B",
                    18,
                ),
                local(
                    "ollama/gemma3:12b-32k",
                    "Gemma 3 12B on Ollama",
                    "Gemma 3 12B",
                    8,
                ),
                hosted(
                    "claude-code/haiku",
                    "Haiku 5.5 on Claude Code",
                    "Haiku 5.5",
                    "claude-code",
                    "Claude",
                    "Anthropic",
                ),
                hosted(
                    "codex/gpt-6-luna",
                    "GPT 6 Luna on Codex",
                    "GPT 6 Luna",
                    "codex",
                    "ChatGPT",
                    "OpenAI",
                ),
            ],
            embedder: EmbedderNote::Download {
                name: "nomic-embed-text on Ollama".to_owned(),
                megabytes: 274,
            },
            mac: true,
            made_by: None,
            named: None,
            keyed_embedder: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Widget};

    use super::sample::page_offer;
    use super::*;
    use crate::overview::golden::{Picture, Region, assert_matches, theme};

    const WIDTH: u16 = 118;

    fn drawn(questions: &DeepQuestions) -> Buffer {
        let theme = theme();
        let kit = Kit::new(&theme);
        let lines = questions.lines(&kit, usize::from(WIDTH));
        let area = Rect::new(0, 0, WIDTH, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines).render(area, &mut buf);
        buf
    }

    fn compare(picture: &str, rows: std::ops::Range<usize>, questions: &DeepQuestions) {
        let buf = drawn(questions);
        crate::overview::golden::keep(&format!("{picture}.questions"), &buf);
        let picture = Picture::load(picture);
        assert_eq!(
            usize::from(buf.area.height),
            rows.len(),
            "the question's rows"
        );
        let region = Region {
            rows,
            columns: 0..usize::from(WIDTH),
            at: (0, 0),
        };
        assert_matches(&picture, &buf, &region, &theme());
    }

    #[test]
    fn what_to_summarize_matches_its_picture() {
        compare(
            "first_visit_questions",
            0..10,
            &DeepQuestions::new(page_offer()),
        );
    }

    #[test]
    fn who_summarizes_matches_its_picture() {
        let mut questions = DeepQuestions::new(page_offer());
        assert_eq!(questions.key(KeyCode::Enter), None);
        compare("first_visit_questions", 13..22, &questions);
    }

    #[test]
    fn a_hosted_summarizer_asks_once_more_as_its_picture_shows() {
        let mut questions = DeepQuestions::new(page_offer());
        questions.key(KeyCode::Enter);
        assert_eq!(questions.key(KeyCode::Char('3')), None);
        compare("hosted_warning", 2..12, &questions);
    }

    #[test]
    fn the_answers_carry_the_scope_the_summarizer_and_the_download() {
        let mut questions = DeepQuestions::new(page_offer());
        questions.key(KeyCode::Down);
        questions.key(KeyCode::Enter);
        questions.key(KeyCode::Enter);
        // The embedder's download is asked once a summarizer is chosen.
        let answer = questions.key(KeyCode::Enter);
        assert_eq!(
            answer,
            Some(DeepAnswer::Start(DeepStart {
                scope: DeepScope::Project {
                    name: "council".to_owned()
                },
                summarizer: "ollama/qwen3-coder:30b-64k".parse().unwrap(),
                download_embedder: true,
                redo: false,
            }))
        );
    }

    #[test]
    fn the_hosted_question_can_take_the_local_model_instead_or_stop() {
        let mut questions = DeepQuestions::new(page_offer());
        questions.key(KeyCode::Enter);
        questions.key(KeyCode::Char('4'));
        questions.key(KeyCode::Char('2'));
        let answer = questions.key(KeyCode::Char('2'));
        let Some(DeepAnswer::Start(start)) = answer else {
            panic!("{answer:?}");
        };
        assert_eq!(start.summarizer.to_string(), "ollama/qwen3-coder:30b-64k");
        assert!(!start.download_embedder);
        let mut questions = DeepQuestions::new(page_offer());
        questions.key(KeyCode::Enter);
        questions.key(KeyCode::Char('3'));
        assert_eq!(questions.key(KeyCode::Char('3')), Some(DeepAnswer::NotNow));
    }

    #[test]
    fn not_now_and_esc_keep_the_label_map_and_esc_steps_back() {
        let mut questions = DeepQuestions::new(page_offer());
        assert_eq!(questions.key(KeyCode::Char('4')), Some(DeepAnswer::NotNow));
        let mut questions = DeepQuestions::new(page_offer());
        questions.key(KeyCode::Enter);
        assert_eq!(questions.key(KeyCode::Esc), None);
        assert_eq!(questions.key(KeyCode::Esc), Some(DeepAnswer::NotNow));
    }

    #[test]
    fn with_the_embedder_here_the_summarizer_is_the_last_question() {
        let mut offer = page_offer();
        offer.embedder = EmbedderNote::Ready {
            name: "nomic-embed-text on Ollama".to_owned(),
        };
        let mut questions = DeepQuestions::new(offer);
        questions.key(KeyCode::Enter);
        let answer = questions.key(KeyCode::Enter);
        assert!(matches!(answer, Some(DeepAnswer::Start(_))), "{answer:?}");
    }

    #[test]
    fn another_summarizer_asks_before_redoing_the_map() {
        let mut offer = page_offer();
        offer.embedder = EmbedderNote::Ready {
            name: "nomic-embed-text on Ollama".to_owned(),
        };
        offer.made_by = Some(argonauts_core::deep_map::MadeBy {
            model: "ollama/gemma3:12b-32k".parse().unwrap(),
            name: "Gemma 3 12B on Ollama".to_owned(),
            actions: 562,
            new_actions: 0,
            prompt_changed: false,
        });
        let mut questions = DeepQuestions::new(offer.clone());
        questions.key(KeyCode::Enter);
        assert_eq!(questions.key(KeyCode::Enter), None, "it asks first");
        let theme = theme();
        let text: Vec<String> = questions
            .lines(&Kit::new(&theme), 118)
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert_eq!(text[0], "  ◆ Summarize the map again?");
        assert!(
            text[1].contains("made by Gemma 3 12B on Ollama"),
            "{text:?}"
        );
        let Some(DeepAnswer::Start(start)) = questions.key(KeyCode::Enter) else {
            panic!("a yes starts it");
        };
        assert!(start.redo);
        let mut questions = DeepQuestions::new(offer.clone());
        questions.key(KeyCode::Enter);
        questions.key(KeyCode::Enter);
        assert_eq!(questions.key(KeyCode::Char('2')), Some(DeepAnswer::NotNow));
        // The map's own summarizer goes straight on.
        let mut questions = DeepQuestions::new(offer);
        questions.key(KeyCode::Enter);
        let answer = questions.key(KeyCode::Char('2'));
        assert!(
            matches!(answer, Some(DeepAnswer::Start(ref start)) if !start.redo),
            "{answer:?}"
        );
    }

    fn made_by_qwen(prompt_changed: bool) -> argonauts_core::deep_map::MadeBy {
        argonauts_core::deep_map::MadeBy {
            model: "ollama/qwen3-coder:30b-64k".parse().unwrap(),
            name: "Qwen3 Coder 30B on Ollama".to_owned(),
            actions: 562,
            new_actions: 37,
            prompt_changed,
        }
    }

    #[test]
    fn an_update_with_a_newer_prompt_asks_before_making_the_map_again() {
        let mut offer = page_offer();
        offer.embedder = EmbedderNote::Ready {
            name: "nomic-embed-text on Ollama".to_owned(),
        };
        offer.made_by = Some(made_by_qwen(true));
        let mut questions = DeepQuestions::update(offer.clone());
        assert_eq!(questions.key(KeyCode::Enter), None, "Update asks first");
        let theme = theme();
        let text: Vec<String> = questions
            .lines(&Kit::new(&theme), 118)
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert_eq!(text[0], "  ◆ Summarize the map again?");
        assert!(text[1].contains("older prompt"), "{text:?}");
        let Some(DeepAnswer::Start(start)) = questions.key(KeyCode::Enter) else {
            panic!("a yes starts it");
        };
        assert!(start.redo);
        assert_eq!(start.scope, DeepScope::Everything);
        assert_eq!(start.summarizer.to_string(), "ollama/qwen3-coder:30b-64k");
        // With the prompt as it was, the map's own summarizer adds the new actions.
        offer.made_by = Some(made_by_qwen(false));
        let mut questions = DeepQuestions::update(offer.clone());
        let answer = questions.key(KeyCode::Enter);
        assert!(
            matches!(answer, Some(DeepAnswer::Start(ref start)) if !start.redo),
            "{answer:?}"
        );
        // The settings now name another summarizer: that one would make it again, which asks.
        offer.named = Some("ollama/gemma3:12b-32k".parse().unwrap());
        let mut questions = DeepQuestions::update(offer);
        assert_eq!(questions.key(KeyCode::Enter), None);
        let Some(DeepAnswer::Start(start)) = questions.key(KeyCode::Enter) else {
            panic!("a yes starts it");
        };
        assert!(start.redo);
        assert_eq!(start.summarizer.to_string(), "ollama/gemma3:12b-32k");
    }

    #[test]
    fn with_no_model_to_summarize_with_d_says_so_and_takes_no_key_but_esc() {
        let mut offer = page_offer();
        offer.summarizers.clear();
        for mut questions in [
            DeepQuestions::new(offer.clone()),
            DeepQuestions::update(offer.clone()),
        ] {
            let theme = theme();
            let text: Vec<String> = questions
                .lines(&Kit::new(&theme), 118)
                .iter()
                .map(|line| line.to_string())
                .collect();
            assert_eq!(text[0], "  ◆ No model to summarize with");
            for code in [KeyCode::Up, KeyCode::Down, KeyCode::Char('1')] {
                assert_eq!(questions.key(code), None, "{code:?}");
            }
            assert_eq!(questions.key(KeyCode::Enter), Some(DeepAnswer::NotNow));
            assert_eq!(questions.key(KeyCode::Esc), Some(DeepAnswer::NotNow));
        }
        offer.made_by = Some(made_by_qwen(false));
        let mut questions = DeepQuestions::update(offer);
        assert_eq!(questions.key(KeyCode::Down), None);
        assert_eq!(questions.key(KeyCode::Esc), Some(DeepAnswer::NotNow));
    }

    #[test]
    fn a_screen_reader_hears_the_first_question_and_its_numbered_answers() {
        let said = spoken(&page_offer());
        assert_eq!(said[0], "Summarize your agents' actions?");
        assert!(
            said.contains(
                &"1. Everything: 562 actions in 6 projects, about 2 minutes with your local model"
                    .to_owned()
            ),
            "{said:#?}"
        );
        assert_eq!(
            said.last().unwrap(),
            "Enter to choose · Esc to keep the label map"
        );
    }

    /// The update starts from the offer's time for the summarizer it timed, auto's, and
    /// waits for the app's estimate for another kind.
    #[test]
    fn an_update_starts_with_the_offers_estimate_for_the_summarizer_it_timed() {
        let questions = DeepQuestions::new(page_offer());
        let start = |summarizer: &str| DeepStart {
            scope: DeepScope::Everything,
            summarizer: summarizer.parse().unwrap(),
            download_embedder: false,
            redo: false,
        };
        let local = questions.started(&start("ollama/qwen3-coder:30b-64k"));
        assert_eq!((local.done, local.total), (0, 562));
        assert_eq!(local.seconds_left, Some(page_offer().scopes[0].seconds));
        assert_eq!(local.summarizer, "Qwen3 Coder 30B on Ollama");
        let hosted = questions.started(&start("claude-code/haiku"));
        assert_eq!(hosted.seconds_left, None, "the offer timed the local model");
        assert_eq!(hosted.summarizer, "Haiku 5.5 on Claude Code");
    }

    #[test]
    fn durations_read_as_people_say_them() {
        assert_eq!(duration_words(25), "under a minute");
        assert_eq!(duration_words(60), "about a minute");
        assert_eq!(duration_words(112), "about 2 minutes");
        assert_eq!(token_words(55_200), "55k");
        assert_eq!(token_words(640), "640");
        assert_eq!(short_duration_words(3), "a few seconds");
        assert_eq!(short_duration_words(7), "about 5 seconds");
        assert_eq!(short_duration_words(60), "about a minute");
    }
}
