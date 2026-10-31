//! Behaviors, the third sub-tab beside Overview and Models: a world grown from every action your
//! agents took. The map is the planet unrolled, one dot an action, coloured by its group; − goes
//! out to the planet and on to the universe, where each maker is a galaxy and each model a star.
//! It opens on every group's dots with nothing compared; ← → compares one group with the rest,
//! red behind the dots where it acts more and blue where less, with its distinctive behaviors
//! listed under the map. ↓ goes into the list, which lights the selected behavior's dots; Enter
//! lists its actions, each ringed on the map in turn, and Enter on one opens its card. The world
//! is drawn as pictures where the terminal shows them and in braille elsewhere; nothing is
//! written on it, and what the pointer rests on is said under it.

mod braille;
mod colours;
mod drawn;
mod found;
pub mod hover;
mod key_line;
mod keys;
mod layout;
mod legend;
pub mod level;
mod list;
pub mod places;
mod pointer_line;
mod pointing;
mod shortcuts;
mod spoken;
mod summary;
pub mod universe;
mod window;
mod words;

#[cfg(test)]
pub(crate) mod crews;
#[cfg(test)]
mod golden;
#[cfg(test)]
pub(crate) mod sample;
#[cfg(test)]
mod sweeps;
#[cfg(test)]
mod tests;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;

use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::behaviors::behavior_map::{BehaviorMap, MapRequest};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::settings::behaviors::BehaviorView;

pub use colours::GroupColours;
pub use found::{PICTURE_CELLS, PICTURE_COLUMN};
pub use layout::{ASSUMED_CELL, Fit, LEGEND_GAP, MAP_INSET};
pub use level::{Level, Zoom};
pub use spoken::{places, spoken};

use crate::base::kit::Kit;
use crate::frame::view::map::MapPoint;
use crate::panels::stats::deep_flow::DeepFlow;
use crate::world::Pointed;
use list::LIST_ROWS;
use universe::Universe;

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

/// Which row has the keys while the panel's keys are in the sub-tab's rows: the groupings at the
/// right of the sub-tabs, or the picture and what lies under it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stop {
    Groupings,
    #[default]
    Picture,
}

/// What the screen tells the tab as each key or frame comes: when, whether motion is held still,
/// and whether the keys are vim's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    pub now: Instant,
    /// What moves is drawn still.
    pub still: bool,
    /// A change, such as a zoom, jumps to its end: motion is off.
    pub skips_frames: bool,
    pub vim: bool,
}

impl Default for Moment {
    fn default() -> Self {
        Self {
            now: Instant::now(),
            still: false,
            skips_frames: false,
            vim: false,
        }
    }
}

/// What a key on the sub-tab asks of the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum MapKey {
    Moved,
    Ask(ViewRequest),
    Copy(String),
    /// The update of Summaries starts or stops.
    Send(UiCommand),
}

