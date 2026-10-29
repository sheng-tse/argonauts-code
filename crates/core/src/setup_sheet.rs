//! A workflow's setup sheet: what a workflow's command opens in place of the input when the
//! workflow isn't ready. The app builds it from the workflow's own steps, or Council's seats,
//! and builds it again after each change and each time it reads the logins; the screen draws
//! it, moves the pointer, and sends back what each key changes. Nothing is saved until Enter,
//! which saves everything in one write; Esc saves nothing.

use serde::{Deserialize, Serialize};

use crate::logos::Logo;

/// One sheet as the screen draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSheet {
    /// The workflow Enter switches to, such as `council`.
    pub workflow: String,
    /// `Set up Council`.
    pub title: String,
    /// What the workflow does, a line each.
    pub about: Vec<String>,
    /// What changed or what it waits for, under the lines above, such as "Codex is picked to
    /// plan. Enter saves it everywhere and switches."
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<Words>,
    pub layout: SheetLayout,
    pub rows: Vec<SheetRow>,
    /// The dim lines under the rows, each as it prints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The keys' line at the bottom.
    pub keys: String,
    /// The row the pointer starts on: what needs you. `None` keeps it where it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<Pointer>,
    /// Enter saves and switches. Until then it says why it can't.
    pub ready: bool,
    /// Why Enter can't switch yet, as the notice says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_yet: Option<String>,
    /// The same sheet again, for one already open: after a change, or a login read again. A
    /// sheet that closed meanwhile doesn't open for it.
    #[serde(default)]
    pub refresh: bool,
}

/// Where the pointer is: a row, and which of its columns ← and → change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pointer {
    pub row: String,
    pub column: SheetColumn,
}

/// How the columns sit: a workflow's steps, each with a role word and who fills it, or
/// Council's seats, each with who sits and the seat's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetLayout {
    Steps,
    Seats,
}

/// A column the pointer can change on a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetColumn {
    /// Who fills it, or a way's value.
    #[default]
    Who,
    /// A seat's role.
    Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "snake_case")]
pub enum SheetRow {
    Fill(FillRow),
    Aside(AsideRow),
    /// The first line of a block, such as "How tasks are done", with what it means beside it.
    Heading {
        label: String,
        text: String,
    },
    Way(WayRow),
    Blank,
}

/// A step or a seat: its label, its role, who fills it, what it does, and how it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillRow {
    /// What a change names; a row without one is never pointed at.
    pub id: String,
    /// `Plan`, `Seat`, `Checks`.
    pub label: String,
    /// On steps the role word beside the label; on seats the seat's role.
    pub role: Words,
    pub who: Vec<Part>,
    /// The tone the names take.
    #[serde(default)]
    pub who_tone: Tone,
    pub what: Vec<Words>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Words>,
    /// The columns ← and → change, the first one first; tab goes to the next.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<SheetColumn>,
    /// Pointed at, the names take the pointer's colour, as a choice that stands for a rule
    /// rather than a model does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lit: bool,
    /// Lines shown under it only while the pointer is on it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub under: Vec<UnderLine>,
}

/// A line under a pointed row: a dim label in a column of its own, then names or words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnderLine {
    /// `Can pick`, or empty for a line of words alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    pub parts: Vec<Part>,
}

/// A line under the seats or steps that fills nothing: the Add one line, or an agent that
/// can't sit yet and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsideRow {
    /// Pointed at, space acts on it, as Add one adds a seat. `None` is only read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `None` draws the open circle of something to add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<Logo>,
    pub name: String,
    pub parts: Vec<Part>,
}

/// One way a task may be done, allowed or not, as How tasks are done lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WayRow {
    pub id: String,
    pub allowed: bool,
    pub name: String,
    /// `Any`, `Up to 4`: what ← and → change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// A workflow of a list, set in under the Workflows line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inner: bool,
}

/// Names and words in a row, each logo before the name it serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "part", rename_all = "snake_case")]
pub enum Part {
    /// A model or CLI by its logo and name. `number` tells two seats of one model apart.
    Named {
        logo: Logo,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        number: Option<u8>,
    },
    Text {
        text: String,
    },
}

impl Part {
    pub fn named(logo: Logo, name: impl Into<String>) -> Self {
        Self::Named {
            logo,
            name: name.into(),
            number: None,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// Words in one tone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Words {
    pub text: String,
    #[serde(default)]
    pub tone: Tone,
}

impl Words {
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Text)
    }

    pub fn dim(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Dim)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Text,
    Dim,
    /// Ready, picked, allowed.
    Ok,
    /// Same maker: it works, with a caveat.
    Warn,
}

/// What a key on the sheet asks the app to change. The app answers with the sheet again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SheetAction {
    /// ← or →: the next or the one before in the row's column.
    Change {
        row: String,
        column: SheetColumn,
        forward: bool,
    },
    /// Space: adds or drops a seat, allows a way or keeps it out.
    Toggle { row: String },
    /// Enter, or ctrl+s for this session only: everything in one write, then the switch.
    Save { session: bool },
    /// Esc: nothing is saved, and the logins stop being read.
    Close,
}

/// Where a workflow stands, as the `/` menu says it at the right of the workflow's command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    /// Your requests go to it now.
    InUse,
    /// Its command switches at once.
    Ready,
    /// Its command opens its setup sheet first.
    SetUpFirst,
}

impl WorkflowStatus {
    pub fn words(self) -> &'static str {
        match self {
            Self::InUse => "in use",
            Self::Ready => "ready",
            Self::SetUpFirst => "set up first",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sheet_reads_back_as_it_was_written() {
        let sheet = SetupSheet {
            workflow: "council".into(),
            title: "Set up Council".into(),
            about: vec!["Each seat answers alone first.".into()],
            status: Vec::new(),
            layout: SheetLayout::Seats,
            rows: vec![
                SheetRow::Fill(FillRow {
                    id: "seat-1".into(),
                    label: "Seat".into(),
                    role: Words::plain("Skeptic"),
                    who: vec![Part::Named {
                        logo: Logo::dot("claude-code"),
                        name: "Opus 5.5 · 2".into(),
                        number: Some(2),
                    }],
                    who_tone: Tone::Text,
                    what: vec![Words::dim("Argues against where most seats lean")],
                    state: Some(Words::new("Ready", Tone::Ok)),
                    columns: vec![SheetColumn::Who, SheetColumn::Role],
                    lit: false,
                    under: Vec::new(),
                }),
                SheetRow::Blank,
                SheetRow::Way(WayRow {
                    id: "ways.agents".into(),
                    allowed: true,
                    name: "Agents".into(),
                    value: Some("Up to 4".into()),
                    hint: None,
                    inner: false,
                }),
            ],
            notes: vec!["The note keeper picks a way for each task.".into()],
            keys: "esc to leave it".into(),
            pointer: Some(Pointer {
                row: "seat-1".into(),
                column: SheetColumn::Role,
            }),
            ready: true,
            not_yet: None,
            refresh: false,
        };
        let text = serde_json::to_string(&sheet).unwrap();
        assert_eq!(serde_json::from_str::<SetupSheet>(&text).unwrap(), sheet);
        let action = SheetAction::Change {
            row: "seat-1".into(),
            column: SheetColumn::Role,
            forward: true,
        };
        let text = serde_json::to_string(&action).unwrap();
        assert_eq!(serde_json::from_str::<SheetAction>(&text).unwrap(), action);
    }
}
