//! Behaviors, the third sub-tab beside Overview and Models: a map of every action your agents
//! took, one dot each, coloured by its group. It opens on every group's dots with nothing
//! compared; ← → compares one group with the rest, red behind the dots where it acts more and
//! blue where less, with its distinctive behaviors listed under the map. ↓ goes into the list,
//! which lights the selected behavior's dots; Enter lists its actions, each ringed on the map
//! in turn, and Enter on one opens its card. The map is a picture where the terminal draws one
//! and braille elsewhere; nothing is written on it.

mod braille;
mod colours;
pub mod picture;
mod words;

#[cfg(test)]
mod golden;
#[cfg(test)]
pub(crate) mod sample;
#[cfg(test)]
mod tests;

use std::hash::{DefaultHasher, Hash, Hasher};

use unicode_width::UnicodeWidthStr;

use argonauts_core::RunId;
use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::behavior_map::{
    BehaviorMap, BehaviorRow, Comparison, MapRequest, Placing, Scope,
};
use argonauts_core::deep_map::DeepOffer;
use argonauts_core::settings::behaviors::BehaviorMap as BehaviorMapSetting;
use crossterm::event::KeyCode;

pub use colours::GroupColours;

use crate::kit::{Drawn, Kit};
use crate::overview::deep_flow::{DeepFlow, DeepKey, Shown};
use crate::views::map::MapPoint;

/// The map's size in cells.
pub const MAP_COLUMNS: usize = 64;
pub const MAP_ROWS: usize = 13;
/// The map starts this many cells in, and the legend this many cells after it.
pub const MAP_INSET: usize = 1;
pub const LEGEND_GAP: usize = 2;
/// The list shows at most this many behaviors, and of a behavior this many actions.
const LIST_ROWS: usize = 6;
/// The map's block among the sub-tab's, under the line that says what it holds.
pub const MAP_BLOCK: usize = 1;

/// Where the keys are on the sub-tab.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum At {
    /// On the map, nothing lit.
    #[default]
    Map,
    /// On a behavior in the list, its dots lit.
    List(usize),
    /// On one of a behavior's actions, ringed on the map.
    Actions { row: usize, action: usize },
    /// An action's card.
    Card { row: usize, action: usize },
}

/// What a key on the sub-tab asks of the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum MapKey {
    Moved,
    Ask(ViewRequest),
    Copy(String),
    /// The deeper map's update starts or stops.
    Send(UiCommand),
}

/// The sub-tab's state while the panel is open.
#[derive(Debug, Clone, Default)]
pub struct BehaviorsTab {
    pub request: MapRequest,
    map: Option<BehaviorMap>,
    /// The group compared with the rest; `None` shows every group's dots alike.
    compared: Option<usize>,
    pub at: At,
    /// The dot nearest the mouse, while it rests on the map.
    hovered: Option<usize>,
    /// The first action shown of a behavior's, as its list scrolls.
    first_action: usize,
    /// The first behavior shown, as the list scrolls past the rows it shows at once.
    first_row: usize,
    /// The clock a card's times read on.
    pub clock: crate::format::WallClock,
    /// The screen puts the map's picture over the cells it leaves for one, so the legend sits
    /// beside them; otherwise the map is braille with its legend under it.
    pub picture_placed: bool,
    /// Counts the maps taken, so a picture of an older one is never taken for this one's.
    maps_taken: u64,
    /// `d`: the deeper map's questions, and its update while one runs.
    pub deep: DeepFlow,
}

impl BehaviorsTab {
    pub fn ask(&self) -> ViewRequest {
        ViewRequest::Behaviors(self.request)
    }

    pub fn has_map(&self) -> bool {
        self.map.is_some()
    }

    /// Takes a new map. The comparison stays on the group of the same name where it can. A
    /// new map for the same request, as an update in the background sends, keeps the place in
    /// the list or the card open where it still has one.
    pub fn accept(&mut self, map: &BehaviorMap) {
        if map.request != self.request {
            return;
        }
        let again = self
            .map
            .as_ref()
            .is_some_and(|old| old.request == map.request);
        let was = self.compared_group().map(|group| group.to_owned());
        self.compared = was.and_then(|name| map.groups.iter().position(|g| g.name == name));
        self.map = Some(map.clone());
        self.maps_taken += 1;
        self.hovered = None;
        if !(again && self.compared.is_some() && self.still_there(self.at)) {
            self.at = At::Map;
            self.first_action = 0;
            self.first_row = 0;
        }
    }

