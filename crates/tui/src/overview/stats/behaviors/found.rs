//! What the pointer found on the world the map or the planet drew, and the picture the line
//! under the map opens with for it: the kind of nature, land or ice, or the wonder, on a tile
//! two cells wide, so the line shows what to look for.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, PoisonError};

use argonauts_core::behavior_world::Wonder;

use super::BehaviorsTab;
use super::drawn::Drawn;
use super::hover;
use crate::world::map::land::{self, GroundKind, GroundLine, IceKind};
use crate::world::map::nature::Kind;
use crate::world::planet::OnGlobe;
use crate::world::{Image, PartId, Picture, Plate, Pointed, picture_key};

/// Names the pictures the pointer's line opens with.
const LINE: PartId = PartId::new("behaviors line picture", 0);

/// Where the line's picture sits in its row, from the start of the tab's blocks: after the
/// blank every line under the map starts with. The line's words follow one blank after it.
pub const PICTURE_COLUMN: u16 = 1;
/// The cells the line's picture covers.
pub const PICTURE_CELLS: u16 = 2;

/// What the pointer found on the world drawn on the map or the planet.
pub(super) enum Found {
    /// A mark of nature, a kind of land or a shoal, by its place in nature's list.
    Nature(usize),
    /// A wonder, by its place among those the map shows.
    Wonder(usize),
    /// The land, the sea or the ice under the pointer.
    Ground(GroundLine),
    /// The planet's aurora or its clouds.
    Air(hover::Nature),
}

/// What the picture before the pointer's line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineIcon {
    /// A kind of nature, land or ice, by the word nature names it with. A southern storm's is
    /// mirrored, its arms winding as the storm on the map does.
    Kind {
        word: &'static str,
        south: bool,
    },
    Wonder(Wonder),
}

impl LineIcon {
    fn kind(word: &'static str) -> Self {
        Self::Kind { word, south: false }
    }

    /// Its picture, two terminal cells.
    fn draw(self) -> Option<Image> {
        let canvas = match self {
            Self::Kind { word, south } => crate::world::map::weather::icon::icon(word, south)?,
            Self::Wonder(wonder) => crate::world::wonders::icon(wonder),
        };
        Some(canvas.to_image())
    }
}

/// The line's picture last drawn, kept while the pointer stays on the kind it shows, so a frame
/// asks for it at no cost.
#[derive(Debug, Default)]
pub struct LinePictures(Mutex<Option<(LineIcon, Picture)>>);

impl LinePictures {
    fn get(&self, icon: LineIcon) -> Option<Picture> {
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((shown, picture)) = kept.as_ref()
            && *shown == icon
        {
            return Some(picture.clone());
        }
        let mut hasher = DefaultHasher::new();
        icon.hash(&mut hasher);
        let picture = Picture::new(picture_key([LINE.get(), hasher.finish()]), icon.draw()?);
        *kept = Some((icon, picture.clone()));
        Some(picture)
    }
}

impl BehaviorsTab {
    /// What the pointer found on the world `drawn` shows: on the planet, its own ice, aurora
    /// and clouds answer before the ground under them.
    pub(super) fn found(&self, drawn: &Drawn) -> Option<Found> {
        let grown = drawn.world.as_deref()?;
        let at = |index: u32| usize::try_from(index).ok();
        let found = match self.pointed? {
            Pointed::Feature(index) => Found::Nature(at(index)?),
            Pointed::Wonder(index) => Found::Wonder(at(index)?),
            Pointed::Ground(Some(place)) => {
                let on_map = Plate::MAP.pixel(place);
                let (x, y) = match &drawn.globe {
                    None => on_map,
                    Some(globe) => match globe.planet.on_globe(place, globe.side, globe.drift) {
                        OnGlobe::Ground { x, y } => (x, y),
                        OnGlobe::IceCap | OnGlobe::SouthIce | OnGlobe::Iceberg(_) => on_map,
                        OnGlobe::Aurora => return Some(Found::Air(hover::Nature::Aurora)),
                        OnGlobe::Clouds => return Some(Found::Air(hover::Nature::Clouds)),
                        OnGlobe::OpenSea => return None,
                    },
                };
                let line = grown
                    .polar
                    .line_at(&grown.ground, x, y)
                    .unwrap_or_else(|| land::line_at(&grown.ground, x, y));
                Found::Ground(line)
            }
            _ => return None,
        };
        Some(found)
    }

    /// What the picture before the pointer's line shows, while the screen places the world's
    /// pictures and the line is the world's. A dot's line opens with its own mark instead.
    pub(super) fn line_icon(&self) -> Option<LineIcon> {
        let drawn = self.drawn_here()?;
        let grown = drawn.world.as_deref()?;
        match self.found(&drawn)? {
            Found::Nature(at) => {
                let nature = &grown.nature;
                match nature.features.get(at) {
                    Some(feature) => Some(LineIcon::Kind {
                        word: feature.kind.word(),
                        south: matches!(&feature.kind, Kind::Hurricane(storm) if storm.south),
                    }),
                    None => nature
                        .shoals
                        .get(at - nature.features.len())
                        .map(|_| LineIcon::kind("shoal")),
                }
            }
            Found::Wonder(at) => grown
                .wonders
                .get(at)
                .map(|placed| LineIcon::Wonder(placed.wonder)),
            Found::Ground(line) => Some(LineIcon::kind(ground_word(line.kind))),
            Found::Air(hover::Nature::Aurora) => Some(LineIcon::kind("aurora")),
            Found::Air(_) => Some(LineIcon::kind("clouds")),
        }
    }

    /// The picture before the pointer's line, where the last frame left room for one.
    pub fn line_picture(&self) -> Option<Picture> {
        self.line_pictures.get(self.line_shown.get()?)
    }
}

/// The word a kind of ground's picture goes by: snow and bare rock alike show as rock.
fn ground_word(kind: GroundKind) -> &'static str {
    match kind {
        GroundKind::Sea | GroundKind::OpenSea => "sea",
        GroundKind::Rock | GroundKind::Snow => "rock",
        GroundKind::Climate(climate) => climate.name(),
        GroundKind::Ice(IceKind::Cap) => "cap",
        GroundKind::Ice(IceKind::South) => "shelf",
        GroundKind::Ice(IceKind::Berg(_)) => "iceberg",
    }
}
