//! What the line under the picture says the pointer rests on, with your own numbers. Nothing in
//! the world is listed ahead of time: a mark of nature, a wonder or a rare body names itself only
//! here, when the pointer finds it, so each crew finds different things.
//!
//! Every line starts with a capital, a behavior is quoted as ‘read a source file’, and a model
//! stands after the logo of whoever serves it, without its maker's word where the logo is the
//! maker's own.

use argonauts_core::behavior_world::{ModelBehavior, ModelFacts, RisenWonder, Wonder, WorldFacts};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::logos::{Logo, logo_for};
use ratatui::text::Span;
use time::OffsetDateTime;

use super::universe::{Body, PLANETS, Universe};
use super::words::Look;
use crate::format::WallClock;
use crate::kit::logo::LOGO_WIDTH;
use crate::text::str_width;
use crate::theme::Role;
use crate::world::Pointed;
use crate::world::cosmos::{maker_logo, served_logo};

/// A hover line takes at most two rows of this many cells.
pub const LINE_CELLS: usize = 116;

/// `text` with its first letter a capital.
pub fn cap(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A behavior as a line names it: ‘read a source file’.
pub fn quoted(behavior: &str) -> String {
    format!("‘{behavior}’")
}

pub fn plural(count: u32, one: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        _ => format!("{count} {one}s"),
    }
}

/// `today`, `1 day ago` or `8 days ago`.
pub fn day_word(days: i64) -> String {
    match days {
        ..=0 => "today".to_owned(),
        1 => "1 day ago".to_owned(),
        days => format!("{days} days ago"),
    }
}

/// `3 min ago`, `5 h ago`, or a day word past a day.
fn ago(minutes: i64) -> String {
    match minutes {
        ..60 => format!("{} min ago", minutes.max(1)),
        60..1440 => format!("{} h ago", (minutes as f64 / 60.0).round() as i64),
        _ => day_word(minutes / 1440),
    }
}

fn percent(part: u32, whole: u32) -> u32 {
    (f64::from(part) * 100.0 / f64::from(whole.max(1))).round() as u32
}

/// What raises each wonder, as its line says once the pointer finds it.
pub fn trigger(wonder: Wonder) -> &'static str {
    match wonder {
        Wonder::Argo => {
            "Your first workflow run with models from three or more makers aboard lands its work"
        }
        Wonder::Fleece => {
            "A run of five or more tasks lands with every task merged on its first try: no failed \
             check, nothing sent back"
        }
        Wonder::Symplegades => {
            "One task's checks fail five times in a row, and it still gets through and merges"
        }
        Wonder::Talos => {
            "Your guard checks (protected paths, secrets, dependencies) stop work ten times, and \
             nothing they guard is ever merged past them"
        }
        Wonder::Sirens => {
            "An agent tries to delete or skip a test to pass, the test check catches it, and the \
             task later merges with every test kept"
        }
        Wonder::Dragon => {
            "A run works through your night, six hours or more with nothing from you while your \
             rules and the judge decide, and it lands"
        }
        Wonder::Harpies => {
            "Usage limits snatch one task's model three times, it falls to another model each \
             time, and it still merges"
        }
        Wonder::Dodona => {
            "A council with seats from three or more makers talks for two rounds or more, agrees, \
             and the work it planned lands"
        }
        Wonder::Pyramid => "Your crew has merged work on 100 different days",
        Wonder::Gardens => {
            "Your crew adds tests in 20 tasks that merge, with every check passing at each merge"
        }
        Wonder::Colossus => "One workflow runs ten or more agents and lands its work",
        Wonder::Pharos => {
            "Work built by a model on your own machine merges after another maker's model \
             reviewed it"
        }
        Wonder::Artemis => {
            "A run that stopped is carried on with /resume and lands what it could not before"
        }
        Wonder::Zeus => {
            "The judge settles 25 disagreements between a worker and its reviewer, and nothing it \
             let stand fails its checks after merging"
        }
        Wonder::Mausoleum => {
            "A model that did 500 or more of your actions has done none for 60 days, and a newer \
             model of its line took its place"
        }
        Wonder::Atlantis => {
            "A behavior that was one of your five most used goes unused for 30 days while your \
             crew keeps working"
        }
    }
}