    /// Whether `at` is still a place on the map now taken.
    fn still_there(&self, at: At) -> bool {
        let rows = self.comparison().map_or(0, |c| c.rows.len());
        match at {
            At::Map => true,
            At::List(row) => row < rows,
            At::Actions { row, action } | At::Card { row, action } => {
                row < rows && action < self.actions_of(row).len()
            }
        }
    }

    fn compared_group(&self) -> Option<&str> {
        let map = self.map.as_ref()?;
        Some(map.groups.get(self.compared?)?.name.as_str())
    }

    fn comparison(&self) -> Option<&Comparison> {
        self.map.as_ref()?.comparisons.get(self.compared?)
    }

    fn row(&self, row: usize) -> Option<&BehaviorRow> {
        self.comparison()?.rows.get(row)
    }

    /// The actions Enter lists for a behavior: the compared group's, newest first.
    fn actions_of(&self, row: usize) -> Vec<usize> {
        let (Some(map), Some(group), Some(row)) = (&self.map, self.compared, self.row(row)) else {
            return Vec::new();
        };
        let mut mine: Vec<usize> = row
            .lit
            .iter()
            .copied()
            .filter(|at| map.actions.get(*at).is_some_and(|a| a.group == group))
            .collect();
        // A deeper map's row lists its actions nearest first, as it lights them; a label's
        // actions come newest first.
        if map.placing == Placing::Labels {
            mine.sort_by(|a, b| map.actions[*b].at.cmp(&map.actions[*a].at).then(a.cmp(b)));
        }
        mine
    }

    /// Whether the map holds any action, which `d` summarizes and the arrows compare.
    fn has_actions(&self) -> bool {
        self.map.as_ref().is_some_and(|map| !map.actions.is_empty())
    }

    /// The map is on screen, so the mouse points at its dots.
    pub fn map_shown(&self) -> bool {
        self.map.is_some() && !matches!(self.at, At::Card { .. })
    }

    /// ↑ from here leaves the sub-tab's rows for the sub-tabs.
    pub fn on_first(&self) -> bool {
        self.at == At::Map
    }

    /// The keys this sub-tab answers before the panel's own: a step back with Esc, and every
    /// key in its rows.
    pub fn claims(&self, code: KeyCode, in_rows: bool) -> bool {
        if self.deep.asking() {
            return true;
        }
        match code {
            KeyCode::Char('d') => self.map_shown() && self.has_actions() && self.deep.claims(code),
            KeyCode::Char('x') => self.deep.claims(code),
            KeyCode::Char('g' | 'p' | 'r') => true,
            KeyCode::Esc => self.at != At::Map || self.hovered.is_some(),
            KeyCode::Char('o' | 'c') => matches!(self.at, At::Card { .. }),
            KeyCode::Up => in_rows && self.at != At::Map,
            KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Enter => in_rows,
            _ => false,
        }
    }

