//! `d` on the Behaviors map, from the key to the map it makes. With no Summaries kept, `d` asks
//! the app what it can offer and opens the questions; their answer starts the update. With
//! Summaries kept, `d` on the label map switches to them, and on Summaries goes back to the
//! labels, or first asks whether to add the actions they don't have yet; with nothing to add
//! them with, leaving that question shows the labels, so `d` is never a dead end. While an
//! update runs, the line under the sub-tabs says who summarizes, a line under it how far it
//! got, `x` stops it, and Esc leaves it going in the background, as every panel's Esc does.
//!
//! The map's own view holds one of these, tells it what the app reported, and draws what it
//! gives back in its place.

use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::deep_map::{DeepOffer, DeepProgress};
use argonauts_core::settings::behaviors::BehaviorMap;
use crossterm::event::KeyCode;
use ratatui::text::Line;

use super::deep_lines;
use crate::kit::Kit;
use crate::questions::deep_map::{DeepAnswer, DeepQuestions};
use crate::state::DeepUpdate;

/// The map on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// The label map, and whether Summaries are kept to switch to.
    Labels { kept: bool },
    /// Summaries, and the actions they have no summary of yet.
    Summaries { unsummarized: usize },
}

/// What a key asks of the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum DeepKey {
    /// Ask the app, such as for the offer.
    Ask(ViewRequest),
    /// Tell the app: start or stop an update.
    Send(UiCommand),
    /// Show the label map or Summaries.
    Show(BehaviorMap),
    /// Only the tab's own look changed.
    Moved,
}

#[derive(Debug, Clone, Default)]
pub struct DeepFlow {
    /// `d` asked for the offer; what it opens waits for it.
    asked: Option<Shown>,
    questions: Option<DeepQuestions>,
    /// The update this tab started, as it stood at its start, until the app says where it is.
    started: Option<DeepProgress>,
    /// What the app last reported of its updates.
    update: DeepUpdate,
}

impl DeepFlow {
    /// What the app reported. An update that ended clears the one this tab started.
    pub fn sync(&mut self, update: &DeepUpdate) {
        if update.ended != self.update.ended {
            self.started = None;
        }
        if &self.update != update {
            self.update = update.clone();
        }
    }

    /// `d` on the map.
    pub fn d(&mut self, shown: Shown) -> DeepKey {
        match shown {
            Shown::Labels { kept: true } => DeepKey::Show(BehaviorMap::Summaries),
            Shown::Summaries { unsummarized: 0 } => DeepKey::Show(BehaviorMap::Labels),
            Shown::Labels { kept: false } | Shown::Summaries { .. } => {
                self.asked = Some(shown);
                DeepKey::Ask(ViewRequest::DeepOffer)
            }
        }
    }

    /// The offer `d` asked for opens its questions.
    pub fn accept(&mut self, offer: &DeepOffer) {
        let Some(shown) = self.asked.take() else {
            return;
        };
        self.questions = Some(match shown {
            Shown::Summaries { .. } => DeepQuestions::update(offer.clone()),
            Shown::Labels { .. } => DeepQuestions::new(offer.clone()),
        });
    }

    /// The questions show in the tab's place.
    pub fn asking(&self) -> bool {
        self.questions.is_some()
    }

    /// An update runs: this tab's, or one the app started after a run.
    pub fn running(&self) -> bool {
        self.started.is_some() || self.update.running.is_some()
    }

    /// The keys this flow answers before the map's own.
    pub fn claims(&self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('d') => !self.running(),
            KeyCode::Char('x') => self.running(),
            _ => self.asking(),
        }
    }

    /// A key it claimed. `d` itself goes to [`DeepFlow::d`], with the map on screen.
    pub fn key(&mut self, code: KeyCode) -> Option<DeepKey> {
        if let Some(questions) = self.questions.as_mut() {
            let answer = questions.key(code);
            return Some(match answer {
                None => DeepKey::Moved,
                Some(DeepAnswer::NotNow) => {
                    self.questions = None;
                    DeepKey::Moved
                }
                Some(DeepAnswer::LabelMap) => {
                    self.questions = None;
                    DeepKey::Show(BehaviorMap::Labels)
                }
                Some(DeepAnswer::Start(start)) => {
                    self.started = Some(questions.started(&start));
                    self.questions = None;
                    DeepKey::Send(UiCommand::DeepStart { start })
                }
            });
        }
        (code == KeyCode::Char('x') && self.running()).then_some(DeepKey::Send(UiCommand::DeepStop))
    }

    /// The questions' rows, while they show.
    pub fn questions(&self, kit: &Kit<'_>, width: usize) -> Option<Vec<Line<'static>>> {
        Some(self.questions.as_ref()?.lines(kit, width))
    }

    /// While an update runs, in the line under the sub-tabs' place: that line, a blank row,
    /// and the progress.
    pub fn above_map(&self, kit: &Kit<'_>) -> Option<Vec<Line<'static>>> {
        let progress = self.progress()?;
        Some(vec![
            deep_lines::summarizing(kit, progress.total, &progress.summarizer),
            Line::default(),
            deep_lines::progress(kit, &progress),
        ])
    }

    /// While an update runs, under the map in the list's place.
    pub fn under_map(&self, kit: &Kit<'_>) -> Option<Line<'static>> {
        self.running().then(|| deep_lines::arriving(kit))
    }

    /// Where the update is: the app's word, else none done of what this tab started. Until
    /// the app has an estimate of its own, the offer's says how long is left of what remains.
    fn progress(&self) -> Option<DeepProgress> {
        let Some(running) = &self.update.running else {
            return self.started.clone();
        };
        let mut progress = running.clone();
        if progress.seconds_left.is_none()
            && let Some(estimate) = self.started.as_ref().and_then(|s| s.seconds_left)
        {
            let left = progress.total.saturating_sub(progress.done) as u64;
            progress.seconds_left = Some(estimate * left / progress.total.max(1) as u64);
        }
        Some(progress)
    }
}

#[cfg(test)]
#[path = "deep_flow_tests.rs"]
mod tests;