/// What a land or sea's climate is called, and what makes it, by when its behavior was last used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Climate {
    Rainforest,
    Forest,
    Grassland,
    Savanna,
    Desert,
}

impl Climate {
    pub fn of(last_used: u32, days_in_last_7: u8) -> Self {
        match last_used {
            _ if days_in_last_7 >= 4 => Climate::Rainforest,
            0..7 => Climate::Forest,
            7..14 => Climate::Grassland,
            14..21 => Climate::Savanna,
            _ => Climate::Desert,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Climate::Rainforest => "Rainforest",
            Climate::Forest => "Forest",
            Climate::Grassland => "Grassland",
            Climate::Savanna => "Savanna",
            Climate::Desert => "Desert",
        }
    }

    pub fn rule(self) -> &'static str {
        match self {
            Climate::Rainforest => "used on 4 or more of the last 7 days",
            Climate::Forest => "used in the last 7 days",
            Climate::Grassland => "last used 7 to 13 days ago",
            Climate::Savanna => "last used 14 to 20 days ago",
            Climate::Desert => "last used 21 or more days ago",
        }
    }

    /// Seconds a turn of the innermost planet of this climate takes; a desert's stands still.
    fn orbit(self) -> u32 {
        match self {
            Climate::Rainforest => 20,
            Climate::Forest => 40,
            Climate::Grassland => 80,
            Climate::Savanna => 160,
            Climate::Desert => 0,
        }
    }
}

/// What only the planet shows and the map's own lines don't say: the aurora on its night side
/// and the clouds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nature {
    Aurora,
    Clouds,
}

/// What a line reads beside the facts: the world's own, the map's counts, the clock, and names.
pub struct Reading<'a> {
    pub world: &'a WorldFacts,
    /// Every action on the map, and those no label fits.
    pub actions: u32,
    pub unlabelled: u32,
    pub clock: WallClock,
    pub names: &'a DisplayNames,
}

impl Reading<'_> {
    /// Days from `at` to the end of the period, in your time zone.
    fn days_back(&self, at: OffsetDateTime) -> i64 {
        let end = self.clock.to_local(self.world.end).date();
        (end - self.clock.to_local(at).date()).whole_days()
    }

    fn minutes_back(&self, at: OffsetDateTime) -> i64 {
        (self.world.end - at).whole_minutes()
    }

    fn when(&self, at: OffsetDateTime) -> String {
        format!(
            "{} at {}",
            day_word(self.days_back(at)),
            self.clock.clock(at)
        )
    }
}

impl Nature {
    /// Its line in your own numbers.
    pub fn line(self, reading: &Reading<'_>) -> Option<String> {
        let text = match self {
            Nature::Aurora => {
                let night = reading.world.crew.night_actions;
                format!(
                    "aurora · {night} actions between 10:00 PM and 5:00 AM ({}%)",
                    percent(night, reading.actions)
                )
            }
            Nature::Clouds => format!(
                "clouds · {} actions fit no label ({}%)",
                reading.unlabelled,
                percent(reading.unlabelled, reading.actions + reading.unlabelled)
            ),
        };
        Some(cap(&text))
    }
}

/// A risen wonder's two rows: its name and what raised it in your numbers, with each model it
/// names; then the rule that raises it and the behavior it rose beside.
pub fn wonder_lines(wonder: &RisenWonder, names: &[String]) -> [String; 2] {
    let named = if names.is_empty() {
        String::new()
    } else {
        format!(": {}", names.join(", "))
    };
    [
        format!("{} · {}{named}", wonder.wonder.name(), wonder.why),
        format!(
            "{} · about {}",
            trigger(wonder.wonder),
            quoted(&wonder.behavior)
        ),
    ]
}

/// A risen wonder's rows as the line under the picture says them, each model it names after
/// the logo of whoever serves it.
pub(super) fn wonder_said(wonder: &RisenWonder, look: &Look<'_>) -> Vec<Said> {
    let mut logos = Vec::new();
    let named: Vec<String> = wonder
        .models
        .iter()
        .map(|written| {
            let (cli, model) = written.split_once('/').unwrap_or((written.as_str(), ""));
            let model = Some(model).filter(|model| !model.is_empty());
            let name = look
                .names
                .model(cli, model)
                .unwrap_or_else(|| look.names.cli(cli));
            let logo = logo_for(cli, model);
            let shown = look.named_after(&logo, &name);
            logos.push(logo);
            format!("{LOGO} {shown}")
        })
        .collect();
    let [head, tail] = wonder_lines(wonder, &named);
    vec![Said::with(head, logos), Said::plain(tail)]
}

