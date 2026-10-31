//! The pointer's line under the map, two rows: what the pointer rests on as the world says it
//! (a dot's action, a mark of nature, a wonder, the ground, the air, or a body in the
//! universe), or what pointing finds on the level shown, and a word about the last key.

use ratatui::text::Span;

use super::{BehaviorsTab, Level, found, hover, words};
use crate::world::Pointed;

impl BehaviorsTab {
    /// The dot the pointer rests on.
    pub fn hovered(&self) -> Option<usize> {
        match self.pointed? {
            Pointed::Action(at) => usize::try_from(at).ok(),
            _ => None,
        }
    }

    /// The pointer's line, two rows: what the pointer rests on, else what the keys rest on in
    /// the list, else what pointing finds on this level; the second row the line's own second,
    /// or a word about the last key.
    pub(super) fn pointer_rows(&self, look: &words::Look<'_>) -> [Vec<Span<'static>>; 2] {
        let kit = look.kit;
        let room = look.width.saturating_sub(1);
        let mut rows: Vec<Vec<Span<'static>>> = self.pointed_rows(look);
        if rows.is_empty()
            && let Some(line) = self.cursor_line(look)
        {
            rows = two_rows(&line, room);
        }
        if rows.is_empty() {
            rows.push(vec![kit.dim(format!(" {}", self.point_hint()))]);
        }
        let notice = self.notice.as_ref().map(|n| vec![kit.dim(format!(" {n}"))]);
        let first = rows.remove(0);
        let second = notice
            .or_else(|| rows.into_iter().next())
            .unwrap_or_default();
        let fit = |line: Vec<Span<'static>>| crate::base::text::truncate_spans(&line, room);
        [fit(first), fit(second)]
    }

    /// What the pointer rests on, or ← → picked in the universe, as the world says it.
    pub(super) fn pointed_rows(&self, look: &words::Look<'_>) -> Vec<Vec<Span<'static>>> {
        self.line_shown.set(None);
        if let Some(at) = self.hovered() {
            let Some(action) = look.map.actions.get(at) else {
                return Vec::new();
            };
            if !self.draws_world() {
                return look.pointed(action);
            }
            return look
                .dot_line(at)
                .map(|line| {
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(line);
                    two_rows(&spans, look.width.saturating_sub(1))
                })
                .unwrap_or_default();
        }
        if let Some(mut said) = self.world_said(look) {
            // The line's picture goes over the blanks after the first, and the rows under it
            // line up with the words beside it.
            let icon = self.line_icon();
            self.line_shown.set(icon);
            let gap = icon.map_or(0, |_| usize::from(found::PICTURE_CELLS) + 1);
            // A line too long for its row goes on in the row under it, unless a notice has it,
            // in the same words' colour and under them.
            let mut rows = if said.len() == 1 && self.notice.is_none() {
                let room = look.width.saturating_sub(2 + gap);
                said.remove(0)
                    .flowed(room, look.logos)
                    .iter()
                    .map(|part| {
                        let mut line = vec![Span::raw(" ")];
                        line.extend(look.said(part, true));
                        line
                    })
                    .collect()
            } else {
                self.said_rows(look, &said)
            };
            if gap > 0 {
                for row in &mut rows {
                    row.insert(1, Span::raw(" ".repeat(gap)));
                }
            }
            return rows;
        }
        let target = self.pointed.or(self.picked);
        let (Some(target), Some(world)) = (target, look.world) else {
            return Vec::new();
        };
        let reading = self.reading(look, world);
        let named =
            |logo: &argonauts_core::look::logos::Logo,
             model: &argonauts_core::behaviors::behavior_world::ModelFacts| {
                look.named_after(logo, &look.model_name(model))
            };
        let Some(said) =
            hover::cosmos_lines(target, &self.universe, world, &reading, self.level, &named)
        else {
            return Vec::new();
        };
        self.said_rows(look, &said)
    }

    /// Hover rows as spans, the first in the text's colour and the rest dim.
    fn said_rows(&self, look: &words::Look<'_>, said: &[hover::Said]) -> Vec<Vec<Span<'static>>> {
        said.iter()
            .enumerate()
            .map(|(at, said)| {
                let mut line = vec![Span::raw(" ")];
                line.extend(look.said(said, at == 0));
                line
            })
            .collect()
    }

    /// What the world drawn on the map says of what the pointer found: a mark of nature, a
    /// wonder, or the ground and its ice.
    pub(super) fn world_said(&self, look: &words::Look<'_>) -> Option<Vec<hover::Said>> {
        use found::Found;
        let drawn = self.drawn_here()?;
        let grown = drawn.world.as_deref()?;
        let said = match self.found(&drawn)? {
            Found::Nature(at) => {
                // A CLI the line names stands after its own logo.
                let mut logos = Vec::new();
                let line = grown
                    .nature
                    .line(at, &grown.ground, &grown.facts, &mut |cli| {
                        logos.push(argonauts_core::look::logos::logo_for(cli, None));
                        format!("{} {}", hover::LOGO, look.names.cli(cli))
                    })?;
                vec![hover::Said::with(line, logos)]
            }
            Found::Wonder(at) => {
                let placed = grown.wonders.get(at)?;
                hover::wonder_said(grown.risen.get(placed.index)?, look)
            }
            Found::Ground(line) => [Some(line.text), line.note]
                .into_iter()
                .flatten()
                .map(hover::Said::plain)
                .collect(),
            Found::Air(air) => {
                let reading = self.reading(look, look.world?);
                vec![hover::Said::plain(air.line(&reading)?)]
            }
        };
        Some(said)
    }

    /// What the world's words read beside its facts.
    fn reading<'a>(
        &self,
        look: &words::Look<'a>,
        world: &'a argonauts_core::behaviors::behavior_world::WorldFacts,
    ) -> hover::Reading<'a> {
        hover::Reading {
            world,
            actions: u32::try_from(look.map.actions.len()).unwrap_or(u32::MAX),
            unlabelled: u32::try_from(look.map.unlabelled).unwrap_or(u32::MAX),
            clock: self.clock,
            names: look.names,
        }
    }

