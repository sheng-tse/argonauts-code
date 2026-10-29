//! One key map for the whole world. ↑ ↓ move between the tabs, the sub-tabs, the groupings and
//! the picture; Tab steps the groupings, as on Stats, from the picture as from their row; ← →
//! compare a group on the map and the planet and pick a galaxy, a star or a body in the
//! universe; Enter and + go into what is picked, − and Esc come back out. `g` is never a
//! grouping: it is Home, as everywhere in the panel, and with vim keys vim's top.

use argonauts_core::app::UiCommand;
use argonauts_core::behavior_map::{Placing, Scope};
use argonauts_core::deep_map::DeepOffer;
use argonauts_core::settings::behaviors::BehaviorMap as BehaviorMapSetting;
use crossterm::event::KeyCode;

use super::level::{Level, Zoom};
use super::{At, BehaviorsTab, MapKey, Stop};
use crate::overview::deep_flow::{DeepKey, Shown};
use crate::world::Pointed;

impl BehaviorsTab {
    /// The keys this sub-tab answers before the panel's own: a step back with Esc, the world's
    /// letters and digits, and every key in its rows.
    pub fn claims(&self, code: KeyCode, in_rows: bool) -> bool {
        if self.deep.asking() {
            return true;
        }
        match code {
            KeyCode::Char('d') => self.map_shown() && self.has_actions() && self.deep.claims(code),
            KeyCode::Char('x') => self.deep.claims(code),
            KeyCode::Char('s' | '1' | '2' | '3' | '+' | '=' | '-' | 't' | 'n' | 'm') => true,
            KeyCode::Char('?') => in_rows || self.shortcuts,
            KeyCode::Esc => self.shortcuts || self.steps_back(),
            KeyCode::Char('o' | 'c') => matches!(self.at, At::Card { .. }),
            KeyCode::Up => in_rows && self.stop == Stop::Picture,
            KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Enter
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Home
            | KeyCode::End => in_rows,
            _ => false,
        }
    }

    /// The keys come down from the sub-tabs onto the groupings' row.
    pub fn arrive_from_above(&mut self) {
        self.stop = Stop::Groupings;
        self.shortcuts = false;
    }

    /// ↑ from here leaves the sub-tab's rows for the sub-tabs: from the groupings' row.
    pub fn on_first(&self) -> bool {
        self.stop == Stop::Groupings
    }

    /// The period `by` along from this one, wrapping, through the Overview's own three, as the
    /// panel's `r` and Shift+R step it. Summaries' questions keep the period as it is.
    pub fn step_period(&mut self, by: isize) -> Option<MapKey> {
        if self.deep.asking() {
            return None;
        }
        let all = argonauts_core::overview::OverviewPeriod::ALL;
        self.request.period = crate::overview::periods::cycled(&all, self.request.period, by);
        Some(self.asked())
    }

    /// The offer `d` asked for.
    pub fn accept_offer(&mut self, offer: &DeepOffer) {
        self.deep.accept(offer);
    }

    /// The map on screen, as `d` reads it.
    pub(super) fn shown(&self) -> Option<Shown> {
        let map = self.map.as_ref()?;
        Some(match map.placing {
            Placing::Labels => Shown::Labels {
                kept: !map.offer_summaries,
            },
            Placing::Summaries { .. } => Shown::Summaries {
                unsummarized: map.unsummarized,
            },
        })
    }

    /// What the flow of Summaries asked. The map an update makes is the one asked for next.
    fn deep_key(&mut self, key: DeepKey) -> MapKey {
        match key {
            DeepKey::Moved => MapKey::Moved,
            DeepKey::Ask(request) => MapKey::Ask(request),
            DeepKey::Send(command) => {
                if matches!(command, UiCommand::DeepStart { .. }) {
                    self.request.map = Some(BehaviorMapSetting::Summaries);
                }
                MapKey::Send(command)
            }
            DeepKey::Show(map) => {
                self.request.map = Some(map);
                self.asked()
            }
        }
    }

    pub(super) fn asked(&mut self) -> MapKey {
        self.at = At::Map;
        self.pointed = None;
        MapKey::Ask(self.ask())
    }

    /// Whether Esc has anything to undo before the panel closes: the pointer's line, a place in
    /// the list, a level inside the universe, or a level away from the map.
    fn steps_back(&self) -> bool {
        self.stop == Stop::Picture
            && (self.pointed.is_some() || self.at != At::Map || self.level != Level::Map)
    }

