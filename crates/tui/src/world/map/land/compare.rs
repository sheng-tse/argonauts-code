//! A compared group's tint on the land: red where the group does a behavior more than the
//! rest, blue where it does it less, and the rest of the land a calm grey.

use crate::world::map::fields::{COARSE_HEIGHT, COARSE_WIDTH, Ground};
use crate::world::raster::Grid;
use crate::world::raster::noise::smooth;

/// How much more or less a group does a behavior than the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Leaning {
    MuchLess,
    Less,
    Same,
    More,
    MuchMore,
}

impl Leaning {
    /// As a dot's line says it.
    pub fn words(self) -> &'static str {
        match self {
            Self::MuchLess => "much less here",
            Self::Less => "less here",
            Self::Same => "about the same here",
            Self::More => "more here",
            Self::MuchMore => "much more here",
        }
    }

    /// How strongly the land under it is tinted.
    fn strength(self) -> f64 {
        match self {
            Self::MuchMore => 0.55,
            Self::More => 0.32,
            Self::Same => 0.0,
            Self::Less => 0.28,
            Self::MuchLess => 0.48,
        }
    }
}

/// Each behavior's leaning for one compared group, `None` where it has too few actions to say.
#[derive(Debug, Clone, PartialEq)]
pub struct Compared {
    pub group: usize,
    pub leaning: Vec<Option<Leaning>>,
}

impl Compared {
    /// The group against the rest, or `None` when either side has fewer than 5 actions. A
    /// behavior leans when the group has 3 or more of its actions or would by the others' rate;
    /// twice the rate is much more, a quarter more is more.
    pub fn new(ground: &Ground, group: usize) -> Option<Self> {
        let mine_all = f64::from(*ground.group_count.get(group)?);
        let others_all = ground.facts.dots.len() as f64 - mine_all;
        if mine_all < 5.0 || others_all < 5.0 {
            return None;
        }
        let leaning = ground
            .count
            .iter()
            .zip(&ground.by_group)
            .map(|(count, by_group)| {
                if *count == 0 {
                    return None;
                }
                let mine = f64::from(by_group[group]);
                let others = f64::from(*count) - mine;
                if !(mine >= 3.0 || mine_all * others / others_all >= 3.0) {
                    return None;
                }
                let lean = ((mine / mine_all) / ((others + 1.0) / others_all)).log2();
                Some(if lean >= 1.0 {
                    Leaning::MuchMore
                } else if lean >= 0.321928 {
                    Leaning::More
                } else if lean > -0.321928 {
                    Leaning::Same
                } else if lean > -1.0 {
                    Leaning::Less
                } else {
                    Leaning::MuchLess
                })
            })
            .collect();
        Some(Self { group, leaning })
    }
}

/// The tint over the land for a comparison, on the coarse grid: above 0 red, below blue.
#[derive(Debug, Clone)]
pub struct Tint {
    pub compared: Compared,
    pub field: Grid,
}

impl Tint {
    pub fn new(ground: &Ground, compared: Compared) -> Self {
        let field = Grid::from_fn(COARSE_WIDTH, COARSE_HEIGHT, |k, j| {
            let gx = k as f64 + ground.warp_x.get(k, j);
            let gy = j as f64 + ground.warp_y.get(k, j);
            let height = ground.peaks.sample(gx, gy);
            if height < 0.05 {
                return 0.0;
            }
            let (mut red, mut blue) = (0.0, 0.0);
            for (mountain, leaning) in ground.mountains.iter().zip(&compared.leaning) {
                let (Some(mountain), Some(leaning)) = (mountain, leaning) else {
                    continue;
                };
                if *leaning == Leaning::Same {
                    continue;
                }
                let own = mountain.sample(gx, gy);
                let amount =
                    leaning.strength() * smooth(0.25, 0.7, own / height) * smooth(0.15, 1.6, own);
                if *leaning > Leaning::Same {
                    red = 1.0 - (1.0 - red) * (1.0 - amount);
                } else {
                    blue = 1.0 - (1.0 - blue) * (1.0 - amount);
                }
            }
            red - blue
        });
        Self { compared, field }
    }
}