    /// What pointing finds on the level shown, where the pointer rests on nothing.
    fn point_hint(&self) -> &'static str {
        let world = self.draws_world();
        match self.level {
            Level::Map if world => "Point at anything on the map to see what it is.",
            Level::Map | Level::Planet if !world => "Point at a dot to see the action under it",
            Level::Planet => "Point at anything on the planet to see what it is; drag to turn it.",
            _ if self.universe.makers.is_empty() => {
                "Only your planet so far: no maker is known yet for the models that did these actions."
            }
            Level::Universe => "Point at a galaxy, a star or a point to see what it is.",
            Level::Galaxy(_) => "Point at a star or a point to see what it is.",
            Level::System(_) => "Point at the star, a planet, a moon or a rock to see what it is.",
            Level::Map => "Point at anything on the map to see what it is.",
        }
    }
}

/// `line` in two rows of `room`, the second set in under the words after its first part, such
/// as `Lit:`, and cut where even two can't hold it.
fn two_rows(line: &[Span<'static>], room: usize) -> Vec<Vec<Span<'static>>> {
    if crate::base::text::spans_width(line) <= room {
        return vec![line.to_vec()];
    }
    let hang = line
        .first()
        .map_or(0, |first| crate::base::text::str_width(&first.content));
    let rest = [Span::raw(" ".repeat(hang))];
    let mut rows: Vec<Vec<Span<'static>>> = crate::base::text::wrap_spans(line, room, &[], &rest)
        .into_iter()
        .map(|row| row.spans)
        .collect();
    if rows.len() > 2 {
        // Each row after the first opens with the set-in blanks.
        let mut tail = rows[1].clone();
        for row in rows.drain(2..) {
            tail.push(Span::raw(" "));
            tail.extend(row.into_iter().skip(1));
        }
        rows[1] = crate::base::text::cut_spans(&tail, room, crate::base::text::At::Word);
    }
    rows
}