/// What a model's star is, by how you use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarKind {
    Star,
    /// New to you: under 30 actions, first used in the last 14 days.
    Protostar,
    /// Once busy, now quiet: 100 or more actions, none in the last 7 days.
    RedGiant,
    /// An older model of a line whose newer model did more in the last 7 days.
    WhiteDwarf,
    /// 20 or more actions, and none of its work merged.
    BlackHole,
}

impl StarKind {
    pub fn of(model: &ModelFacts, world: &WorldFacts) -> Self {
        let newer = model
            .newer
            .as_ref()
            .and_then(|newer| world.models.iter().find(|other| other.model == newer.model));
        if newer.is_some_and(|newer| newer.last_7_days > model.last_7_days) {
            StarKind::WhiteDwarf
        } else if model.actions >= 20 && model.merged == 0 && model.sent_back + model.dropped > 0 {
            StarKind::BlackHole
        } else if model.actions >= 100 && model.last_7_days == 0 {
            StarKind::RedGiant
        } else if model.actions < 30 && model.first_used < 14 {
            StarKind::Protostar
        } else {
            StarKind::Star
        }
    }

    fn phrase(self) -> &'static str {
        match self {
            StarKind::Star => "a star",
            StarKind::Protostar => "a protostar, still forming",
            StarKind::RedGiant => "a red giant",
            StarKind::WhiteDwarf => "a white dwarf",
            StarKind::BlackHole => "a black hole",
        }
    }
}

/// Joins `parts` with ` · ` as far as they fit in a row, always the first.
fn fit(parts: &[String]) -> String {
    let mut out = String::new();
    for part in parts.iter().filter(|part| !part.is_empty()) {
        let next = if out.is_empty() {
            part.clone()
        } else {
            format!("{out} · {part}")
        };
        if !out.is_empty() && unicode_width::UnicodeWidthStr::width(next.as_str()) > LINE_CELLS {
            break;
        }
        out = next;
    }
    out
}

/// Where a logo stands in a hover line's words, before the name it belongs to.
pub const LOGO: &str = "{L}";

/// One row of a hover line: its words, each [`LOGO`] in them standing for the next of `logos`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub text: String,
    pub logos: Vec<Logo>,
}

impl Said {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            logos: Vec::new(),
        }
    }

    pub fn with(text: String, logos: Vec<Logo>) -> Self {
        Self { text, logos }
    }

    /// Its words as a screen reader hears them, without the logos.
    pub fn words(&self) -> String {
        self.text.replace(&format!("{LOGO} "), "")
    }

    /// Its width in cells as a row draws it, each logo [`LOGO_WIDTH`] cells where logos show.
    fn cells(&self, logos: bool) -> usize {
        drawn_width(&self.text, logos)
    }

    /// The row in `room` cells: whole where it fits, else ended at its last blank that leaves
    /// it fitting, with the rest a second row. A ` · ` between parts goes with the break, and a
    /// logo stays with the name after it.
    pub fn flowed(self, room: usize, logos: bool) -> Vec<Said> {
        if self.cells(logos) <= room {
            return vec![self];
        }
        const PARTS: &str = " · ";
        let text = &self.text;
        let fits = |end: usize| drawn_width(&text[..end], logos) <= room;
        let break_at = |at: usize| {
            if text[at..].starts_with(PARTS) {
                (at, PARTS.len())
            } else if text[..at].ends_with(PARTS.trim_end()) {
                (at - PARTS.trim_end().len(), PARTS.len())
            } else {
                (at, 1)
            }
        };
        let Some((at, gap)) = text
            .rmatch_indices(' ')
            .map(|(at, _)| break_at(at))
            .find(|&(at, _)| at > 0 && !text[..at].ends_with(LOGO) && fits(at))
        else {
            return vec![self];
        };
        let marks = text[..at].matches(LOGO).count();
        let mut first = self.logos;
        let rest = first.split_off(marks.min(first.len()));
        vec![
            Said::with(text[..at].to_owned(), first),
            Said::with(text[at + gap..].to_owned(), rest),
        ]
    }
}

