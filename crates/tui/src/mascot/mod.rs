//! The mode mascots the welcome header draws beside its lines: the argonaut for Captain, the
//! medusa for Orchestrator and the hydra for Council. Each is a picture where the terminal draws
//! one and block cells elsewhere, amber, or in its CLI's colour while that CLI's own ultra mode
//! is on. `assets/mascots/` holds the SVGs, the generator that draws them, the pictures and the
//! cells.

mod blocks;
pub mod graphics;
mod pictures;
mod png;
mod send;
#[cfg(test)]
mod terminal_golden;

use std::time::Duration;

use argonauts_core::app::UltraColour;

pub use blocks::rows;
pub use graphics::Graphics;
pub use pictures::picture;
pub use send::Sent;

/// A mascot fills 9 cells across and 3 down.
pub const WIDTH: usize = 9;
pub const ROWS: usize = 3;
/// The frames of one cycle of an ultra look's animation.
pub const FRAMES: usize = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mascot {
    /// An argonaut, the real octopus named after Jason's crew, with one arm raised the way
    /// Aristotle believed it sailed the sea: it steers itself and does the work itself, as the
    /// captain does.
    Argonaut,
    /// A medusa, as a jellyfish is called after the Gorgon: one beat of its bell sets every
    /// tentacle stroking in time, as the orchestrator hands out the parts and keeps the crew in
    /// step.
    Medusa,
    /// The hydra that Heracles, one of the Argonauts, fought, with three heads arguing over one
    /// body: one strong, one overwhelmed, and one goofy, like whichever model everyone is teasing
    /// that day, so the other two catch its mistakes.
    Hydra,
}

impl Mascot {
    pub const ALL: [Mascot; 3] = [Mascot::Argonaut, Mascot::Medusa, Mascot::Hydra];

    /// The mascot of the workflow in effect. A workflow of your own takes the captain's.
    pub fn for_workflow(workflow: &str) -> Self {
        match workflow.trim().to_ascii_lowercase().as_str() {
            "orchestrator" => Mascot::Medusa,
            "council" => Mascot::Hydra,
            _ => Mascot::Argonaut,
        }
    }

    /// Its name in the assets.
    fn name(self) -> &'static str {
        match self {
            Mascot::Argonaut => "a-captain-argonaut",
            Mascot::Medusa => "a-orchestrator-medusa",
            Mascot::Hydra => "a-council-hydra",
        }
    }

    /// Who it is, as its SVG says at the top.
    pub fn story(self) -> &'static str {
        &blocks::file().mascots[self.name()].story
    }

    /// How long each frame of an ultra look shows.
    pub fn frame_time(self) -> Duration {
        Duration::from_secs_f64(pictures::cycle(self) / FRAMES as f64)
    }
}

/// The colours a mascot is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Look {
    /// Amber.
    Normal,
    /// A darker amber that reads on a light terminal.
    Light,
    /// Claude Code's ultracode purple, animated.
    Ultracode,
    /// Codex's ultra-effort blue, animated.
    Codex,
}

impl Look {
    pub const ALL: [Look; 4] = [Look::Normal, Look::Light, Look::Ultracode, Look::Codex];

    /// The look while the main agent's own ultra mode is `ultra`, on a light terminal or not.
    /// The ultra looks keep their colours on a light terminal.
    pub fn new(ultra: Option<UltraColour>, light: bool) -> Self {
        match ultra {
            Some(UltraColour::Purple) => Look::Ultracode,
            Some(UltraColour::Blue) => Look::Codex,
            None if light => Look::Light,
            None => Look::Normal,
        }
    }

    /// Whether it animates, with the gold mark of the ultra looks.
    pub fn is_ultra(self) -> bool {
        matches!(self, Look::Ultracode | Look::Codex)
    }

    fn name(self) -> &'static str {
        match self {
            Look::Normal => "normal",
            Look::Light => "light",
            Look::Ultracode => "ultracode",
            Look::Codex => "codex",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_workflow_has_its_mascot() {
        assert_eq!(Mascot::for_workflow("captain"), Mascot::Argonaut);
        assert_eq!(Mascot::for_workflow("orchestrator"), Mascot::Medusa);
        assert_eq!(Mascot::for_workflow("Council"), Mascot::Hydra);
        assert_eq!(Mascot::for_workflow("my-review-loop"), Mascot::Argonaut);
    }

    #[test]
    fn each_mascot_says_who_it_is_as_its_svg_does() {
        let svgs = [
            include_str!("../../assets/mascots/svg/a-captain-argonaut.svg"),
            include_str!("../../assets/mascots/svg/a-orchestrator-medusa.svg"),
            include_str!("../../assets/mascots/svg/a-council-hydra.svg"),
        ];
        for (mascot, svg) in Mascot::ALL.into_iter().zip(svgs) {
            let first = svg.lines().nth(1).unwrap_or_default();
            assert_eq!(
                first,
                format!("  <!-- {} -->", mascot.story()),
                "{mascot:?}"
            );
        }
        assert!(Mascot::Hydra.story().starts_with("The hydra that Heracles"));
    }

    #[test]
    fn only_an_ultra_mode_changes_the_colour_and_light_terminals_get_the_darker_amber() {
        assert_eq!(Look::new(None, false), Look::Normal);
        assert_eq!(Look::new(None, true), Look::Light);
        assert_eq!(Look::new(Some(UltraColour::Purple), true), Look::Ultracode);
        assert_eq!(Look::new(Some(UltraColour::Blue), false), Look::Codex);
        assert!(Look::Codex.is_ultra() && !Look::Light.is_ultra());
    }

    #[test]
    fn each_frame_shows_an_eighteenth_of_its_mascots_cycle() {
        let seconds = |mascot: Mascot| mascot.frame_time().as_secs_f64();
        assert!((seconds(Mascot::Medusa) - 0.1).abs() < 1e-6);
        assert!((seconds(Mascot::Hydra) - 0.2).abs() < 1e-6);
    }
}
