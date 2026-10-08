//! What `/prompts` lists: for every crew role, its frame, who writes its instructions with
//! your file for the role, and each built-in prompt of its steps.

use serde::{Deserialize, Serialize};

/// The mark on a template you changed.
pub const CHANGED: &str = "✎ changed";

/// The part every role has beside its frame: what its agents are asked to do.
pub const INSTRUCTIONS: &str = "instructions";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptList {
    pub rows: Vec<PromptRow>,
    /// Who may write a role's instructions: the orchestrator, Argonauts' own templates, then
    /// each agent on the crew.
    #[serde(default)]
    pub writers: Vec<WriterChoice>,
    /// What you should know about your own files, a line each: one that can't be used, one
    /// that tells a reviewer who made the work, and the ones that wait for your trust.
    #[serde(default)]
    pub notes: Vec<String>,
    /// What the last key did, such as `Wrote prompts/frames/reviewers.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// Sent after a copy or a reset for the list already open, so nothing opens for it once
    /// that list has closed.
    #[serde(default)]
    pub refresh: bool,
}

/// One line of `/prompts`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRow {
    /// The crew role whose agents read it, such as `reviewers`.
    pub role: String,
    /// `frame`, `instructions`, or what a step's prompt is for, such as `review a change`.
    pub part: String,
    /// `built in`, `the orchestrator`, or the model you named.
    pub written_by: String,
    /// Your file for it, or the mark that you changed it. Empty for the built-in as it is.
    #[serde(default)]
    pub yours: String,
    /// The template's name under `prompts/`, such as `frames/reviewers`. `None` for a role's
    /// instructions, which you add to with a file under `roles/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether a file of yours stands where the built-in would, so there is one to compare
    /// or delete.
    #[serde(default)]
    pub has_file: bool,
    /// The text in use, so showing it costs no round trip.
    #[serde(default)]
    pub text: String,
}

/// One answer to "who writes this role's instructions".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterChoice {
    /// As the setting takes it: `orchestrator`, `built-in`, or a model such as
    /// `codex/gpt-5.6`.
    pub value: String,
    /// As the screen says it: `the orchestrator`, `built in`, `GPT 5.6 on Codex`.
    pub shown: String,
}

impl PromptList {
    /// The role named on a row: on its first line only, as the table shows it.
    pub fn role_shown(&self, index: usize) -> &str {
        let Some(row) = self.rows.get(index) else {
            return "";
        };
        let repeated = index > 0 && self.rows[index - 1].role == row.role;
        if repeated { "" } else { &row.role }
    }

    /// The rows as plain text, columns padded to their widest cell, for a screen with no
    /// list to move in.
    pub fn table(&self) -> String {
        let head = [
            "role".to_owned(),
            "part".to_owned(),
            "written by".to_owned(),
            "yours".to_owned(),
            "name".to_owned(),
        ];
        let cells = |(index, row): (usize, &PromptRow)| {
            [
                self.role_shown(index).to_owned(),
                row.part.clone(),
                row.written_by.clone(),
                row.yours.clone(),
                row.name.clone().unwrap_or_default(),
            ]
        };
        let all: Vec<[String; 5]> = std::iter::once(head)
            .chain(self.rows.iter().enumerate().map(cells))
            .collect();
        let widths: Vec<usize> = (0..5)
            .map(|column| {
                let cells = all.iter().map(|cells| cells[column].chars().count());
                cells.max().unwrap_or(0)
            })
            .collect();
        all.iter()
            .map(|cells| {
                let line: Vec<String> = cells
                    .iter()
                    .zip(&widths)
                    .map(|(cell, &width)| format!("{cell:<width$}"))
                    .collect();
                format!("  {}", line.join("  ")).trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, part: &str, written_by: &str, yours: &str, name: Option<&str>) -> PromptRow {
        PromptRow {
            role: role.to_owned(),
            part: part.to_owned(),
            written_by: written_by.to_owned(),
            yours: yours.to_owned(),
            name: name.map(str::to_owned),
            ..PromptRow::default()
        }
    }

    /// In the table a role is named once, on its first line.
    #[test]
    fn the_table_names_a_role_on_its_first_line() {
        let list = PromptList {
            rows: vec![
                row("workers", "frame", "built in", "", Some("frames/workers")),
                row(
                    "workers",
                    INSTRUCTIONS,
                    "the orchestrator",
                    "roles/workers.md",
                    None,
                ),
                row(
                    "reviewers",
                    "frame",
                    "built in",
                    CHANGED,
                    Some("frames/reviewers"),
                ),
            ],
            ..PromptList::default()
        };
        let lines: Vec<String> = list.table().lines().map(str::to_owned).collect();
        assert_eq!(
            lines,
            [
                "  role       part          written by        yours             name",
                "  workers    frame         built in                            frames/workers",
                "             instructions  the orchestrator  roles/workers.md",
                "  reviewers  frame         built in          ✎ changed         frames/reviewers",
            ]
        );
        assert_eq!(list.role_shown(1), "");
        assert_eq!(list.role_shown(2), "reviewers");
    }
}