/// `text`'s width in cells as a row draws it: each logo [`LOGO_WIDTH`] cells where logos show,
/// and where they don't, none, nor the blank after it.
fn drawn_width(text: &str, logos: bool) -> usize {
    if logos {
        let marks = text.matches(LOGO).count();
        str_width(&text.replace(LOGO, "")) + marks * usize::from(LOGO_WIDTH)
    } else {
        str_width(&text.replace(&format!("{LOGO} "), ""))
    }
}

/// What the universe says of what the pointer rests on or ← → picked: one or two rows. `named`
/// words a model as the line says it after `logo`, which the line puts before it.
pub fn cosmos_lines(
    pointed: Pointed,
    universe: &Universe,
    world: &WorldFacts,
    reading: &Reading<'_>,
    level: super::Level,
    named: &dyn Fn(&Logo, &ModelFacts) -> String,
) -> Option<Vec<Said>> {
    let share = |part: u32| format!("{}%", percent(part, universe.actions));
    match pointed {
        Pointed::Home => {
            let newest = world
                .models
                .iter()
                .filter_map(|m| m.last_action)
                .max()
                .map_or_else(String::new, |at| {
                    format!(", the newest {}", ago(reading.minutes_back(at)))
                });
            let sessions: u32 = world.models.iter().map(|m| m.sessions).sum();
            Some(vec![
                Said::plain(format!(
                    "Your world · {} actions in {}{newest}",
                    reading.actions,
                    plural(sessions, "session")
                )),
                Said::plain("Every galaxy round it is a maker whose models did those actions"),
            ])
        }
        Pointed::Galaxy(at) => {
            let maker = universe.maker(at)?;
            let models: Vec<&ModelFacts> = maker
                .stars
                .iter()
                .filter_map(|star| world.models.get(universe.stars.get(*star)?.model))
                .collect();
            let failed: u32 = models.iter().map(|m| m.failed).sum();
            let recent: u32 = models.iter().map(|m| m.last_7_days).sum();
            let name = argonauts_core::registry::provider::provider_name(&maker.key);
            let failed = if failed > 0 {
                format!(" · {failed} failed")
            } else {
                String::new()
            };
            let head = format!(
                "{LOGO} {name} · {} actions, {} of yours · {}{failed}",
                maker.actions,
                share(maker.actions),
                plural(u32::try_from(models.len()).unwrap_or(u32::MAX), "model")
            );
            let motion = if recent > 0 {
                format!(
                    "Turning: {}% of its actions came in the last 7 days",
                    percent(recent, maker.actions)
                )
            } else {
                "Still: none of its actions came in the last 7 days".to_owned()
            };
            let dwarf = if percent(maker.actions, universe.actions) < 3 {
                format!("A dwarf galaxy, under 3% of your actions · {motion}")
            } else {
                motion
            };
            let inside = matches!(level, super::Level::Galaxy(_));
            let key = if inside { "" } else { "enter to go in" };
            Some(vec![
                Said::with(head, vec![maker_logo(&maker.key)]),
                Said::plain(fit(&[dwarf, key.to_owned()])),
            ])
        }
        Pointed::Star(at) => {
            let star = universe.star(at)?;
            let model = world.models.get(star.model)?;
            let maker = universe.makers.get(star.maker)?;
            let maker_name = argonauts_core::registry::provider::provider_name(&maker.key);
            let kind = StarKind::of(model, world);
            let logo = served_logo(model);
            let name = named(&logo, model);
            let failed = if model.failed > 0 {
                format!("{} failed", model.failed)
            } else {
                String::new()
            };
            let used = model.last_action.map_or_else(String::new, |at| {
                format!("last used {}", ago(reading.minutes_back(at)))
            });
            // A rare star names itself on the first row, so the room left for the rest can't
            // drop it: in a binary with the star it goes round with, in a supernova's remnant.
            let mut logos = vec![logo];
            let binary = star.binary.and_then(|other| {
                let other = world.models.get(universe.stars.get(other)?.model)?;
                let logo = served_logo(other);
                let name = named(&logo, other);
                logos.push(logo);
                Some(format!(" in a binary with {LOGO} {name}"))
            });
            let remnant = if star.supernova {
                " in a supernova remnant"
            } else {
                ""
            };
            let head = fit(&[
                format!(
                    "{LOGO} {name} · {}{}{remnant}",
                    kind.phrase(),
                    binary.unwrap_or_default()
                ),
                format!("{} actions", model.actions),
                failed,
                used,
            ]);
            let first = maker.stars.first() == Some(&(at as usize));
            let why = match kind {
                StarKind::Star if first => format!("At the core: {maker_name}'s most used model"),
                StarKind::Star => {
                    format!("Further out the more different its work is from {maker_name}'s mix")
                }
                StarKind::Protostar => format!(
                    "New to you: under 30 actions, first used {}",
                    day_word(i64::from(model.first_used))
                ),
                StarKind::RedGiant => {
                    "Once busy, now quiet: 100 or more actions, none in the last 7 days".to_owned()
                }
                StarKind::WhiteDwarf => "Replaced by a newer model of its line".to_owned(),
                StarKind::BlackHole => format!(
                    "Its work went in and never came out: none merged, {} dropped, {} sent back",
                    model.dropped, model.sent_back
                ),
            };
            let key = match level {
                super::Level::Universe => "enter to visit its galaxy",
                super::Level::Galaxy(_) => "enter to see its system",
                _ => "",
            };
            Some(vec![
                Said::with(head, logos),
                Said::plain(fit(&[why, key.to_owned()])),
            ])
        }
        Pointed::Body(at) => {
            let (star, body) = match universe.body(at)? {
                Body::Planet { star, behavior } => (star, Some(behavior)),
                Body::Belt { star } => (star, None),
                other => return between_lines(other, universe, world, reading, named),
            };
            let model = world.models.get(universe.stars.get(star)?.model)?;
            let logo = served_logo(model);
            let name = named(&logo, model);
            match body {
                Some(behavior) => {
                    let b = model.behaviors.get(behavior)?;
                    Some(planet_lines(b, behavior, &name, logo))
                }
                None => {
                    let rest = &model.behaviors[PLANETS.min(model.behaviors.len())..];
                    let actions: u32 = rest.iter().map(|b| b.count).sum();
                    Some(vec![
                        Said::with(
                            format!(
                                "Asteroid belt · {LOGO} {name} · {} in {}",
                                plural(actions, "action"),
                                plural(u32::try_from(rest.len()).unwrap_or(0), "other behavior")
                            ),
                            vec![logo],
                        ),
                        Said::plain(
                            "One rock per action, round the belt the days of the period, today at \
                             the front; red rocks failed",
                        ),
                    ])
                }
            }
        }
        Pointed::Sky(month) => {
            let months = &world.crew.earlier_months;
            let earlier: u32 = months.iter().map(|m| m.actions).sum();
            let lines = match month
                .and_then(|m| months.get(usize::try_from(m).ok()?).map(|o| (m, o)))
            {
                Some((back, month)) => [
                    format!(
                        "The far stars · {} of your actions from {} before this period, one star \
                         each",
                        thousands(month.actions),
                        match back {
                            0 => "the month".to_owned(),
                            back => format!("{} months", back + 1),
                        }
                    ),
                    "One cluster per earlier month: the older the month, the dimmer its stars"
                        .to_owned(),
                ],
                None => [
                    format!(
                        "The far stars · your {} actions before this period, one star each, in {}, \
                         one per month",
                        thousands(earlier),
                        plural(u32::try_from(months.len()).unwrap_or(0), "cluster")
                    ),
                    "The faintest haze is the same for everyone · the sky fills as your crew works"
                        .to_owned(),
                ],
            };
            Some(lines.into_iter().map(Said::plain).collect())
        }
        Pointed::Action(_) | Pointed::Feature(_) | Pointed::Wonder(_) | Pointed::Ground(_) => None,
    }
}

