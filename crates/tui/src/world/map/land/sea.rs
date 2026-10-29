//! The sea's colours: a real sea, deep blue to the shallows, dark enough that the dots and the
//! land stand out. The shelf and shallows stay dark and a little green, as seas look from orbit.

use crate::world::map::fields::{Look, Rgb};

/// A thin beach where the land meets the sea, and the tundra by the polar ice.
pub const BEACH: Rgb = [206.0, 192.0, 150.0];
pub const TUNDRA: Rgb = [136.0, 138.0, 118.0];

/// One sea's colours, from the deep to the foam on its beaches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeaColours {
    pub abyss: Rgb,
    pub shelf: Rgb,
    pub shallow: Rgb,
    pub surf: Rgb,
    pub foam: Rgb,
}

impl SeaColours {
    /// The map's own sea.
    pub const MAP: SeaColours = SeaColours {
        abyss: [10.0, 22.0, 42.0],
        shelf: [14.0, 40.0, 68.0],
        shallow: [20.0, 64.0, 88.0],
        surf: [34.0, 92.0, 108.0],
        foam: [178.0, 220.0, 228.0],
    };

    /// The planet's sea for its look: a rocky world's, an ocean world's or a frozen one's.
    pub fn planet(look: Look) -> SeaColours {
        match look {
            Look::Ocean => SeaColours {
                abyss: hex(0x0a2246),
                shelf: hex(0x0e3d66),
                shallow: hex(0x16627a),
                surf: hex(0x2c8a96),
                foam: hex(0xc6ece8),
            },
            Look::Ice => SeaColours {
                abyss: hex(0x101d35),
                shelf: hex(0x1a304f),
                shallow: hex(0x284868),
                surf: hex(0x6a8aab),
                foam: hex(0xcfdceb),
            },
            Look::Rock | Look::Gas => SeaColours {
                abyss: hex(0x0a1c3a),
                shelf: hex(0x10345a),
                shallow: hex(0x185270),
                surf: hex(0x2c7288),
                foam: hex(0xb9dce6),
            },
        }
    }
}

const fn hex(rgb: u32) -> Rgb {
    [
        ((rgb >> 16) & 0xff) as f64,
        ((rgb >> 8) & 0xff) as f64,
        (rgb & 0xff) as f64,
    ]
}
