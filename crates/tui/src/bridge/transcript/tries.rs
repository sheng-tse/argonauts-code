//! A run whose job no agent could take waited and tried again on a timer, as runs did before
//! they stopped fully instead. Its wait prints once, as one item that says which job nothing
//! could take, why, and who stopped for it:
//!
//! ```text
//! ✢ The run waits · nothing could take propose:p3's place · it tries again at 1:58 AM
//!   ⎿  Antigravity CLI can't run Gemini 3.6 Flash (Low), the model this run started with
//!      propose:p1 and propose:p2 stopped, their work so far kept
//! ```
//!
//! Drawn again, each later try that ended in the same wait is one line, `✢ Tried again at 1:59 AM
//! · the same error`, its items kept for the transcript overlay. The record doesn't say what
//! woke a try, so the line doesn't either.

use argonauts_core::events::run::WaitReason;
use argonauts_core::events::session::{EndReason, SessionEnd};
use argonauts_core::log::{GAVE_NO_ANSWER, NOBODY_TO_FALL_BACK_TO};
use argonauts_core::names::SessionKey;
use argonauts_core::{Note, RunId};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::bridge::agents::reason_words;
use crate::cell::{Cell, HistoryCell, Notice, Tone};
use crate::format::WallClock;
use crate::render::RenderCtx;
use crate::text::{marks, wrap_spans};
use crate::theme::Role;

use super::{Cx, emit};

/// The glyph of a run that waits on a timer, as a reset's row has it.
const WAITS: &str = "✢";

/// What a run's wait needs from the try that led to it, and what the try after it is compared
/// with.
#[derive(Debug, Default)]
pub(super) struct Tries {
    /// What the wait that printed last said nobody could fill.
    waited: Option<String>,
    /// When the try now under way woke, while its items wait to be folded or printed.
    trying: Option<OffsetDateTime>,
    /// The job no agent could take, as the note that it got no answer names it.
    job: Option<String>,
    /// Each job's first failure since the last wait: its label, CLI and why.
    failed: Vec<(String, String, EndReason)>,
    /// The agents stopped for the wait since the last one, by label.
    cut: Vec<String>,
    /// How many cells had gone to scrollback after the last try's line, so the next one
    /// stacks right under it when nothing printed between.
    line_ended_at: Option<usize>,
}

impl Tries {
    /// A note that a job got no answer, or that nobody could take one, begins the stretch the
    /// wait's item says in its place. Drawn again, the stretch waits off scrollback until the
    /// wait comes; true when it does.
    pub(super) fn note(&mut self, note: &Note, cx: &mut Cx<'_>) -> bool {
        let text = note.text.trim();
        let no_answer = text
            .split_once(GAVE_NO_ANSWER)
            .filter(|(_, why)| why.starts_with(NOBODY_TO_FALL_BACK_TO));
        if let Some((job, _)) = no_answer {
            self.job.get_or_insert_with(|| job.trim().to_owned());
        }
        let begins = no_answer.is_some() || text.starts_with(NOBODY_TO_FALL_BACK_TO);
        begins && hold(cx)
    }

    /// The run woke from a wait that printed. Drawn again, the try waits off scrollback until
    /// it is known to end in the same wait. Says whether it waits, and whether the record's
    /// cause can't be trusted to say what woke the run.
    pub(super) fn woke(&mut self, at: OffsetDateTime, cx: &mut Cx<'_>) -> (bool, bool) {
        if self.waited.is_none() {
            return (false, false);
        }
        self.since_the_wait();
        let held = hold(cx);
        if held {
            self.trying = Some(at);
        }
        (held, true)
    }

    /// Keeps a job's first failure and the agents stopped for the wait, for the wait's item.
    pub(super) fn session_ended(
        &mut self,
        run: &RunId,
        key: &SessionKey,
        end: &SessionEnd,
        cx: &Cx<'_>,
    ) {
        let Some(session) = cx
            .state
            .runs
            .get(run)
            .and_then(|state| state.sessions.get(key))
        else {
            return;
        };
        let Some(label) = session.label.as_deref().map(str::trim) else {
            return;
        };
        match &end.reason {
            EndReason::Completed | EndReason::Stopped => {}
            EndReason::Paused => {
                if !self.cut.iter().any(|cut| cut == label) {
                    self.cut.push(label.to_owned());
                }
            }
            reason => {
                if !self.failed.iter().any(|(job, _, _)| job == label) {
                    self.failed
                        .push((label.to_owned(), session.agent_id.clone(), reason.clone()));
                }
            }
        }
    }

