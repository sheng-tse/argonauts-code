//! The band between the working line and the input: the mods you pinned, top to bottom.
//! Each is a card, or folded to its line. When the band runs out of room the
//! cards step down from the last one up, and Ctrl+X Ctrl+A hides them all to one line that
//! still shows each one's number. Ctrl+X Tab gives the band the keys. On a wide screen two
//! half-width mods share a row, and a change of height waits a moment so it lands in one
//! frame.
//!
//! `band_cards.rs` says which mods it draws and from what. A name pinned that none of them
//! answers to is left for the mod that draws it.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::api::{UsagePeriod, UsageSummary};
use argonauts_core::runs::events::context::ContextDetail;
use argonauts_core::settings::ui::{ModName, WhenQuestion};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::base::draw::render::RenderCtx;
use crate::base::kit::{ContextCard, Form, Frame, Kit, Toggle, side_by_side, spread};
use crate::base::text::{spans_width, truncate_spans};
use crate::base::theme::Role;
use crate::panels::{StatusCards, UsageCards, UsageWatch};

use crate::panels::pinned_run::PinnedRun;
use cards::{BandCard, Sources};

/// What the bottom line says while the band has the keys.
pub const BAND_KEYS: &str =
    "tab next · - fold · enter open · p unpin · [ ] move · , settings · esc for the input";

/// What a key did while the band had the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandEvent {
    Moved,
    /// Fold the mod to its line, or open it again.
    Fold {
        name: ModName,
        open: bool,
    },
    /// Open the mod's tab of the usage panel.
    Open(ModName),
    Unpin(ModName),
    /// One place toward the top, or toward the input.
    Move {
        name: ModName,
        up: bool,
    },
    Settings(ModName),
    /// Esc gave the keys back to the input.
    Leave,
    /// Tab went past the last mod, to whatever comes next above the input.
    Past,
    Unhandled,
}

#[derive(Debug, Clone)]
pub struct Band {
    /// `ui.above_input.mods`, top to bottom.
    pub mods: Vec<ModName>,
    /// The mods folded to their line, `ui.mods.<name>.open = false`.
    pub folded: Vec<ModName>,
    /// `ui.above_input.shown`.
    pub shown: bool,
    /// Ctrl+X Ctrl+A hid it, for this session.
    pub hidden: bool,
    /// The most rows the band takes.
    pub max_rows: usize,
    /// Narrower than this, every mod draws its line.
    pub min_width: usize,
    pub when_question: WhenQuestion,
    /// The numbers the cards draw, once the controller sent them.
    pub usage: Option<Arc<UsageSummary>>,
    pub names: DisplayNames,
    /// When `usage` came, in the zone times are shown in.
    pub wall: OffsetDateTime,
    /// The run the screen follows, for the run's own mods.
    pub run: Option<PinnedRun>,
    /// The conversation's context, or the fullest session's, for the context mod.
    pub context: Option<ContextCard>,
    /// The mods set to half the width, `ui.mods.<name>.width = "half"`.
    pub halves: Vec<ModName>,
    /// From this width two half-width neighbours share a row. 0 never.
    pub side_by_side_at: usize,
    /// Each mod's own key, as written, such as `ctrl+x q`, for its folded line.
    pub keys: Vec<(ModName, String)>,
    /// How long a change of height waits before it lands, `ui.above_input.settle`.
    pub settle: Duration,
    /// The pinned mod that has the keys, by its place among those drawn.
    selected: Option<usize>,
    pub watch: UsageWatch,
    /// What the conversation's session was last told about counting its context: the detail
    /// the context mod asks for while it's pinned, and nothing once it's gone.
    pub context_watched: Option<ContextDetail>,
    /// The bottom line lists the stats mod, so its numbers are wanted wherever the band is.
    pub line_stats: bool,
    settling: Settling,
}

