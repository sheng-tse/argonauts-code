//! The Behaviors rows' pickers: who writes the summaries of Summaries, and what turns them into
//! vectors. Their choices come from the offer `d` asks with, so the summarizers read as "Summarize
//! with" lists them, and a hosted one asks once more in that question's words. A change waits
//! for the next `d`, which asks before it makes the map again.

use argonauts_core::deep_map::{DeepOffer, EmbedderNote};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::behaviors::{AUTO, WORD_VECTORS};
use crossterm::event::KeyEvent;

use crate::bottom::{Picker, PickerEvent, PickerRow};
use crate::format;
use crate::questions::deep_map;

/// Which Behaviors row the picker changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Summarizer,
    Embedder,
}

/// The embedding model auto downloads to Ollama, which the embedder's picker names.
const NOMIC: &str = "nomic-embed-text";

/// A Behaviors row's picker while it shows.
#[derive(Debug, Clone)]
pub struct Picking {
    pub pick: Pick,
    pub picker: Picker,
    /// The value the row has, which its row marks and Esc keeps.
    value: String,
    offer: Option<DeepOffer>,
    /// What each of the list's rows saves, as TOML; `None` for one that can't be picked here.
    saves: Vec<Option<String>>,
    /// The hosted summarizer asked about once more, by its place among the offer's.
    hosted: Option<usize>,
}

/// What a key in the picker came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// The picker stays.
    Open,
    /// Save this TOML for the row, which names it as `label`.
    Save { value: String, label: String },
    /// The row keeps its value.
    Keep,
}

impl Picking {
    /// The picker as it opens, while the offer is asked for.
    pub fn new(pick: Pick, value: &str, vim: bool) -> Picking {
        let (title, about) = title_and_about(pick);
        let mut picker = Picker::new(title, about);
        picker.placeholder = "Asking this computer's model servers…".to_owned();
        picker.vim = vim;
        Picking {
            pick,
            picker,
            value: value.to_owned(),
            offer: None,
            saves: Vec::new(),
            hosted: None,
        }
    }

    /// The offer the picker asked for, which fills its rows once they are next drawn.
    pub fn accept(&mut self, offer: &DeepOffer) {
        if self.offer.is_none() {
            self.offer = Some(offer.clone());
        }
    }

    /// Fills the rows from the offer once it has come, in the names the screen uses.
    pub fn sync(&mut self, names: &DisplayNames) {
        if self.offer.is_some() && self.saves.is_empty() && self.hosted.is_none() {
            self.list(names);
        }
    }

    /// The list of choices, the pointer on the one the row has.
    fn list(&mut self, names: &DisplayNames) {
        let Some(offer) = &self.offer else {
            return;
        };
        let choices = match self.pick {
            Pick::Summarizer => summarizers(offer, &self.value, names),
            Pick::Embedder => embedders(offer, &self.value, names),
        };
        let current = choices.iter().position(|choice| choice.row.current);
        let (rows, saves): (Vec<PickerRow>, Vec<Option<String>>) = choices
            .into_iter()
            .map(|choice| (choice.row, choice.saves))
            .unzip();
        self.picker.set_rows(rows);
        self.saves = saves;
        if let Some(current) = current {
            self.picker.point_at(current);
        }
    }

    pub fn key(&mut self, key: KeyEvent, names: &DisplayNames) -> Picked {
        self.sync(names);
        match self.picker.handle_key(key) {
            PickerEvent::Picked(at) => self.picked(at),
            PickerEvent::Closed | PickerEvent::Left if self.hosted.is_some() => {
                let hosted = self.hosted.take();
                self.back_to_list(hosted, names);
                Picked::Open
            }
            PickerEvent::Closed => Picked::Keep,
            _ => Picked::Open,
        }
    }

