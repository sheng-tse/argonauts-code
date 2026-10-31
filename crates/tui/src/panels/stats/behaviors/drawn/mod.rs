//! The world drawn from a map, off the frame. A map's world is grown once: its ground, its
//! nature and the wonders the crew raised. Each look the tab asks for is drawn once from that,
//! and each scene at a moment is put together from the looks, all on a worker of its own, so no
//! frame waits on drawing. The screen gets the newest scene finished, and the ring round what
//! the pointer found is laid over it at once, drawn again only when it moved.

mod grown;
mod map;
mod planet;
mod pointer;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub use grown::{Grown, SEED};
pub use map::{Look, MapLook};
pub use planet::{Facing, PlanetLook};

use crate::world::cosmos::Cosmos;
use crate::world::planet::{Planet, Side};
pub use pointer::Marks;

use crate::world::Scene;
use crate::world::deliver::clock::Rate;
use crate::world::deliver::worker::{self, Cancel, Worker};
use crate::world::picture_key;

/// How many looks of the map are kept drawn, so going back to one costs nothing.
const LOOKS_KEPT: usize = 4;

/// A scene drawn for one look at one moment.
#[derive(Debug, Clone)]
pub struct Drawn {
    /// What it was drawn for, apart from its moment.
    pub look: u64,
    /// The motion's step it shows, `None` for the still pose.
    pub tick: Option<u64>,
    pub scene: Arc<Scene>,
    /// The world it was drawn from, which the lines of what the pointer finds read.
    pub world: Option<Arc<Grown>>,
    /// The planet it shows, the side facing you and how far its clouds have drifted, in
    /// radians, for what the pointer finds on the globe.
    pub globe: Option<Globed>,
}

/// The planet a scene shows, as the pointer reads it.
#[derive(Debug, Clone)]
pub struct Globed {
    pub planet: Arc<Planet>,
    pub side: Side,
    pub drift: f64,
    /// The earlier months on the universe's sky behind it, where the scene shows that sky.
    pub sky: Option<Arc<crate::world::cosmos::SkyClusters>>,
}

/// What a job hands back: the scene, and what the pointer reads it with.
pub struct Made {
    pub scene: Scene,
    pub world: Option<Arc<Grown>>,
    pub globe: Option<Globed>,
}

/// What the jobs draw from, kept from one job to the next.
#[derive(Default)]
pub struct Kept {
    grown: Option<(u64, Arc<Grown>)>,
    looks: Vec<(u64, Arc<MapLook>)>,
    planet: Option<(u64, PlanetLook)>,
    cosmos: Option<(u64, Cosmos)>,
}

impl Kept {
    /// The world grown from the map `key` names, grown now unless it was the last one. A
    /// growing given up before it finished keeps nothing.
    pub fn grown(&mut self, key: u64, grow: impl FnOnce() -> Option<Grown>) -> Option<Arc<Grown>> {
        match &self.grown {
            Some((kept, grown)) if *kept == key => Some(grown.clone()),
            _ => {
                let grown = Arc::new(grow()?);
                self.looks.clear();
                self.planet = None;
                self.cosmos = None;
                self.grown = Some((key, grown.clone()));
                Some(grown)
            }
        }
    }

    /// Keeps `planet` as the one `key` names.
    pub fn set_planet(&mut self, key: u64, planet: PlanetLook) {
        self.planet = Some((key, planet));
    }

    /// The world grown from the map `key` names, if it is the one kept.
    pub fn grown_kept(&self, key: u64) -> Option<Arc<Grown>> {
        self.grown
            .as_ref()
            .filter(|(kept, _)| *kept == key)
            .map(|(_, grown)| grown.clone())
    }

    pub fn has_cosmos(&self, key: u64) -> bool {
        self.cosmos.as_ref().is_some_and(|(kept, _)| *kept == key)
    }

    /// Keeps `cosmos`, made ahead of being asked for, as the one `key` names.
    pub fn offer_cosmos(&mut self, key: u64, cosmos: Cosmos) {
        if !self.has_cosmos(key) {
            self.cosmos = Some((key, cosmos));
        }
    }

    /// A sibling of the universe `key` names, if it is the one kept, to draw a level of it ahead
    /// without holding these.
    pub fn cosmos_sibling(&self, key: u64) -> Option<Cosmos> {
        self.cosmos
            .as_ref()
            .filter(|(kept, _)| *kept == key)
            .map(|(_, cosmos)| cosmos.sibling())
    }

    /// Keeps what `drawn`, a sibling of the universe `key` names, drew ahead.
    pub fn adopt_cosmos(&mut self, key: u64, drawn: Cosmos) {
        if let Some((kept, cosmos)) = &mut self.cosmos
            && *kept == key
        {
            cosmos.adopt(drawn);
        }
    }