impl Default for Band {
    fn default() -> Self {
        Self {
            mods: Vec::new(),
            folded: Vec::new(),
            shown: true,
            hidden: false,
            max_rows: 14,
            min_width: 60,
            when_question: WhenQuestion::Fold,
            usage: None,
            names: DisplayNames::default(),
            wall: OffsetDateTime::UNIX_EPOCH,
            run: None,
            context: None,
            halves: Vec::new(),
            side_by_side_at: 120,
            keys: Vec::new(),
            settle: Duration::from_millis(150),
            selected: None,
            watch: UsageWatch::default(),
            context_watched: None,
            line_stats: false,
            settling: Settling::default(),
        }
    }
}

/// The rows the band last drew, kept on screen while a change of height settles: Claude
/// Code holds a resize of its own band for the same moment, so a number that flickers
/// between two heights never makes the input jump.
#[derive(Debug, Clone, Default)]
struct Settling(Arc<Mutex<Held>>);

#[derive(Debug, Default)]
struct Held {
    lines: Vec<Line<'static>>,
    /// The width they were drawn at. Rows of another width can't be kept.
    width: usize,
    /// When the band first came out at another height.
    since: Option<Instant>,
}

impl Settling {
    /// What to draw now: `fresh`, or the rows held while their height is different from it and
    /// `settle` hasn't passed. Held rows that no longer fit `room` give way at once.
    fn land(
        &self,
        fresh: Vec<Line<'static>>,
        width: usize,
        room: usize,
        now: Instant,
        settle: Duration,
    ) -> Vec<Line<'static>> {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let waits = !settle.is_zero()
            && held.width == width
            && held.lines.len() != fresh.len()
            && held.lines.len() <= room;
        if waits {
            let since = *held.since.get_or_insert(now);
            if now.saturating_duration_since(since) < settle {
                return held.lines.clone();
            }
        }
        held.since = None;
        held.width = width;
        held.lines.clone_from(&fresh);
        fresh
    }

    /// When held rows give way to the height that waits.
    fn lands_at(&self, settle: Duration) -> Option<Instant> {
        let held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        held.since.map(|since| since + settle)
    }
}