    pub fn key(&mut self, code: KeyCode) -> Option<MapKey> {
        // A word about a key lasts until the next one.
        self.notice = None;
        if self.shortcuts_key(code) {
            return Some(MapKey::Moved);
        }
        if self.deep.asking() || code == KeyCode::Char('x') {
            let key = self.deep.key(code)?;
            return Some(self.deep_key(key));
        }
        if code == KeyCode::Char('d') {
            if !self.has_actions() {
                return None;
            }
            let key = self.deep.d(self.shown()?);
            return Some(self.deep_key(key));
        }
        if let Some(key) = self.world_key(code) {
            return Some(key);
        }
        if self.stop == Stop::Groupings {
            return self.groupings_key(code);
        }
        if code == KeyCode::Esc && self.pointed.take().is_some() {
            return Some(MapKey::Moved);
        }
        if self.level.in_cosmos() {
            return self.cosmos_key(code);
        }
        if code == KeyCode::Up && self.at == At::Map {
            self.stop = Stop::Groupings;
            return Some(MapKey::Moved);
        }
        if code == KeyCode::Esc && self.at == At::Map {
            return Some(self.go(Level::Map));
        }
        self.dots_key(code)
    }

    /// The keys that do the same wherever the keys are on the sub-tab.
    fn world_key(&mut self, code: KeyCode) -> Option<MapKey> {
        Some(match code {
            KeyCode::Char('s') => {
                self.request.scope = match self.request.scope {
                    Scope::Everywhere => Scope::ThisProject,
                    Scope::ThisProject => Scope::Everywhere,
                };
                self.asked()
            }
            KeyCode::Tab => self.step_grouping(1),
            KeyCode::BackTab => self.step_grouping(-1),
            KeyCode::Char('1') => self.go(Level::Map),
            KeyCode::Char('2') => self.go(Level::Planet),
            KeyCode::Char('3') => self.go(Level::Universe),
            KeyCode::Char('+' | '=') => self.zoom_in(),
            KeyCode::Char('-') => self.zoom_out(),
            KeyCode::Char('t') => {
                if self.level == Level::Map {
                    self.flat = !self.flat;
                    self.pointed = None;
                }
                MapKey::Moved
            }
            KeyCode::Char('n') => {
                if self.level == Level::Planet {
                    self.night = !self.night;
                }
                MapKey::Moved
            }
            KeyCode::Char('m') => {
                self.nature_hidden = !self.nature_hidden;
                self.pointed = None;
                MapKey::Moved
            }
            _ => return None,
        })
    }

    /// On the groupings' row: ← → step them, ↓ goes into the picture.
    fn groupings_key(&mut self, code: KeyCode) -> Option<MapKey> {
        match code {
            KeyCode::Left => Some(self.step_grouping(-1)),
            KeyCode::Right => Some(self.step_grouping(1)),
            KeyCode::Down | KeyCode::Enter | KeyCode::Home | KeyCode::End => {
                self.stop = Stop::Picture;
                Some(MapKey::Moved)
            }
            _ => None,
        }
    }

    /// The next grouping `by` along, which asks for the map grouped that way. The universe is
    /// drawn by maker and model whatever the grouping, so there it says what the grouping colours.
    fn step_grouping(&mut self, by: isize) -> MapKey {
        self.request.grouping = self.request.grouping.step(by);
        self.compared = None;
        if self.level.in_cosmos() {
            self.notice = Some(format!(
                "The universe is drawn by maker and model; by {} colours the map and the planet.",
                self.request.grouping.word()
            ));
        }
        self.asked()
    }

    /// The universe, a galaxy and a system: ← → pick, Enter goes in, Esc comes back out.
    fn cosmos_key(&mut self, code: KeyCode) -> Option<MapKey> {
        let picks = self.universe.picks(self.level);
        match code {
            KeyCode::Left | KeyCode::Right if !picks.is_empty() => {
                let at = self
                    .picked
                    .and_then(|picked| picks.iter().position(|p| *p == picked));
                let count = picks.len() as isize;
                let next = match (at, code) {
                    (None, KeyCode::Right) => 0,
                    (None, _) => count - 1,
                    (Some(at), KeyCode::Right) => (at as isize + 1).rem_euclid(count),
                    (Some(at), _) => (at as isize - 1).rem_euclid(count),
                };
                self.picked = picks.get(next as usize).copied();
                self.pointed = None;
                Some(MapKey::Moved)
            }
            KeyCode::Enter => Some(self.enter_cosmos()),
            KeyCode::Up => {
                self.stop = Stop::Groupings;
                Some(MapKey::Moved)
            }
            KeyCode::Home | KeyCode::End => {
                let picked = match code {
                    KeyCode::Home => picks.first(),
                    _ => picks.last(),
                };
                self.picked = picked.copied();
                Some(MapKey::Moved)
            }
            KeyCode::Esc => match self.universe.out_of(self.level) {
                Some((level, picked)) => {
                    let key = self.go(level);
                    self.picked = Some(picked);
                    Some(key)
                }
                None => Some(self.go(Level::Map)),
            },
            _ => Some(MapKey::Moved),
        }
    }