    pub fn has_look(&self, key: u64) -> bool {
        self.looks.iter().any(|(kept, _)| *kept == key)
    }

    /// Keeps `look`, drawn ahead of being asked for, as the one `key` names, behind the look
    /// last asked for, which keeps its weather's frames.
    pub fn offer_look(&mut self, key: u64, look: MapLook) {
        if self.has_look(key) {
            return;
        }
        let at = self.looks.len().saturating_sub(1);
        self.looks.insert(at, (key, Arc::new(look)));
        if self.looks.len() > LOOKS_KEPT {
            self.looks.remove(0);
        }
    }

    /// The universe `key` names, made now unless it is the one kept.
    pub fn cosmos(&mut self, key: u64, make: impl FnOnce() -> Cosmos) -> &mut Cosmos {
        match self.cosmos.take() {
            Some((kept, cosmos)) if kept == key => &mut self.cosmos.insert((kept, cosmos)).1,
            _ => &mut self.cosmos.insert((key, make())).1,
        }
    }

    /// The planet `key` names, if it is the one kept.
    pub fn kept_planet(&mut self, key: u64) -> Option<&mut PlanetLook> {
        self.planet
            .as_mut()
            .filter(|(kept, _)| *kept == key)
            .map(|(_, planet)| planet)
    }

    /// The look `key` names, drawn now unless it is kept. The look asked for before it lets
    /// go of its weather's frames, so only the look the map shows holds them.
    pub fn look(
        &mut self,
        key: u64,
        draw: impl FnOnce() -> Option<MapLook>,
    ) -> Option<Arc<MapLook>> {
        if let Some((last, look)) = self.looks.last()
            && *last != key
            && let Some(weather) = &look.weather
        {
            weather.let_frames_go();
        }
        if let Some(at) = self.looks.iter().position(|(kept, _)| *kept == key) {
            let found = self.looks.remove(at);
            self.looks.push(found.clone());
            return Some(found.1);
        }
        let look = Arc::new(draw()?);
        if self.looks.len() == LOOKS_KEPT {
            self.looks.remove(0);
        }
        self.looks.push((key, look.clone()));
        Some(look)
    }
}

#[derive(Debug, Default)]
struct Shown {
    newest: Option<Drawn>,
    /// Counts the scenes finished, so the screen asks again when one is.
    finished: u64,
    /// The look the last job was asked for.
    asked: Option<u64>,
    /// When the world's motion clock started.
    origin: Option<Instant>,
}

/// The tab's drawings: the worker that draws them, what they draw from, the newest scene and
/// the marks laid over it.
pub struct Drawings {
    worker: Worker<Drawn>,
    shown: Mutex<Shown>,
    kept: Arc<Mutex<Kept>>,
    marks: Marks,
    /// Draws what the levels next to the one shown need, while `worker` is idle.
    ahead: Worker<()>,
    /// What `ahead` was last asked to draw.
    ahead_asked: Mutex<Option<u64>>,
}

impl std::fmt::Debug for Drawings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Drawings")
            .field("worker", &self.worker)
            .field("shown", &self.shown())
            .finish_non_exhaustive()
    }
}

impl Default for Drawings {
    /// Drawings on a thread of their own; the crate's tests read every frame whole, so theirs
    /// are drawn the moment they are asked for, unless the test asked for threads.
    fn default() -> Self {
        Self::new(Worker::off_the_frame())
    }
}

impl Drawings {
    fn new(worker: Worker<Drawn>) -> Self {
        Self {
            worker,
            shown: Mutex::default(),
            kept: Arc::default(),
            marks: Marks::default(),
            ahead: Worker::off_the_frame(),
            ahead_asked: Mutex::default(),
        }
    }

    /// What the pointer and the list last laid over a scene.
    pub fn marks(&self) -> &Marks {
        &self.marks
    }

    fn shown(&self) -> MutexGuard<'_, Shown> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes what the worker finished, the newest last.
    fn collect(&self) {
        let done = self.worker.take();
        if done.is_empty() {
            return;
        }
        let mut shown = self.shown();
        for (_, drawn) in done {
            shown.newest = Some(drawn);
            shown.finished += 1;
        }
    }

    /// Changes each time a scene lands, so the screen asks for it.
    pub fn finished(&self) -> u64 {
        self.collect();
        self.shown().finished
    }

