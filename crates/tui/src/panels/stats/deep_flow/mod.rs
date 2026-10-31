//! `d` and `u` on the Behaviors map, from the key to the map it makes. With no Summaries kept,
//! `d` asks the app what it can offer and opens the questions; their answer starts the update.
//! With Summaries kept, `d` only switches between them and the labels. `u` on Summaries, while
//! actions wait, has the app add them with the map's own summarizer, which asks first only
//! when that model is a plan's or isn't answering. While an update runs, the line under the
//! sub-tabs says who summarizes, a line under it how far it got, `x` stops it, and Esc leaves
//! it going in the background, as every panel's Esc does. One that stopped or failed says so
//! under the headline until you leave Behaviors. A model chosen from Open models that isn't
//! here yet downloads first, its progress in the same lines.
//!
//! The map's own view holds one of these, tells it what the app reported, and draws what it
//! gives back in its place.

use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::behaviors::deep_map::{DeepDone, DeepOffer, DeepProgress};
use argonauts_core::settings::behaviors::BehaviorMap;
use crossterm::event::KeyCode;
use ratatui::text::Line;

use super::deep_lines;
use crate::base::kit::Kit;
use crate::frame::state::DeepUpdate;
use crate::panels::stats::deep_map::{DeepAnswer, DeepQuestions};

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
    /// How the last update that left the map as it was ended, said until you leave Behaviors.
    kept: Option<Kept>,
}

/// An update that stopped or failed before its map was whole, or whose map had fewer dots,
/// so the map stayed as it was.
#[derive(Debug, Clone, PartialEq)]
struct Kept {
    done: DeepDone,
    summarizer: String,
    total: usize,
    redo: bool,
}

impl DeepFlow {
    /// What the app reported. An update that ended clears the one this tab started.
    pub fn sync(&mut self, update: &DeepUpdate) {
        if update.running.is_some() || update.ended != self.update.ended {
            // The app took `u` itself, so no offer comes for it.
            if matches!(self.asked, Some(Shown::Summaries { .. })) {
                self.asked = None;
            }
            self.kept = None;
        }
        if update.ended != self.update.ended {
            // A panel opened after the update ended didn't see it, so says nothing of it.
            let watched = self.update.running.is_some() || self.started.is_some();
            let progress = self.progress();
            self.kept = update
                .last
                .as_ref()
                .filter(|done| {
                    (done.stopped || done.failed.is_some() || done.fewer.is_some())
                        && done.new_places == 0
                })
                .filter(|_| watched)
                .map(|done| Kept {
                    done: done.clone(),
                    summarizer: progress.as_ref().map_or_else(String::new, |p| {
                        if p.short.is_empty() {
                            p.summarizer.clone()
                        } else {
                            p.short.clone()
                        }
                    }),
                    total: progress.as_ref().map_or(0, |p| p.total),
                    redo: progress.as_ref().is_some_and(|p| p.redo),
                });
            self.started = None;
        }
        if &self.update != update {
            self.update = update.clone();
        }
    }

    /// You left Behaviors, the Overview tab or the panel, so what the last update kept is said
    /// no more.
    pub fn leave(&mut self) {
        self.kept = None;
    }

    /// `d` on the map: Summaries from the labels, their setting up where none are kept, and
    /// the labels from Summaries. It asks nothing once Summaries are kept.
    pub fn d(&mut self, shown: Shown) -> DeepKey {
        if self.asking() {
            return self.key(KeyCode::Char('d')).unwrap_or(DeepKey::Moved);
        }
        match shown {
            Shown::Labels { kept: true } => DeepKey::Show(BehaviorMap::Summaries),
            Shown::Summaries { .. } => DeepKey::Show(BehaviorMap::Labels),
            Shown::Labels { kept: false } => {
                self.asked = Some(shown);
                DeepKey::Ask(ViewRequest::DeepOffer { start: None })
            }
        }
    }

    /// `u` on Summaries while actions wait, which `by` summarized. The app starts it, or sends
    /// the offer back for the questions; until it reports, the tab says it started.
    pub fn u(&mut self, shown: Shown, by: &str) -> Option<DeepKey> {
        if self.asking() {
            return self.key(KeyCode::Char('u'));
        }
        let Shown::Summaries { unsummarized } = shown else {
            return None;
        };
        if unsummarized == 0 || self.running() {
            return None;
        }
        self.asked = Some(shown);
        self.kept = None;
        self.started = Some(DeepProgress {
            done: 0,
            total: unsummarized,
            seconds_left: None,
            summarizer: by.to_owned(),
            short: by.to_owned(),
            redo: false,
            downloading: false,
        });
        Some(DeepKey::Send(UiCommand::DeepUpdate))
    }

    /// The offer `d` asked for opens its questions.
    pub fn accept(&mut self, offer: &DeepOffer) {
        let Some(shown) = self.asked.take() else {
            return;
        };
        if let Some(waiting) = self.questions.as_ref().filter(|q| q.starting()) {
            self.questions = Some(waiting.restart(offer.clone()));
            return;
        }
        self.questions = Some(match shown {
            Shown::Summaries { .. } => {
                // The app asks rather than starting, so nothing started yet.
                self.started = None;
                DeepQuestions::update(offer.clone())
            }
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
            KeyCode::Char('d' | 'u') => !self.running(),
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
                Some(DeepAnswer::Start(start)) => {
                    self.started = Some(questions.started(&start));
                    self.questions = None;
                    DeepKey::Send(UiCommand::DeepStart { start })
                }
                Some(DeepAnswer::StartServer(runtime)) => {
                    // The offer after the start reopens these where they were.
                    self.asked = Some(match questions.updating() {
                        true => Shown::Summaries { unsummarized: 0 },
                        false => Shown::Labels { kept: false },
                    });
                    DeepKey::Ask(ViewRequest::DeepOffer {
                        start: Some(runtime),
                    })
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
        let first = match progress.downloading {
            true => deep_lines::downloading(kit, &progress.summarizer),
            false => deep_lines::summarizing(kit, progress.total, &progress.summarizer),
        };
        Some(vec![
            first,
            Line::default(),
            deep_lines::progress(kit, &progress),
        ])
    }

    /// Under the headline, after an update that left the map as it was: why, and that the
    /// summaries it wrote are kept for the next try.
    pub fn kept_lines(&self, kit: &Kit<'_>) -> Option<Vec<Line<'static>>> {
        let kept = self.kept.as_ref()?;
        Some(deep_lines::kept(
            kit,
            &kept.done,
            &kept.summarizer,
            kept.total,
            kept.redo,
        ))
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
mod tests;