    /// Enter in the universe: into the galaxy or onto your planet picked; in a galaxy, into
    /// the star's system. A system is the deepest level, so there Enter picks what the pointer
    /// rests on and keeps its line.
    fn enter_cosmos(&mut self) -> MapKey {
        let target = self.pointed.or(self.picked).or(match self.level {
            Level::System(star) => u32::try_from(star).ok().map(Pointed::Star),
            _ => None,
        });
        let Some(target) = target else {
            let what = match self.level {
                Level::Galaxy(_) => "a star",
                _ => "a galaxy",
            };
            self.notice = Some(format!(
                "Pick {what} with {}  first, or point at one.",
                self.arrows()
            ));
            return MapKey::Moved;
        };
        if let Level::System(_) = self.level {
            self.picked = Some(target);
            self.pointed = None;
            return MapKey::Moved;
        }
        match self.universe.inside(self.level, target) {
            Some(level) => {
                let key = self.go(level);
                // From the universe onto a star, its galaxy opens with the star picked.
                if let (Level::Galaxy(_), Pointed::Star(_)) = (level, target) {
                    self.picked = Some(target);
                }
                key
            }
            None => MapKey::Moved,
        }
    }

    /// `+`: into what is picked, or from the universe to the planet and the planet to the map.
    fn zoom_in(&mut self) -> MapKey {
        let next = match self.level {
            Level::Map | Level::System(_) => None,
            Level::Planet => Some(Level::Map),
            Level::Universe => Some(
                self.picked
                    .and_then(|picked| self.universe.inside(Level::Universe, picked))
                    .unwrap_or(Level::Planet),
            ),
            Level::Galaxy(_) => self
                .picked
                .and_then(|picked| self.universe.inside(self.level, picked)),
        };
        next.map_or(MapKey::Moved, |level| self.go(level))
    }

    /// `−`: from the map out to the planet and on to the universe, and from a system or a
    /// galaxy back out toward the universe.
    fn zoom_out(&mut self) -> MapKey {
        match self.level {
            Level::Map => self.go(Level::Planet),
            Level::Planet => self.go(Level::Universe),
            Level::Universe => MapKey::Moved,
            Level::Galaxy(_) | Level::System(_) => match self.universe.out_of(self.level) {
                Some((level, picked)) => {
                    let key = self.go(level);
                    self.picked = Some(picked);
                    key
                }
                None => MapKey::Moved,
            },
        }
    }

    /// The wheel over the picture, `up` toward what is under the pointer: into a galaxy or a
    /// star, else the next level in, and down the next level out.
    pub fn wheel(&mut self, up: bool) -> bool {
        let before = (self.level, self.picked);
        if !up {
            self.zoom_out();
            return (self.level, self.picked) != before;
        }
        let toward = self
            .pointed
            .filter(|p| matches!(p, Pointed::Galaxy(_) | Pointed::Star(_)));
        match toward.and_then(|target| self.universe.inside(self.level, target)) {
            Some(level) => {
                self.go(level);
            }
            None => {
                self.zoom_in();
            }
        }
        (self.level, self.picked) != before
    }

    /// The world goes to `to`, zooming there unless motion is off; reduced motion zooms with
    /// what moves held still. Leaving the levels with dots closes the list, and every level
    /// starts with nothing picked or pointed at.
    pub(super) fn go(&mut self, to: Level) -> MapKey {
        let from = self.level;
        if from == to {
            return MapKey::Moved;
        }
        let now = self.moment.now;
        self.zoom = (!self.moment.skips_frames).then(|| Zoom::new(from, to, now));
        self.level = to;
        self.pointed = None;
        self.picked = None;
        if !to.shows_dots() {
            self.at = At::Map;
            self.first_row.set(0);
        }
        MapKey::Moved
    }