/// What the universe says of a moon, a flare, a comet or a tidal stream.
fn between_lines(
    body: Body,
    universe: &Universe,
    world: &WorldFacts,
    reading: &Reading<'_>,
    named_after: &dyn Fn(&Logo, &ModelFacts) -> String,
) -> Option<Vec<Said>> {
    let star_model = |star: usize| world.models.get(universe.stars.get(star)?.model);
    let named = |model: &ModelFacts, logos: &mut Vec<Logo>| {
        let logo = served_logo(model);
        let name = named_after(&logo, model);
        logos.push(logo);
        format!("{LOGO} {name}")
    };
    let mut logos = Vec::new();
    let (head, tail) = match body {
        Body::Moon {
            star,
            behavior,
            planet,
        } => {
            let model = star_model(star)?;
            let moon = model.behaviors.get(behavior)?;
            let around = model.behaviors.get(planet)?;
            let name = named(model, &mut logos);
            (
                format!(
                    "{} · a moon of {name}'s {} · {}",
                    cap(&moon.behavior),
                    quoted(&around.behavior),
                    plural(moon.count, "action")
                ),
                "Moons are the finer behaviors of the one they go round".to_owned(),
            )
        }
        Body::Flare { star, failure } => {
            let model = star_model(star)?;
            let failed = model.cli_failures.get(failure)?;
            let name = named(model, &mut logos);
            (
                format!(
                    "Flare · {name}'s CLI failed in a session {}",
                    reading.when(failed.at)
                ),
                "One flare per session that ended because its CLI failed".to_owned(),
            )
        }
        Body::Comet { run, from, to } => {
            let mixed = world.mixed_runs.get(run)?;
            let from = named(star_model(from)?, &mut logos);
            let to = named(star_model(to)?, &mut logos);
            let aboard = match mixed.makers.len() {
                0..=2 => String::new(),
                makers => format!(", {makers} makers aboard"),
            };
            (
                format!(
                    "Comet · a run {}, from {from} to {to}{aboard} · {}",
                    reading.when(mixed.at),
                    plural(mixed.actions, "action")
                ),
                "Comets are runs of the last 24 hours with two makers or more, flying from the \
                 one that started"
                    .to_owned(),
            )
        }
        Body::Stream { pair } => {
            let shared = world.pairs.get(pair)?;
            let model = |id: &str| world.models.iter().find(|model| model.model == id);
            let first = named(model(&shared.models[0])?, &mut logos);
            let second = named(model(&shared.models[1])?, &mut logos);
            (
                format!(
                    "Tidal stream · {first} and {second} shared {}",
                    plural(shared.runs, "run")
                ),
                format!(
                    "{:.1} times as many as chance would give: they work together",
                    shared.lift
                ),
            )
        }
        Body::Planet { .. } | Body::Belt { .. } => return None,
    };
    Some(vec![Said::with(head, logos), Said::plain(tail)])
}