    fn picked(&mut self, at: usize) -> Picked {
        let Some(offer) = self.offer.clone() else {
            return Picked::Open;
        };
        let summarizer = |at: usize| {
            let summarizer = &offer.summarizers[at];
            Picked::Save {
                value: toml_text(&summarizer.model.to_string()),
                label: summarizer.name.clone(),
            }
        };
        if let Some(chosen) = self.hosted {
            return match (at, deep_map::first_local(&offer)) {
                (0, _) => summarizer(chosen),
                (1, Some(local)) => summarizer(local),
                _ => Picked::Keep,
            };
        }
        let Some(Some(saves)) = self.saves.get(at).cloned() else {
            return Picked::Open;
        };
        let label = self.picker.rows[at].label.clone();
        let hosted = (self.pick == Pick::Summarizer)
            .then(|| {
                offer.summarizers.iter().position(|summarizer| {
                    !summarizer.local() && toml_text(&summarizer.model.to_string()) == saves
                })
            })
            .flatten();
        match hosted {
            Some(chosen) if !same(&saves, &self.value) => {
                self.ask_hosted(&offer, chosen, offer.scopes.first());
                Picked::Open
            }
            _ => Picked::Save {
                value: saves,
                label,
            },
        }
    }

    /// A hosted summarizer asks once more, as the `d` flow's question does, for every action,
    /// and before any is recorded, for the ones recorded from then on.
    fn ask_hosted(
        &mut self,
        offer: &DeepOffer,
        chosen: usize,
        scope: Option<&argonauts_core::deep_map::ScopeOffer>,
    ) {
        let (title, body) = deep_map::hosted_question(offer, chosen, scope);
        self.picker.title = title;
        self.picker.about = body.join(" ");
        let rows = deep_map::hosted_answers(offer, chosen)
            .into_iter()
            .map(|(words, description)| PickerRow::new(words, description.unwrap_or_default()))
            .collect();
        self.picker.set_rows(rows);
        self.picker.wide_labels = true;
        self.picker.esc_hint = "Esc to go back".to_owned();
        self.hosted = Some(chosen);
    }

    /// Back from the hosted question to the list, on the summarizer it asked about.
    fn back_to_list(&mut self, hosted: Option<usize>, names: &DisplayNames) {
        let (title, about) = title_and_about(self.pick);
        self.picker.title = title.to_owned();
        self.picker.about = about.to_owned();
        self.picker.esc_hint = String::new();
        self.picker.wide_labels = false;
        self.list(names);
        let model = hosted
            .and_then(|chosen| self.offer.as_ref()?.summarizers.get(chosen))
            .map(|summarizer| toml_text(&summarizer.model.to_string()));
        if let Some(at) = self.saves.iter().position(|saves| *saves == model) {
            self.picker.point_at(at);
        }
    }
}

/// The list's title, and what the choice does under it.
fn title_and_about(pick: Pick) -> (&'static str, &'static str) {
    match pick {
        Pick::Summarizer => (
            "Summarize with",
            "One model writes every summary, so the map compares the agents, not their \
             summarizers. The next d asks before it summarizes a map again.",
        ),
        Pick::Embedder => (
            "Embed with",
            "It turns each summary into a vector, so summaries that mean the same land \
             together. The next d asks before it places a map again.",
        ),
    }
}

/// A row of the list and what it saves.
struct Choice {
    row: PickerRow,
    saves: Option<String>,
}

impl Choice {
    fn new(label: impl Into<String>, description: impl Into<String>, saves: &str) -> Choice {
        Choice {
            row: PickerRow::new(label, description),
            saves: Some(toml_text(saves)),
        }
    }
}

/// Auto first, saying what it picks now, then each model the offer's "Summarize with" lists:
/// those served on this computer, then each CLI's cheapest on your plan. A model the row names
/// that isn't offered here now keeps a row of its own, so the list still shows what it is.
fn summarizers(offer: &DeepOffer, value: &str, names: &DisplayNames) -> Vec<Choice> {
    let auto_picks = offer
        .made_by
        .as_ref()
        .map(|made_by| made_by.name.clone())
        .or_else(|| offer.summarizers.first().map(|first| first.name.clone()));
    let auto_words = match auto_picks {
        Some(name) => format!("picks {name} now"),
        None => "no model to summarize with here now".to_owned(),
    };
    let tokens = offer.scopes.first().map_or(0, |scope| scope.tokens);
    let mut choices = vec![Choice::new("Auto", auto_words, AUTO)];
    choices.extend(
        offer
            .summarizers
            .iter()
            .zip(deep_map::summarizer_descriptions(offer, tokens))
            .map(|(summarizer, description)| {
                Choice::new(
                    summarizer.name.clone(),
                    description,
                    &summarizer.model.to_string(),
                )
            }),
    );
    with_named(choices, value, names, "not served or installed here now")
}