    /// The compared group's list is drawn: it has rows, and no update of Summaries hides it.
    pub(super) fn listed_now(&self) -> bool {
        !self.deep.running() && self.comparison().is_some_and(|c| !c.rows.is_empty())
    }

    /// On the map and the planet: ← → compare, ↓ goes into the list, Enter lists a behavior's
    /// actions and opens their cards.
    fn dots_key(&mut self, code: KeyCode) -> Option<MapKey> {
        let order = self
            .map
            .as_ref()
            .map_or_else(Vec::new, |map| super::legend::busiest_first(map));
        let rows = self.comparison().map_or(0, |c| c.rows.len());
        let next = match (self.at, code) {
            (At::Map | At::List(_), KeyCode::Left | KeyCode::Right) => {
                let by = if code == KeyCode::Left { -1 } else { 1 };
                // Every group alike, then each group compared in the legend's order, round
                // again, so each press moves the mark one row.
                let at = self
                    .compared
                    .and_then(|group| order.iter().position(|at| *at == group))
                    .map_or(0, |at| at + 1) as isize;
                let next = (at + by).rem_euclid(order.len() as isize + 1) as usize;
                self.compared = next.checked_sub(1).map(|at| order[at]);
                self.pointed = None;
                self.first_row.set(0);
                let has_rows = self.comparison().is_some_and(|c| !c.rows.is_empty());
                match self.at {
                    At::List(_) if has_rows => At::List(0),
                    _ => At::Map,
                }
            }
            // While Summaries are made the list waits for them, so ↓ stays on the map.
            (At::Map, KeyCode::Down) if self.listed_now() => At::List(0),
            (At::List(0), KeyCode::Up) | (At::List(_), KeyCode::Esc) => {
                self.first_row.set(0);
                At::Map
            }
            // A key in the list takes over from the pointer, so the line under the map says
            // what the keys rest on.
            (At::List(row), KeyCode::Up) => {
                self.pointed = None;
                At::List(row - 1)
            }
            (At::List(row), KeyCode::Down) => {
                self.pointed = None;
                At::List((row + 1).min(rows.saturating_sub(1)))
            }
            (At::List(_), KeyCode::Home) => {
                self.pointed = None;
                At::List(0)
            }
            (At::List(_), KeyCode::End) => {
                self.pointed = None;
                At::List(rows.saturating_sub(1))
            }
            (At::List(row), KeyCode::Enter) if !self.actions_of(row).is_empty() => {
                self.first_action.set(0);
                At::Actions { row, action: 0 }
            }
            (At::Actions { row, action }, KeyCode::Up | KeyCode::Down) => {
                self.pointed = None;
                let last = self.actions_of(row).len().saturating_sub(1);
                let action = match code {
                    KeyCode::Up => action.saturating_sub(1),
                    _ => (action + 1).min(last),
                };
                At::Actions { row, action }
            }
            (At::Actions { row, .. }, KeyCode::Home | KeyCode::End) => {
                self.pointed = None;
                let action = match code {
                    KeyCode::Home => 0,
                    _ => self.actions_of(row).len().saturating_sub(1),
                };
                At::Actions { row, action }
            }
            (At::Actions { row, action }, KeyCode::Enter) => At::Card { row, action },
            (At::Actions { row, .. }, KeyCode::Esc) => At::List(row),
            (At::Card { row, action }, KeyCode::Left | KeyCode::Right) => {
                let count = self.actions_of(row).len().max(1);
                let by = if code == KeyCode::Left { count - 1 } else { 1 };
                At::Card {
                    row,
                    action: (action + by) % count,
                }
            }
            (At::Card { row, action }, KeyCode::Esc) => At::Actions { row, action },
            (At::Card { row, action }, KeyCode::Char('c')) => {
                let at = *self.actions_of(row).get(action)?;
                let action = self.map.as_ref()?.actions.get(at)?;
                return Some(MapKey::Copy(action.input.clone()));
            }
            (At::Card { row, action }, KeyCode::Char('o')) => {
                let at = *self.actions_of(row).get(action)?;
                let run = self.map.as_ref()?.actions.get(at)?.run.clone()?;
                return Some(MapKey::Ask(argonauts_core::app::ViewRequest::Workflow {
                    run: argonauts_core::RunId::new(&run).ok()?,
                    workflow: None,
                }));
            }
            (at, _) => at,
        };
        self.at = next;
        Some(MapKey::Moved)
    }
}
