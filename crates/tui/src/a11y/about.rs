//! What a card is about, read from the item the screen draws above it rather than from the
//! drawn text: a table reads a row at a time as `header: value` pairs, so a blank cell or a
//! column with no header never moves a value under the wrong header.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::checkpoint::CheckpointOpened;
use time::UtcOffset;

use crate::cards::{AboutCell, Block, Table, about_item, details};

use super::decisions::Decision;

/// Clock times in these read at `offset`, the terminal's zone.
pub fn about_lines(
    opened: &CheckpointOpened,
    offset: UtcOffset,
    names: &DisplayNames,
) -> Vec<String> {
    about_item(opened, offset, names).map_or_else(Vec::new, |item| item_lines(&item))
}

/// Everything the card is about, as Show the details reads it.
pub fn details_lines(
    opened: &CheckpointOpened,
    offset: UtcOffset,
    names: &DisplayNames,
) -> Vec<String> {
    item_lines(&details(opened, offset, names))
}

/// Why an auto decision was made, then what it was about.
pub fn evidence_lines(decision: &Decision, offset: UtcOffset, names: &DisplayNames) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(check) = &decision.check {
        lines.extend(check.reasons.iter().cloned());
        lines.extend(
            check
                .violations
                .iter()
                .map(|violation| violation.detail.clone()),
        );
    }
    lines.extend(details_lines(&decision.opened, offset, names));
    lines
}

fn item_lines(item: &AboutCell) -> Vec<String> {
    let mut lines = vec![item.heading.clone()];
    for block in &item.blocks {
        match block {
            Block::Line { text, .. } => lines.extend(text.lines().map(str::to_owned)),
            Block::Labeled { label, text } => lines.push(format!("{}: {}", label.trim(), text)),
            Block::Table(table) => lines.extend(table_lines(table)),
            Block::Blank => {}
        }
    }
    lines
}

/// A cell under a column with no header is read as it is, and a blank cell not at all.
fn table_lines(table: &Table) -> Vec<String> {
    table
        .labeled_rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .filter(|(_, value)| !value.trim().is_empty())
                .map(|(header, value)| match header.trim() {
                    "" => value.trim().to_owned(),
                    header => format!("{header}: {}", value.trim()),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|line| !line.is_empty())
        .collect()
}