    /// The run waits for nobody and tries again on a timer. The first such wait prints its
    /// item; a try that woke from it and ended in the same wait folds into one line. False when
    /// the wait is another kind, which prints as it does.
    pub(super) fn wait(&mut self, reason: &WaitReason, clock: &WallClock, cx: &mut Cx<'_>) -> bool {
        let WaitReason::NoFallback {
            detail,
            until: Some(until),
            stop: None,
        } = reason
        else {
            self.waited = None;
            self.trying = None;
            return false;
        };
        match self.trying.take() {
            Some(at) if self.waited.as_deref() == Some(detail.as_str()) => {
                cx.model.history.fold_withheld();
                let continues = self.line_ended_at == Some(cx.model.history.committed());
                emit(
                    cx,
                    Cell::Custom(Box::new(TriedAgain {
                        text: format!("Tried again at {} · the same error", clock.clock(at)),
                        continues,
                    })),
                );
                self.line_ended_at = Some(cx.model.history.committed());
                self.since_the_wait();
                return true;
            }
            // A try that ended in another wait prints in full before it.
            Some(_) => cx.model.history.print_withheld(),
            // The notes and stopped agents before the first wait are what its item says.
            None => cx.model.history.fold_withheld(),
        }
        emit(cx, self.item(*until, clock, cx));
        self.waited = Some(detail.clone());
        self.line_ended_at = None;
        self.since_the_wait();
        true
    }

    /// `✢ The run waits · nothing could take propose:p3's place · it tries again at 1:58 AM`,
    /// with why the job failed and who stopped for it. The time is the record's own, to the
    /// minute, as every time in a past drawn again reads.
    fn item(&self, until: OffsetDateTime, clock: &WallClock, cx: &Cx<'_>) -> Notice {
        let job = self
            .job
            .clone()
            .or_else(|| self.failed.first().map(|(job, _, _)| job.clone()));
        let place = match &job {
            Some(job) => format!("nothing could take {job}'s place"),
            None => "nothing could take a job the run needs".to_owned(),
        };
        let mut notice = Notice::new(
            Tone::Warning,
            format!(
                "The run waits · {place} · it tries again at {}",
                clock.clock(until)
            ),
        )
        .with_glyph(WAITS)
        .with_dim_details();
        let failed = self
            .failed
            .iter()
            .find(|(label, _, _)| Some(label) == job.as_ref());
        if let Some((_, agent, reason)) = failed {
            let names = &cx.state.names;
            let why = match reason {
                EndReason::ModelUnavailable { model } => Some(format!(
                    "{} can't run {}, the model this run started with",
                    names.cli(agent),
                    names
                        .model(agent, Some(model))
                        .unwrap_or_else(|| model.clone())
                )),
                reason => reason_words(reason, agent, names),
            };
            if let Some(why) = why {
                notice = notice.with_detail(why);
            }
        }
        if !self.cut.is_empty() {
            let mut cut = self.cut.clone();
            cut.sort();
            notice = notice.with_detail(format!(
                "{} stopped, their work so far kept",
                argonauts_core::text::and_list(&cut)
            ));
        }
        notice
    }

    fn since_the_wait(&mut self) {
        self.job = None;
        self.failed.clear();
        self.cut.clear();
    }
}

/// Only a past drawn again waits off scrollback: live, each item prints as it comes.
fn hold(cx: &mut Cx<'_>) -> bool {
    if !cx.drawing_past() {
        return false;
    }
    cx.model.history.withhold();
    true
}

/// `✢ Tried again at 12:32 AM · the same error`, dim, right under the line before it when that
/// one was a try too.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TriedAgain {
    text: String,
    continues: bool,
}

impl HistoryCell for TriedAgain {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let first = [
            marks::decorative(theme.glyphs.agent_timer, dim),
            Span::raw(" "),
        ];
        let rest = [Span::raw("  ")];
        wrap_spans(
            &[Span::styled(self.text.clone(), dim)],
            usize::from(ctx.width),
            &first,
            &rest,
        )
    }

    fn continues_previous(&self) -> bool {
        self.continues
    }
}