/// A planet of a model's system: its behavior and numbers, then its climate and its turn.
fn planet_lines(b: &ModelBehavior, at: usize, model: &str, logo: Logo) -> Vec<Said> {
    let climate = Climate::of(b.last_used, b.days_in_last_7);
    let failed = if b.failed > 0 {
        format!(", {} failed", b.failed)
    } else {
        String::new()
    };
    let mut rows = Vec::new();
    if b.count > 0 && b.failed * 10 >= b.count * 3 {
        rows.push(format!(
            "Lava world: {} of its {} failed",
            b.failed, b.count
        ));
    }
    rows.push(format!("{}: {}", climate.word(), climate.rule()));
    match b.runs_in_a_row {
        0 => {}
        1 => rows.push("rings: it looped once".to_owned()),
        n => rows.push(format!("rings: it looped {n} times")),
    }
    rows.push(match climate.orbit() {
        0 => "still: not used in 14 days".to_owned(),
        seconds => {
            let turn = (f64::from(seconds) * (1.0 + 0.45 * at as f64)).round();
            format!("once round in {turn} s")
        }
    });
    vec![
        Said::with(
            fit(&[
                format!("{} · a planet of {LOGO} {model}", cap(&b.behavior)),
                format!("{}{failed}", plural(b.count, "action")),
                format!("last used {}", day_word(i64::from(b.last_used))),
            ]),
            vec![logo],
        ),
        Said::plain(fit(&rows)),
    ]
}

/// `1,234`, as the far stars count them.
fn thousands(count: u32) -> String {
    crate::format::thousands(u64::from(count))
}