    /// Whether a drawing is under way.
    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.worker.busy()
    }

    /// What the last drawing to panic said, once: a scene's before one drawn ahead.
    pub fn take_failure(&self) -> Option<String> {
        let scene = self.worker.take_failure();
        let ahead = self.ahead.take_failure();
        scene.or(ahead)
    }

    /// The world left the screen: every drawing asked for is stopped, and the next look asked
    /// for is drawn afresh.
    pub fn leave(&self) {
        self.worker.stop();
        self.ahead.stop();
        self.shown().asked = None;
        *self
            .ahead_asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.marks.forget();
    }

    /// Draws what `key` names ahead of being asked for, such as the planet while the map shows,
    /// so going there shows at once. It waits while a scene is drawn, and asks once for each
    /// `key`. The job takes what the jobs draw from only to read and to keep, so a scene asked
    /// for meanwhile doesn't wait on it. The crate's tests draw in line, where drawing ahead
    /// would only slow them, so theirs draw nothing ahead unless they asked for threads.
    pub fn ahead(&self, key: u64, job: impl FnOnce(&Mutex<Kept>, &Cancel) + Send + 'static) {
        if worker::in_line_here() || self.worker.busy() {
            return;
        }
        {
            let mut asked = self
                .ahead_asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if *asked == Some(key) {
                return;
            }
            *asked = Some(key);
        }
        let kept = self.kept.clone();
        self.ahead.draw_quietly(key, move |cancel| {
            job(&kept, cancel);
            None
        });
    }

    /// The step of the world's motion at `now`, stepping at `rate`, counted from the first time
    /// it was asked.
    pub fn tick(&self, now: Instant, rate: Rate) -> u64 {
        rate.steps_in(self.since_start(now))
    }

    /// How long the world's motion has run at `now`, from the first time it was asked.
    pub fn since_start(&self, now: Instant) -> Duration {
        let mut shown = self.shown();
        let origin = *shown.origin.get_or_insert(now);
        now.saturating_duration_since(origin)
    }

    /// The newest scene finished, whatever it was drawn for.
    pub fn newest(&self) -> Option<Drawn> {
        self.collect();
        self.shown().newest.clone()
    }

    /// The newest scene finished for the look last asked for, which the pointer points into.
    pub fn current(&self) -> Option<Drawn> {
        self.collect();
        let shown = self.shown();
        shown
            .newest
            .clone()
            .filter(|newest| shown.asked == Some(newest.look))
    }

    /// The scene of `look` at `tick`, `None` for the still pose: the newest scene drawn, while
    /// `draw` makes the one wanted on the worker unless it is the one shown. A scene that holds
    /// still stands for every tick of its look. A new look shows the moment it arrives; a step of
    /// the look on screen goes up with the next step's frame, a step late, so the motion costs
    /// the screen one frame a step.
    pub fn want(
        &self,
        look: u64,
        tick: Option<u64>,
        draw: impl FnOnce(&mut Kept, &Cancel) -> Option<Made> + Send + 'static,
    ) -> Option<Drawn> {
        self.ask(look, tick, false, draw)
    }

    /// As [`want`](Self::want), each step shown the moment it arrives too, as a zoom's few
    /// frames are.
    pub fn want_every_step(
        &self,
        look: u64,
        tick: Option<u64>,
        draw: impl FnOnce(&mut Kept, &Cancel) -> Option<Made> + Send + 'static,
    ) -> Option<Drawn> {
        self.ask(look, tick, true, draw)
    }

    fn ask(
        &self,
        look: u64,
        tick: Option<u64>,
        every_step: bool,
        draw: impl FnOnce(&mut Kept, &Cancel) -> Option<Made> + Send + 'static,
    ) -> Option<Drawn> {
        self.collect();
        {
            let mut shown = self.shown();
            if let Some(newest) = &shown.newest
                && newest.look == look
                && (newest.tick == tick || !newest.scene.moves)
            {
                return Some(newest.clone());
            }
            // Drawings for another look are no longer wanted. For the same look, a step that
            // comes while the last is still drawn is let go, so a slow drawing drops steps
            // rather than falling behind.
            if shown.asked != Some(look) {
                self.worker.stop();
            } else if self.worker.busy() {
                return shown.newest.clone();
            }
            shown.asked = Some(look);
        }
        let kept = self.kept.clone();
        let key = picture_key([look, tick.map_or(u64::MAX, |tick| tick)]);
        let job = move |cancel: &Cancel| {
            let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
            let made = draw(&mut kept, cancel)?;
            (!cancel.cancelled()).then(|| Drawn {
                look,
                tick,
                scene: Arc::new(made.scene),
                world: made.world,
                globe: made.globe,
            })
        };
        let stepped = self
            .shown()
            .newest
            .as_ref()
            .is_some_and(|newest| newest.look == look);
        if stepped && !every_step {
            self.worker.draw_quietly(key, job);
        } else {
            self.worker.draw(key, job);
        }
        self.newest()
    }
}