impl Band {
    /// What the pinned mods draw from, as far as the screen holds it.
    fn sources(&self) -> Sources<'_> {
        Sources {
            usage: self.usage.as_deref().map(|usage| UsageCards {
                usage,
                names: &self.names,
                now: self.wall,
                left: false,
            }),
            status: self.run.as_ref().map(|run| StatusCards {
                summary: &run.summary,
                names: &self.names,
                title: run.title.as_deref(),
            }),
            context: self.context.as_ref(),
        }
    }

    /// The pinned mods this band draws now, in order: those it knows by name that have
    /// something to show.
    fn pinned(&self) -> Vec<(ModName, BandCard)> {
        let sources = self.sources();
        self.mods
            .iter()
            .filter_map(|name| Some((*name, BandCard::of(*name)?)))
            .filter(|(_, card)| sources.has(*card))
            .collect()
    }

    /// A pinned mod draws the usage numbers, so they must be asked for.
    fn wants_usage(&self) -> bool {
        let usage = |name: &ModName| {
            matches!(
                BandCard::of(*name),
                Some(BandCard::Usage(_) | BandCard::Stats)
            )
        };
        self.mods.iter().any(usage)
    }

    /// The period to ask the numbers for. While the stats mod is pinned it's the one
    /// `mods/stats.toml` sets, so a tab that showed another period leaves the card as it was.
    fn period(&self, usage: &UsageSummary) -> UsagePeriod {
        match &usage.stats {
            Some(stats) if self.line_stats || self.mods.contains(&ModName::Stats) => {
                stats.settings.period.usage()
            }
            _ => usage.period,
        }
    }

    /// Whether `name` is pinned, for what keeps its numbers fresh.
    pub fn pins(&self, name: ModName) -> bool {
        self.shown && self.mods.contains(&name)
    }

    /// Something of it shows: it's on, and a pinned mod has something to draw.
    pub fn shows(&self) -> bool {
        self.shown && !self.pinned().is_empty()
    }

    pub fn has_focus(&self) -> bool {
        self.selected.is_some() && self.shows() && !self.hidden
    }

    /// Gives the first mod the keys. False when the band shows nothing to take them.
    pub fn focus(&mut self) -> bool {
        if !self.shows() || self.hidden {
            return false;
        }
        self.selected = Some(0);
        true
    }

    pub fn blur(&mut self) {
        self.selected = None;
    }

    /// Ctrl+X Ctrl+A: to one line, or back.
    pub fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        if self.hidden {
            self.selected = None;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> BandEvent {
        let pinned = self.pinned();
        let Some(at) = self.selected.filter(|_| !pinned.is_empty()) else {
            return BandEvent::Unhandled;
        };
        let at = at.min(pinned.len() - 1);
        let name = pinned[at].0;
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return BandEvent::Unhandled;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down if at + 1 < pinned.len() => {
                self.selected = Some(at + 1);
                BandEvent::Moved
            }
            KeyCode::Tab | KeyCode::Down => BandEvent::Past,
            KeyCode::BackTab | KeyCode::Up => {
                self.selected = Some(at.saturating_sub(1));
                BandEvent::Moved
            }
            KeyCode::Char('-' | ' ') => BandEvent::Fold {
                name,
                open: self.folded.contains(&name),
            },
            KeyCode::Enter => BandEvent::Open(name),
            KeyCode::Char('p') => {
                self.selected = Some(at.min(pinned.len().saturating_sub(2)));
                BandEvent::Unpin(name)
            }
            KeyCode::Char('[') => {
                self.selected = Some(at.saturating_sub(1));
                BandEvent::Move { name, up: true }
            }
            KeyCode::Char(']') => {
                self.selected = Some((at + 1).min(pinned.len() - 1));
                BandEvent::Move { name, up: false }
            }
            KeyCode::Char(',') => BandEvent::Settings(name),
            KeyCode::Esc => {
                self.selected = None;
                BandEvent::Leave
            }
            _ => BandEvent::Unhandled,
        }
    }

    /// The band's rows in at most `room` rows. While a question has the keys the cards fold
    /// to their lines, or go, as `ui.above_input.when_question` says.
    /// `live` is whether work runs, when the numbers move by the second.
    pub fn lines(
        &self,
        ctx: &RenderCtx<'_>,
        room: usize,
        question_up: bool,
        live: bool,
    ) -> Vec<Line<'static>> {
        let fresh = self.fresh_lines(ctx, room, question_up, live);
        let width = usize::from(ctx.width);
        self.settling
            .land(fresh, width, room, ctx.clock.now, self.settle)
    }

    /// When the band next changes by itself: a height that waits lands.
    pub fn next_change(&self) -> Option<Instant> {
        self.settling.lands_at(self.settle)
    }

    fn fresh_lines(
        &self,
        ctx: &RenderCtx<'_>,
        room: usize,
        question_up: bool,
        live: bool,
    ) -> Vec<Line<'static>> {
        let hide_for_question = question_up && self.when_question == WhenQuestion::Hide;
        let drawn = self.shown && !hide_for_question && room > 0;
        if (drawn && self.wants_usage()) || self.line_stats {
            match &self.usage {
                Some(usage) => self.watch.drawn(self.period(usage), live, ctx.clock.now),
                // Nothing to draw yet. Asking is what brings the numbers.
                None => self.watch.drawn(Default::default(), true, ctx.clock.now),
            }
        }
        if !drawn {
            return Vec::new();
        }
        let pinned = self.pinned();
        if pinned.is_empty() {
            return Vec::new();
        }
        let kit = Kit::new(ctx.theme);
        let width = usize::from(ctx.width);
        let cards = self.sources();
        if self.hidden {
            return vec![self.hidden_line(&kit, &cards, &pinned, width)];
        }
        let fold_all =
            width < self.min_width || (question_up && self.when_question == WhenQuestion::Fold);
        let mut forms: Vec<Form> = pinned
            .iter()
            .map(|(name, card)| {
                if fold_all || self.folded.contains(name) {
                    Form::Line
                } else {
                    card.opens_as()
                }
            })
            .collect();
        let limit = self.max_rows.min(room);
        loop {
            let drawn = self.draw(&kit, &cards, &pinned, &forms, width);
            if drawn.len() <= limit {
                return drawn;
            }
            // The last mod steps down first, and the first one last.
            let Some(next) = forms.iter().rposition(|form| *form != Form::Line) else {
                return vec![self.hidden_line(&kit, &cards, &pinned, width)];
            };
            forms[next] = match forms[next] {
                Form::Full => Form::Compact,
                _ => Form::Line,
            };
        }
    }

    fn draw(
        &self,
        kit: &Kit<'_>,
        cards: &Sources<'_>,
        pinned: &[(ModName, BandCard)],
        forms: &[Form],
        width: usize,
    ) -> Vec<Line<'static>> {
        // A card that left, such as a run's when it ended, hands the keys to the last one.
        let selected = self
            .selected
            .filter(|_| self.has_focus())
            .map(|at| at.min(pinned.len().saturating_sub(1)));
        let framed = |at: usize, frame: Frame| {
            let active = selected == Some(at);
            cards.draw(kit, pinned[at].1, forms[at], Frame { active, ..frame })
        };
        let mut lines = Vec::new();
        let mut at = 0;
        while at < pinned.len() {
            if forms[at] == Form::Line {
                let (name, card) = pinned[at];
                let key = self.key_of(name);
                let active = selected == Some(at);
                lines.push(Line::from(folded_line(
                    kit, cards, card, key, active, width,
                )));
                at += 1;
            } else if self.shares_a_row(pinned, forms, at, width) {
                // Each half keeps its own toggle, so the two fold apart.
                let half = width / 2;
                let left = framed(at, Frame::band(half));
                let right = framed(at + 1, Frame::band(width - half));
                lines.extend(side_by_side(left, right, half).lines);
                at += 2;
            } else {
                lines.extend(framed(at, Frame::band(width)).lines);
                at += 1;
            }
        }
        lines
    }

    /// The mod at `at` and the one after it are both half-width cards, on a screen wide
    /// enough for two.
    fn shares_a_row(
        &self,
        pinned: &[(ModName, BandCard)],
        forms: &[Form],
        at: usize,
        width: usize,
    ) -> bool {
        let half = |at: usize| {
            pinned
                .get(at)
                .is_some_and(|(name, _)| self.halves.contains(name))
                && forms.get(at).is_some_and(|form| *form != Form::Line)
        };
        self.side_by_side_at > 0 && width >= self.side_by_side_at && half(at) && half(at + 1)
    }

    fn key_of(&self, name: ModName) -> Option<&str> {
        self.keys
            .iter()
            .find(|(mod_name, _)| *mod_name == name)
            .map(|(_, key)| key.as_str())
    }

    /// ` ▸ 2 mods hidden · Codex 5-hour 92% · ~$1.86 today · ctrl+x ctrl+a to show`
    fn hidden_line(
        &self,
        kit: &Kit<'_>,
        cards: &Sources<'_>,
        pinned: &[(ModName, BandCard)],
        width: usize,
    ) -> Line<'static> {
        let count = match pinned.len() {
            1 => "1 mod hidden".to_owned(),
            n => format!("{n} mods hidden"),
        };
        let mut row = vec![kit.dim(format!(" {} {count}", kit.glyphs.folded))];
        for (_, card) in pinned {
            row.push(kit.dim(" · "));
            row.extend(cards.key_number(kit, *card));
        }
        row.push(kit.dim(" · ctrl+x ctrl+a to show"));
        Line::from(truncate_spans(&row, width))
    }
}

/// ` ▸ ◆ spend  ~$1.86 today                                    ctrl+x s  [+]`
fn folded_line(
    kit: &Kit<'_>,
    cards: &Sources<'_>,
    card: BandCard,
    key: Option<&str>,
    active: bool,
    width: usize,
) -> Vec<Span<'static>> {
    let mark = if active { Role::Question } else { Role::Dim };
    let mut right = Vec::new();
    if let Some(key) = key {
        right.push(kit.dim(format!("{key}  ")));
    }
    right.push(kit.dim(Toggle::Folded.text()));
    let room = width.saturating_sub(3 + 1 + spans_width(&right));
    let mut left = vec![kit.span(format!(" {} ", kit.glyphs.folded), mark)];
    left.extend(cards.line(kit, card, room));
    spread(left, right, width)
}

mod cards;

#[cfg(test)]
mod tests;