impl Look<'_> {
    /// The world's line for a dot under the pointer: its behavior, whose it is with that
    /// group's count and everyone's, and when; while a group is compared, whether it does this
    /// more or less here.
    pub fn dot_line(&self, at: usize) -> Option<Vec<Span<'static>>> {
        let kit = self.kit;
        let action = self.map.actions.get(at)?;
        let group = self.map.groups.get(action.group)?;
        let everyone = self
            .map
            .actions
            .iter()
            .filter(|a| a.behavior == action.behavior)
            .count();
        let mine = self
            .map
            .actions
            .iter()
            .filter(|a| a.behavior == action.behavior && a.group == action.group)
            .count();
        let here = self
            .compared
            .and_then(|g| super::words::compare_class(self.map, g, &action.behavior))
            .map(|class| format!("{} · ", class.word()));
        let days = (self.clock.now().date() - self.clock.to_local(action.at).date()).whole_days();
        let when = format!("{} at {}", day_word(days), self.clock.clock(action.at));
        let mut line = vec![
            Span::styled("◌ ", self.colours.style(action.group)),
            Span::styled(
                cap(&format!("{}{}", here.unwrap_or_default(), action.behavior)),
                kit.style(Role::Text),
            ),
        ];
        // Summaries' list leaves out the label each summary was grouped under; it's said here.
        if self.summaries() {
            line.push(kit.dim(format!(" · label: {}", action.label)));
        }
        line.push(kit.dim(" · "));
        line.extend(self.group_label(action.group));
        line.push(kit.dim(format!(
            " · {mine} of its {} · everyone {everyone} · {when}",
            group.actions
        )));
        Some(line)
    }

    /// One row of a hover line as spans, each logo before its name. The second row is set in
    /// as far as the first row's words start after its logo.
    pub fn said(&self, said: &Said, first: bool) -> Vec<Span<'static>> {
        let kit = self.kit;
        let style = if first {
            kit.style(Role::Text)
        } else {
            kit.style(Role::Dim)
        };
        let mut line = Vec::new();
        if !first && self.logos {
            line.push(Span::raw("   "));
        } else if !first {
            line.push(Span::raw("  "));
        }
        if !self.logos {
            line.push(Span::styled(said.words(), style));
            return line;
        }
        let mut logos = said.logos.iter();
        for (at, words) in said.text.split(LOGO).enumerate() {
            if at > 0
                && let Some(logo) = logos.next()
            {
                line.extend(crate::kit::logo::logo_spans(logo, kit.theme));
            }
            if !words.is_empty() {
                line.push(Span::styled(words.to_owned(), style));
            }
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_too_wide_goes_on_in_the_row_under_it() {
        let logo = logo_for("claude-code", None);
        let said = Said::with(
            format!("Earthquake · {LOGO} Claude Code crashed an hour ago · 2 sessions ended here"),
            vec![logo.clone()],
        );
        assert_eq!(said.clone().flowed(80, true), std::slice::from_ref(&said));

        // At its last blank that leaves the row fitting, a ` · ` there going with the break.
        let rows = said.clone().flowed(50, true);
        assert_eq!(
            rows,
            [
                Said::with(
                    format!("Earthquake · {LOGO} Claude Code crashed an hour ago"),
                    vec![logo.clone()]
                ),
                Said::plain("2 sessions ended here"),
            ]
        );
        assert_eq!(rows[0].cells(true), 47);
        assert_eq!(rows[0].cells(false), 44);

        // Never between a logo and its name.
        let said = Said::with(
            format!("{LOGO} Claude Code crashed an hour ago · 2 sessions"),
            vec![logo.clone()],
        );
        assert_eq!(
            said.clone().flowed(16, true),
            [
                Said::with(format!("{LOGO} Claude Code"), vec![logo]),
                Said::plain("crashed an hour ago · 2 sessions"),
            ]
        );
        assert_eq!(said.clone().flowed(2, true), [said]);
    }

    #[test]
    fn a_climate_follows_when_its_behavior_was_last_used() {
        assert_eq!(Climate::of(0, 4), Climate::Rainforest);
        assert_eq!(Climate::of(6, 1), Climate::Forest);
        assert_eq!(Climate::of(7, 0), Climate::Grassland);
        assert_eq!(Climate::of(14, 0), Climate::Savanna);
        assert_eq!(Climate::of(21, 0), Climate::Desert);
    }

    #[test]
    fn day_words_and_capitals_read_as_the_world_writes_them() {
        assert_eq!(day_word(0), "today");
        assert_eq!(day_word(1), "1 day ago");
        assert_eq!(day_word(8), "8 days ago");
        assert_eq!(cap("hurricane, category 1"), "Hurricane, category 1");
        assert_eq!(quoted("read a source file"), "‘read a source file’");
    }
}