/// Auto, saying what it picks now; nomic-embed-text on Ollama; the endpoint with your key; and
/// word vectors.
fn embedders(offer: &DeepOffer, value: &str, names: &DisplayNames) -> Vec<Choice> {
    let here = deep_map::here(offer);
    let auto_words = match &offer.embedder {
        EmbedderNote::Ready { name } => format!("picks {name} now"),
        EmbedderNote::Download { name, megabytes } => {
            format!("picks {name}, once its {megabytes} MB are downloaded")
        }
        EmbedderNote::Words => "picks word vectors now".to_owned(),
    };
    let ollama = names.cli("ollama");
    let nomic_label = format!("{NOMIC} on {ollama}");
    let nomic = match &offer.embedder {
        EmbedderNote::Download { megabytes, .. } => Choice::new(
            nomic_label,
            format!("free · on this {here} · {megabytes} MB, downloaded once d asks"),
            &format!("ollama/{NOMIC}"),
        ),
        EmbedderNote::Ready { name } if name.starts_with(NOMIC) => Choice::new(
            nomic_label,
            format!("free · on this {here}"),
            &format!("ollama/{NOMIC}"),
        ),
        // The note names nomic-embed-text only where Ollama runs, so here it doesn't, and the
        // next d couldn't use it.
        EmbedderNote::Ready { .. } | EmbedderNote::Words => {
            let mut row = PickerRow::new(nomic_label, format!("{ollama} isn't running here"));
            row.warning = Some(format!(
                "{ollama} isn't running on this {here}. Start it, and pick this again: d \
                 downloads {NOMIC} once it asks."
            ));
            Choice { row, saves: None }
        }
    };
    let mut choices = vec![Choice::new("Auto", auto_words, AUTO), nomic];
    choices.push(match &offer.keyed_embedder {
        Some(keyed) => Choice::new(
            keyed.name.clone(),
            "an OpenAI-compatible endpoint with your key",
            &keyed.model.to_string(),
        ),
        None => {
            let mut row =
                PickerRow::new("An OpenAI-compatible endpoint", "none with a key is set up");
            row.warning = Some(
                "No endpoint with your key is set up. Add one under [endpoints] with its \
                 api_key_env and allow_api_billing."
                    .to_owned(),
            );
            Choice { row, saves: None }
        }
    });
    choices.push(Choice::new(
        "Word vectors",
        "placed by the summaries' words alone",
        WORD_VECTORS,
    ));
    with_named(choices, value, names, "not set up here now")
}

/// Marks the choice the row's value saves. A value none of them saves gets a row of its own
/// after Auto, named as the model it names.
fn with_named(
    mut choices: Vec<Choice>,
    value: &str,
    names: &DisplayNames,
    missing: &str,
) -> Vec<Choice> {
    let value = if format::setting_value(value).trim().is_empty() {
        toml_text(AUTO)
    } else {
        value.to_owned()
    };
    match choices.iter_mut().find(|choice| {
        choice
            .saves
            .as_deref()
            .is_some_and(|saves| same(saves, &value))
    }) {
        Some(choice) => choice.row.current = true,
        None => {
            let named = format::setting_value(&value);
            let label = named
                .parse::<ModelRef>()
                .map_or_else(|_| named.clone(), |model| names.worker(&model));
            let mut choice = Choice::new(label, missing, &named);
            choice.row.current = true;
            choices.insert(1, choice);
        }
    }
    choices
}

/// `text` as a TOML string, as the settings save it.
fn toml_text(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

/// Whether two TOML texts hold the same value, however each was quoted.
fn same(left: &str, right: &str) -> bool {
    format::setting_value(left) == format::setting_value(right)
}