/// The sub-tab's state while the panel is open.
#[derive(Debug, Clone, Default)]
pub struct BehaviorsTab {
    pub request: MapRequest,
    map: Option<std::sync::Arc<BehaviorMap>>,
    /// The group compared with the rest; `None` shows every group's dots alike.
    compared: Option<usize>,
    pub at: At,
    /// What the pointer rests on: a dot on the map or the planet, or anything the world drew.
    pointed: Option<Pointed>,
    /// The first action shown of a behavior's, and the first behavior shown, as each list
    /// scrolls past the rows it shows at once. Each is moved as the list is drawn, in the room it
    /// has then, just far enough to keep the cursor in view.
    first_action: std::cell::Cell<usize>,
    first_row: std::cell::Cell<usize>,
    /// The clock a card's times read on.
    pub clock: crate::base::format::WallClock,
    /// The screen puts the map's picture over the cells it leaves for one, so the legend sits
    /// beside them; otherwise the map is braille with its legend under it.
    pub picture_placed: bool,
    /// The terminal's cell in pixels, where it says, which the map is sized for.
    pub cell: Option<(u16, u16)>,
    /// The most rows the panel gives the tab's blocks, where the panel says.
    most_rows: Option<usize>,
    /// The map's size and where its legend and list go, as the blocks last laid them out.
    fit: std::cell::Cell<Fit>,
    /// Counts the maps taken, so a picture of an older one is never taken for this one's.
    maps_taken: u64,
    /// The map's most used behaviors, counted once as it is taken, since every frame lists them.
    most_common: Vec<(String, usize)>,
    /// `d`: Summaries' questions, and their update while one runs.
    pub deep: DeepFlow,
    /// The world's level, and the zoom on its way to it.
    level: Level,
    zoom: Option<Zoom>,
    /// The row with the keys among the sub-tab's own.
    stop: Stop,
    /// What ← → picked in the universe, a galaxy or a system.
    picked: Option<Pointed>,
    /// The universe the map's facts make: its makers, their models and the models' bodies.
    universe: Universe,
    /// `t` laid the map flat; `n` turned the planet to its night side; `m` hid nature and the
    /// wonders.
    flat: bool,
    night: bool,
    nature_hidden: bool,
    /// The planet turned this many of the terminal's steps from facing the map's middle.
    turn: i32,
    /// Where the pointer last was over the picture, from which a drag turns the planet, and the
    /// part of a step the drag has turned it that no step has taken up yet, in radians.
    held: Option<MapPoint>,
    turning: f32,
    /// When the wheel last zoomed.
    wheeled: Option<Instant>,
    /// `?` has every key listed under the pointer's line.
    shortcuts: bool,
    /// A word about the last key, until the next.
    notice: Option<String>,
    pub moment: Moment,
    /// The world's pictures, drawn off the frame.
    drawings: std::sync::Arc<drawn::Drawings>,
    /// What the picture the pointer's line opens with shows, as the last frame left room for
    /// it, and the picture, kept while it shows.
    line_shown: std::cell::Cell<Option<found::LineIcon>>,
    line_pictures: std::sync::Arc<found::LinePictures>,
    /// The planet's turn holds while the pointer rests on its picture.
    hold: Hold,
    /// What the jobs make the universe of the map last taken from.
    cosmos: std::cell::RefCell<Option<std::sync::Arc<pointing::CosmosParts>>>,
}

/// How long the planet's turn has held, and since when it holds now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Hold {
    since: Option<Instant>,
    total: std::time::Duration,
}

impl Hold {
    /// The pointer rests on the planet's picture at `now`, or has left it.
    fn rest(&mut self, resting: bool, now: Instant) {
        match (resting, self.since) {
            (true, None) => self.since = Some(now),
            (false, Some(since)) => {
                self.total += now.saturating_duration_since(since);
                self.since = None;
            }
            _ => {}
        }
    }

