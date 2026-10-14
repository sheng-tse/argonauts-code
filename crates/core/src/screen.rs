//! A screen that joins a CLI's session to draw what the CLI draws for its own, as Argonauts
//! joins Claude Code's main session for You should know. The CLI sends a component's tree; the
//! adapter reads it into rows of text and buttons, and a press goes back with the button's own
//! handle, so a button Argonauts has never seen works without a change.

use serde::{Deserialize, Serialize};

/// What a screen asks of the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum ScreenRequest {
    /// Join the session, so the CLI's mods draw for this screen.
    Attach,
    /// What's drawn above the input now.
    Render,
    /// Press a button the last drawing carried.
    Press { plugin: String, handle: u64 },
}

/// What came back from the session about the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum ScreenChange {
    /// The session took the screen, or `ok: false`, turned it down.
    Attached { ok: bool },
    /// Something drawn changed, so it's asked for again.
    Stale,
    /// What's above the input now. Empty when nothing is.
    Drawn { drawn: Drawn },
    /// A reply that doesn't read, so this session's screen stops.
    Unreadable { why: String },
}

/// A component's tree as rows of styled text and the buttons under them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drawn {
    pub rows: Vec<DrawnRow>,
    pub buttons: Vec<DrawnButton>,
    /// Element types with no text that were left out, for `/doctor`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_out: Vec<String>,
}

impl Drawn {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.buttons.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawnRow {
    pub spans: Vec<DrawnSpan>,
}

impl DrawnRow {
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawnSpan {
    pub text: String,
    pub look: DrawnLook,
}

/// The few looks a mod's text takes, in the screen's own colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawnLook {
    #[default]
    Plain,
    Dim,
    Bold,
    /// The suggestion color, as Claude Code draws You should know's star.
    Accent,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawnButton {
    pub label: String,
    /// What a press sends back.
    pub plugin: String,
    pub handle: u64,
}