    pub fn key(&mut self, code: KeyCode) -> Option<MapKey> {
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
        match code {
            KeyCode::Char('g') => {
                self.request.grouping = self.request.grouping.step(1);
                return Some(self.asked());
            }
            KeyCode::Char('p') => {
                let all = argonauts_core::overview::OverviewPeriod::ALL;
                self.request.period = super::super::periods::cycled(&all, self.request.period, 1);
                return Some(self.asked());
            }
            KeyCode::Char('r') => {
                self.request.scope = match self.request.scope {
                    Scope::Everywhere => Scope::ThisProject,
                    Scope::ThisProject => Scope::Everywhere,
                };
                return Some(self.asked());
            }
            _ => {}
        }
        if code == KeyCode::Esc && self.hovered.take().is_some() {
            return Some(MapKey::Moved);
        }
        let groups = self.map.as_ref().map_or(0, |map| map.groups.len());
        let rows = self.comparison().map_or(0, |c| c.rows.len());
        let next = match (self.at, code) {
            (At::Map | At::List(_), KeyCode::Left | KeyCode::Right) => {
                let by = if code == KeyCode::Left { -1 } else { 1 };
                // Every group alike, then each group compared, round again.
                let at = self.compared.map_or(0, |group| group + 1) as isize;
                let next = (at + by).rem_euclid(groups as isize + 1) as usize;
                self.compared = next.checked_sub(1);
                self.hovered = None;
                self.first_row = 0;
                let has_rows = self.comparison().is_some_and(|c| !c.rows.is_empty());
                match self.at {
                    At::List(_) if has_rows => At::List(0),
                    _ => At::Map,
                }
            }
            (At::Map, KeyCode::Down) if rows > 0 => At::List(0),
            (At::List(0), KeyCode::Up) | (At::List(_), KeyCode::Esc) => {
                self.first_row = 0;
                At::Map
            }
            (At::List(row), KeyCode::Up) => {
                self.scroll_rows(row - 1);
                At::List(row - 1)
            }
            (At::List(row), KeyCode::Down) => {
                let row = (row + 1).min(rows.saturating_sub(1));
                self.scroll_rows(row);
                At::List(row)
            }
            (At::List(row), KeyCode::Enter) if !self.actions_of(row).is_empty() => {
                self.first_action = 0;
                At::Actions { row, action: 0 }
            }
            (At::Actions { row, action }, KeyCode::Up | KeyCode::Down) => {
                let last = self.actions_of(row).len().saturating_sub(1);
                let action = match code {
                    KeyCode::Up => action.saturating_sub(1),
                    _ => (action + 1).min(last),
                };
                self.scroll_actions(action);
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
            (At::Card { row, action }, KeyCode::Esc) => {
                self.scroll_actions(action);
                At::Actions { row, action }
            }
            (At::Card { row, action }, KeyCode::Char('c')) => {
                let at = *self.actions_of(row).get(action)?;
                let action = self.map.as_ref()?.actions.get(at)?;
                return Some(MapKey::Copy(action.input.clone()));
            }
            (At::Card { row, action }, KeyCode::Char('o')) => {
                let at = *self.actions_of(row).get(action)?;
                let run = self.map.as_ref()?.actions.get(at)?.run.clone()?;
                return Some(MapKey::Ask(ViewRequest::Workflow {
                    run: RunId::new(&run).ok()?,
                    workflow: None,
                }));
            }
            (at, _) => at,
        };
        self.at = next;
        Some(MapKey::Moved)
    }

    /// The map on screen, as `d` reads it.
    fn shown(&self) -> Option<Shown> {
        let map = self.map.as_ref()?;
        Some(match map.placing {
            Placing::Labels => Shown::Labels {
                kept: !map.offer_deeper,
            },
            Placing::Summaries { .. } => Shown::Deeper {
                unsummarized: map.unsummarized,
            },
        })
    }

    /// What the deeper map's flow asked. The map an update makes is the one asked for next.
    fn deep_key(&mut self, key: DeepKey) -> MapKey {
        match key {
            DeepKey::Moved => MapKey::Moved,
            DeepKey::Ask(request) => MapKey::Ask(request),
            DeepKey::Send(command) => {
                if matches!(command, UiCommand::DeepStart { .. }) {
                    self.request.map = Some(BehaviorMapSetting::Deeper);
                }
                MapKey::Send(command)
            }
            DeepKey::Show(map) => {
                self.request.map = Some(map);
                self.asked()
            }
        }
    }

    /// The offer `d` asked for.
    pub fn accept_offer(&mut self, offer: &DeepOffer) {
        self.deep.accept(offer);
    }

    fn asked(&mut self) -> MapKey {
        self.at = At::Map;
        self.hovered = None;
        MapKey::Ask(self.ask())
    }

    /// Keeps the behavior at `row` among the rows the list shows.
    fn scroll_rows(&mut self, row: usize) {
        if row < self.first_row {
            self.first_row = row;
        } else if row >= self.first_row + LIST_ROWS {
            self.first_row = row + 1 - LIST_ROWS;
        }
    }

    fn scroll_actions(&mut self, action: usize) {
        if action < self.first_action {
            self.first_action = action;
        } else if action >= self.first_action + LIST_ROWS {
            self.first_action = action + 1 - LIST_ROWS;
        }
    }

    /// The mouse rests at `point`, in the map's own units, 0..1 across and up; `None` once it
    /// leaves the map. Whether the dot it is nearest changed.
    pub fn point_at(&mut self, point: Option<(f32, f32)>) -> bool {
        let nearest = point.and_then(|(x, y)| {
            let map = self.map.as_ref()?;
            map.actions
                .iter()
                .enumerate()
                .map(|(at, a)| (at, (a.x - x).powi(2) + (a.y - y).powi(2)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(at, _)| at)
        });
        let changed = nearest != self.hovered;
        self.hovered = nearest;
        changed
    }

    /// The place on the map, in its own units, that the mouse at `point` stands for, as the
    /// picture lays the map out or, with `picture` false, as the braille cells do.
    pub fn place_of(point: MapPoint, picture: bool) -> (f32, f32) {
        if picture {
            return picture::to_place(point.x, point.y, (point.width, point.height));
        }
        (point.x / point.width, 1.0 - point.y / point.height)
    }

    /// Changes whenever what the map's picture shows does.
    pub fn picture_key(&self, colours: &GroupColours) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.maps_taken.hash(&mut hasher);
        self.request.hash(&mut hasher);
        self.compared.hash(&mut hasher);
        self.lit().hash(&mut hasher);
        self.ringed().hash(&mut hasher);
        self.hovered.hash(&mut hasher);
        colours.hash(&mut hasher);
        hasher.finish()
    }

    /// The map's picture as a PNG for cells of `cell` pixels, while there is a map.
    pub fn picture(&self, colours: &GroupColours, cell: (u16, u16)) -> Option<Vec<u8>> {
        let lit = self.lit();
        let scene = picture::Scene {
            map: self.map.as_ref()?,
            colours,
            compared: self.compared,
            lit: &lit,
            ringed: self.ringed(),
            hovered: self.hovered,
        };
        Some(picture::png(&scene, cell))
    }

    /// A click on the map opens the card of the dot nearest the pointer.
    pub fn click(&mut self) -> bool {
        let (Some(at), Some(map)) = (self.hovered, &self.map) else {
            return false;
        };
        let group = map.actions[at].group;
        let behavior = map.actions[at].behavior.clone();
        self.compared.get_or_insert(group);
        let Some(row) = self
            .comparison()
            .and_then(|c| c.rows.iter().position(|r| r.behavior == behavior))
        else {
            return false;
        };
        let Some(action) = self.actions_of(row).iter().position(|a| *a == at) else {
            return false;
        };
        self.hovered = None;
        self.at = At::Card { row, action };
        true
    }

    /// The keys, in display order, in their short words where the long ones don't fit in
    /// `width` cells. An arrow is followed by two spaces, since many fonts draw it two cells
    /// wide.
    pub fn keys(&self, width: usize) -> Vec<String> {
        let long = self.keys_in(false);
        let fits = long.join(" · ").width() + 4 <= width;
        if fits { long } else { self.keys_in(true) }
    }

    fn keys_in(&self, short: bool) -> Vec<String> {
        // The deeper map's questions take the keys while they show.
        if self.deep.asking() {
            return ["↑  ↓  choose", "enter answer", "esc back"]
                .map(str::to_owned)
                .to_vec();
        }
        let word = self.request.grouping.word();
        let deeper = matches!(
            self.map.as_ref().map(|map| &map.placing),
            Some(Placing::Summaries { .. })
        );
        let pick = |long: &str, brief: &str| if short { brief } else { long }.to_owned();
        let mut keys: Vec<String> = match self.at {
            At::Map => vec![
                pick(&format!("←  →  compare a {word}"), "←  →  compare"),
                pick(
                    "↓  into the list, which lights each behavior",
                    "↓  the list",
                ),
                "g group".to_owned(),
                "p period".to_owned(),
                "esc back".to_owned(),
            ],
            At::List(_) => vec![
                format!("←  →  {word}"),
                pick("↑  ↓  behavior, lit on the map", "↑  ↓  behavior"),
                pick("enter its actions", "enter actions"),
                "g group".to_owned(),
                "p period".to_owned(),
                "esc back".to_owned(),
            ],
            At::Actions { .. } if deeper => vec![
                pick("↑  ↓  action, ringed on the map", "↑  ↓  action"),
                pick("enter its card", "enter card"),
                pick("esc back to the behaviors", "esc back"),
            ],
            At::Actions { .. } => vec![
                "↑  ↓  example".to_owned(),
                pick("enter opens it", "enter opens"),
                "esc back".to_owned(),
            ],
            At::Card { .. } => vec![
                "←  →  example".to_owned(),
                pick("o opens the run", "o run"),
                pick("c copies the action", "c copy"),
                "esc back".to_owned(),
            ],
        };
        if !self.has_actions() && self.at == At::Map {
            keys.retain(|key| {
                key.starts_with('p') || key.starts_with('g') || key.starts_with("esc")
            });
        }
        keys
    }

    /// The sub-tab's blocks for a screen `width` wide with `room` rows under the tabs.
    pub fn blocks(
        &self,
        kit: &Kit<'_>,
        colours: &GroupColours,
        width: usize,
        room: usize,
    ) -> Vec<Drawn> {
        if let Some(lines) = self.deep.questions(kit, width + 1) {
            return vec![drawn(lines)];
        }
        let Some(map) = &self.map else {
            let mut note = Drawn::new();
            note.push(vec![kit.dim(" Reading what your agents did…")]);
            return vec![note];
        };
        let look = words::Look {
            kit,
            map,
            colours,
            compared: self.compared,
            width,
            clock: self.clock,
        };
        if let At::Card { row, action } = self.at {
            let at = self.actions_of(row).get(action).copied();
            if let Some(action) = at.and_then(|at| map.actions.get(at)) {
                return vec![look.card(action)];
            }
        }
        // The map's block is the second, as MAP_BLOCK says.
        let headline = self
            .deep
            .above_map(kit)
            .map_or_else(|| look.headline(), drawn);
        let mut blocks = vec![headline];
        let lit = self.lit();
        let ringed = self.ringed().or(self.hovered);
        let map_block = match (self.picture_placed, words::legend_beside(width)) {
            (true, true) => look.beside_picture(),
            (true, false) => look.under_picture(),
            (false, _) => braille::block(&look, &lit, ringed),
        };
        // The panel sets a blank row between blocks.
        let used = blocks[0].height() + 1 + map_block.height() + 1;
        blocks.push(map_block);
        let under = match self.deep.under_map(kit) {
            Some(line) => drawn(vec![line]),
            None => self.under_map(&look, room.saturating_sub(used)),
        };
        blocks.push(under);
        blocks
    }

    /// What lights on the map: the selected behavior's dots, or its actions.
    pub fn lit(&self) -> Vec<usize> {
        match self.at {
            At::List(row) => self.row(row).map(|r| r.lit.clone()).unwrap_or_default(),
            At::Actions { row, .. } => self.actions_of(row),
            _ => Vec::new(),
        }
    }

    /// The action ringed on the map.
    pub fn ringed(&self) -> Option<usize> {
        match self.at {
            At::Actions { row, action } => self.actions_of(row).get(action).copied(),
            _ => None,
        }
    }

    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    pub fn compared(&self) -> Option<usize> {
        self.compared
    }

    pub fn map(&self) -> Option<&BehaviorMap> {
        self.map.as_ref()
    }

    /// The rows under the map, in the `room` left for them.
    fn under_map(&self, look: &words::Look<'_>, room: usize) -> Drawn {
        let listed = self.listed(look, room);
        let Some(hovered) = self.hovered.and_then(|at| look.map.actions.get(at)) else {
            return listed;
        };
        // As many rows with the mouse on a dot as without, so the panel keeps its height and
        // the map never moves under the pointer: the dot's own line at least, and the hint
        // under it where there is a row for it.
        let rows = listed.height().max(1);
        let mut out = Drawn::new();
        for line in look.pointed(hovered).into_iter().take(rows) {
            out.push(line);
        }
        while out.height() < rows {
            out.push(Vec::new());
        }
        out
    }

    /// The rows under the map while the mouse rests on none of its dots.
    fn listed(&self, look: &words::Look<'_>, room: usize) -> Drawn {
        let mut out = Drawn::new();
        // The deeper map is offered on the map itself; in the list its rows go to the list.
        let offer = look.map.offer_deeper && self.at == At::Map;
        let offer_rows = if offer { 2 } else { 0 };
        match (self.at, self.comparison()) {
            (_, None) => {
                out.push(look.pick_note(self.request.grouping));
            }
            (At::Actions { row, action }, Some(_)) => {
                let actions = self.actions_of(row);
                if let Some(ringed) = actions.get(action).and_then(|at| look.map.actions.get(*at)) {
                    out.push(look.pointed(ringed).remove(0));
                    out.push(Vec::new());
                }
                let heading = if look.summaries() { 1 } else { 2 };
                let fits = room.saturating_sub(out.height() + heading).min(LIST_ROWS);
                let behavior = self.row(row).map_or("", |r| r.behavior.as_str());
                for line in look.actions(behavior, &actions, action, self.first_action, fits) {
                    out.push(line);
                }
            }
            (at, Some(comparison)) => {
                let selected = match at {
                    At::List(row) => Some(row),
                    _ => None,
                };
                if let Some(row) = selected.and_then(|row| comparison.rows.get(row)) {
                    out.push(look.lit_line(row));
                    out.push(Vec::new());
                }
                if let Some(why) = &comparison.why_not
                    && comparison.rows.is_empty()
                {
                    out.push(vec![look.kit.dim(format!(" {why}."))]);
                }
                let shown = comparison.rows.iter().skip(self.first_row).take(LIST_ROWS);
                let marked = shown.clone().any(|row| row.new);
                let note_rows = if marked { 2 } else { 0 };
                let fits = room
                    .saturating_sub(out.height() + 1 + offer_rows + note_rows)
                    .min(LIST_ROWS);
                for line in look.list(comparison, selected, self.first_row, fits) {
                    out.push(line);
                }
                // Where the room has a row to spare, it says the list runs on.
                if out.height() + offer_rows + note_rows < room
                    && let Some(more) = look.more(comparison, self.first_row, fits)
                {
                    out.push(more);
                }
                if marked {
                    out.push(Vec::new());
                    out.push(vec![
                        look.kit
                            .dim(" new: the summary names what its label only groups"),
                    ]);
                }
            }
        }
        if offer {
            out.push(Vec::new());
            out.push(look.offer());
        }
        out
    }
}

/// The map as plain lines, for a screen reader: what it covers, each group's count, and each
/// group's distinctive behaviors.
pub fn spoken(map: &BehaviorMap) -> Vec<String> {
    let mut lines = vec![words::headline_words(map)];
    for (group, comparison) in map.groups.iter().zip(&map.comparisons) {
        lines.push(format!("{}: {} actions", group.name, group.actions));
        if let Some(why) = &comparison.why_not {
            lines.push(format!("  {why}"));
        }
        for row in comparison.rows.iter().take(LIST_ROWS) {
            lines.push(format!(
                "  {}: {} of its actions, the others {}",
                row.behavior, row.mine, row.others
            ));
        }
    }
    if map.unlabelled > 0 {
        lines.push(format!("{} actions no label fits", map.unlabelled));
    }
    lines
}

/// Rows the deeper map's flow drew, as a block of the tab's. They carry the tab's whole
/// margin, as they'd stand alone, and the panel's inset is one of its cells.
fn drawn(lines: Vec<ratatui::text::Line<'static>>) -> Drawn {
    let mut out = Drawn::new();
    for line in lines {
        let mut spans = line.spans;
        if let Some(first) = spans.first_mut()
            && let Some(rest) = first.content.strip_prefix(' ')
        {
            first.content = rest.to_owned().into();
        }
        out.push(spans);
    }
    out
}