    /// How long the turn has held by `now`.
    fn held(&self, now: Instant) -> std::time::Duration {
        let now_holding = self.since.map_or(std::time::Duration::ZERO, |since| {
            now.saturating_duration_since(since)
        });
        self.total + now_holding
    }
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
        self.map = Some(std::sync::Arc::new(map.clone()));
        self.maps_taken += 1;
        self.most_common = summary::most_common(map);
        self.pointed = None;
        self.universe = map.world.as_deref().map(Universe::of).unwrap_or_default();
        self.keep_level_in_universe();
        if !(again && self.compared.is_some() && self.still_there(self.at)) {
            self.at = At::Map;
            self.scroll_to_top();
        }
    }

    /// A galaxy or a system the new map's universe no longer holds goes back to the universe,
    /// and a pick it no longer holds is let go.
    fn keep_level_in_universe(&mut self) {
        let held = match self.level {
            Level::Galaxy(maker) => maker < self.universe.makers.len(),
            Level::System(star) => star < self.universe.stars.len(),
            _ => true,
        };
        if !held {
            self.level = Level::Universe;
        }
        if let Some(picked) = self.picked
            && !self.universe.picks(self.level).contains(&picked)
        {
            self.picked = None;
        }
    }

    /// The world left the screen, so nothing it was drawing is wanted any more.
    pub fn left(&self) {
        self.drawings.leave();
    }

    /// The level the world shows, or is zooming to.
    pub fn level(&self) -> Level {
        self.level
    }

    /// The zoom on its way, while it has frames to show at `now`.
    pub fn zoom(&self, now: Instant) -> Option<Zoom> {
        self.zoom.filter(|zoom| !zoom.done(now))
    }

    pub fn stop(&self) -> Stop {
        self.stop
    }

    /// What ← → picked in the universe, a galaxy or a system.
    pub fn picked(&self) -> Option<Pointed> {
        self.picked
    }

    pub fn pointed(&self) -> Option<Pointed> {
        self.pointed
    }

    pub fn universe(&self) -> &Universe {
        &self.universe
    }

    /// The world's own toggles: the map laid flat, the planet's night side, nature hidden.
    pub fn flat(&self) -> bool {
        self.flat
    }

    pub fn night(&self) -> bool {
        self.night
    }

    pub fn nature_hidden(&self) -> bool {
        self.nature_hidden
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Whether the tab draws the world as pictures: where the screen places them, unless
    /// `behaviors.view` asks for braille; `world` draws them wherever the screen can.
    pub fn draws_world(&self) -> bool {
        self.picture_placed && self.view() != BehaviorView::Braille
    }

    /// `behaviors.view` as the map now taken carries it.
    pub fn view(&self) -> BehaviorView {
        self.map.as_ref().map_or(BehaviorView::Auto, |map| map.view)
    }

    fn compared_group(&self) -> Option<&str> {
        let map = self.map.as_ref()?;
        Some(map.groups.get(self.compared?)?.name.as_str())
    }

    /// Whether the map holds any action, which `d` summarizes and the arrows compare.
    fn has_actions(&self) -> bool {
        self.map.as_ref().is_some_and(|map| !map.actions.is_empty())
    }

    /// The map is on screen, so the mouse points at its dots.
    pub fn map_shown(&self) -> bool {
        self.map.is_some() && !matches!(self.at, At::Card { .. })
    }

    /// Changes whenever what the map's picture shows does.
    pub fn picture_key(&self, colours: &GroupColours) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.maps_taken.hash(&mut hasher);
        self.request.hash(&mut hasher);
        self.compared.hash(&mut hasher);
        self.lit().hash(&mut hasher);
        self.drawings.finished().hash(&mut hasher);
        self.ringed().hash(&mut hasher);
        self.hovered().hash(&mut hasher);
        self.level.hash(&mut hasher);
        self.turn.hash(&mut hasher);
        self.picked.map(|p| format!("{p:?}")).hash(&mut hasher);
        self.pointed.map(|p| format!("{p:?}")).hash(&mut hasher);
        colours.hash(&mut hasher);
        hasher.finish()
    }

    /// What the words read the map with, for a screen `width` wide.
    fn look<'a>(
        &'a self,
        kit: &'a Kit<'a>,
        map: &'a BehaviorMap,
        colours: &'a GroupColours,
        names: &'a DisplayNames,
        width: usize,
    ) -> words::Look<'a> {
        words::Look {
            kit,
            map,
            colours,
            compared: self.compared,
            width,
            clock: self.clock,
            names,
            world: map.world.as_deref(),
            universe: &self.universe,
            level: self.level,
            picked: self.picked,
            arrows: if self.moment.vim { "h  l" } else { "←  →" },
            logos: self.draws_world(),
            planet: self.planet_view(),
            fit: self.fit.get(),
        }
    }

    pub fn compared(&self) -> Option<usize> {
        self.compared
    }

    pub fn map(&self) -> Option<&BehaviorMap> {
        self.map.as_deref()
    }
}
