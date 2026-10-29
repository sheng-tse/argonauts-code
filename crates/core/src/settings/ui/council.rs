//! How Council's talk is drawn, `ui.council_view` and `ui.bubbles`. Council's screen reads both.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How Council's talk shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CouncilView {
    /// Each message in a bubble, the council's on the left with the writer's logo and yours on
    /// the right, as a group conversation reads in Messages.
    #[default]
    Messages,
    /// One block per message down the page, each under its writer's name.
    Transcript,
}

/// How the bubbles of `messages` are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Bubbles {
    /// Rounded bubbles, drawn as pictures under the text where the terminal draws pictures
    /// there, so selecting and searching still show on the words. Other terminals show the
    /// transcript.
    #[default]
    Auto,
    /// Bubbles of block cells with square ends, in every terminal.
    Blocks,
    /// Messages in their places with nothing filled in behind them.
    Plain,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Ui {
        council_view: CouncilView,
        bubbles: Bubbles,
    }

    #[test]
    fn each_view_and_bubble_reads_its_word() {
        let ui: Ui = toml::from_str("council_view = \"transcript\"\nbubbles = \"blocks\"").unwrap();
        assert_eq!(ui.council_view, CouncilView::Transcript);
        assert_eq!(ui.bubbles, Bubbles::Blocks);
        let ui: Ui = toml::from_str("council_view = \"messages\"\nbubbles = \"plain\"").unwrap();
        assert_eq!(
            (ui.council_view, ui.bubbles),
            (CouncilView::Messages, Bubbles::Plain)
        );
        assert!(toml::from_str::<Ui>("council_view = \"bubbles\"\nbubbles = \"auto\"").is_err());
    }
}
