//! What a card's Show the diff, Show the details, or Show the file opens. The card stays, and
//! nothing is sent but a request for the diff.

use std::io::Read;
use std::path::Path;

use argonauts_core::api::DiffScope;
use argonauts_core::app::{AppChoice, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::checkpoint::{
    CheckpointDecided, CheckpointKind, CheckpointOpened, CheckpointPacket, PolicyCheck,
};
use argonauts_core::{CardId, RunId};
use time::UtcOffset;

use super::about::details;
use super::records::{by_id, outcome};
use super::words;
use crate::text::sanitize_line;
use crate::views::pager::Pager;

/// Pager text is laid out this wide; the pager cuts or scrolls it.
const PAGER_WIDTH: usize = 120;

/// The most of a combined diff file the pager reads, as the controller cuts a card's diff.
const MAX_DIFF_BYTES: u64 = 2 * 1024 * 1024;

pub enum Shown {
    Request(ViewRequest),
    Pager(Pager),
    /// A line for the mode line's notice slot, such as why a diff couldn't be read.
    Notice(String),
}

pub fn checkpoint_view(
    run: &RunId,
    opened: &CheckpointOpened,
    offset: UtcOffset,
    names: &DisplayNames,
) -> Shown {
    let diff_kind = matches!(
        opened.kind,
        CheckpointKind::Merge
            | CheckpointKind::Handoff
            | CheckpointKind::TakeOverReturn
            | CheckpointKind::OriginalReturned
    );
    if !diff_kind {
        return Shown::Pager(Pager::text(
            opened.title.clone(),
            details(opened, offset, names).plain(PAGER_WIDTH),
        ));
    }
    if let Some(card) = diff_card(opened) {
        return Shown::Request(ViewRequest::Diff {
            run: run.clone(),
            card,
            scope: DiffScope::Card,
        });
    }
    match &opened.packet {
        CheckpointPacket::Merge(merge) => match read_diff(&merge.diff_path) {
            Ok(lines) => Shown::Pager(Pager::text(format!("Diff · {}", merge.branch), lines)),
            Err(error) => Shown::Notice(error),
        },
        _ => Shown::Notice("No diff for this task yet".to_owned()),
    }
}

/// The auto decision a spot check asks about, with the reasons it was made.
pub fn evidence_view(
    run: &RunId,
    opened: &CheckpointOpened,
    decided: Option<&CheckpointDecided>,
    check: Option<&PolicyCheck>,
    offset: UtcOffset,
    names: &DisplayNames,
) -> Shown {
    let mut lines = Vec::new();
    if let Some(decided) = decided {
        lines.push(format!(
            "{} · decided by {}",
            outcome(opened, decided, Some(run), names, &by_id),
            words::party(&decided.by, names)
        ));
    }
    if let Some(check) = check {
        for reason in &check.reasons {
            lines.push(format!("  {}", sanitize_line(reason).trim()));
        }
        for violation in &check.violations {
            lines.push(format!("  {}", sanitize_line(&violation.detail).trim()));
        }
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(details(opened, offset, names).plain(PAGER_WIDTH));
    Shown::Pager(Pager::text(
        format!("Evidence · {}", opened.title.trim()),
        lines,
    ))
}

/// A choice's whole body, such as every safety key a project's settings set.
pub fn choice_view(choice: &AppChoice) -> Shown {
    let lines = choice
        .body
        .iter()
        .map(|line| sanitize_line(line).into_owned())
        .collect();
    Shown::Pager(Pager::text(choice.title.clone(), lines))
}

/// The one card a diff is about, when there is one.
fn diff_card(opened: &CheckpointOpened) -> Option<CardId> {
    match &opened.packet {
        CheckpointPacket::Handoff(plan) => Some(plan.card.clone()),
        CheckpointPacket::TakeOverReturn(back) => Some(back.card.clone()),
        CheckpointPacket::OriginalReturned(back) => Some(back.card.clone()),
        CheckpointPacket::Merge(merge) if opened.card.is_none() && merge.rows.len() == 1 => {
            Some(merge.rows[0].card.clone())
        }
        _ => opened.card.clone(),
    }
}

/// A crew merge's combined diff is one file for the whole run, with no card to ask for.
fn read_diff(path: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("Can't open the diff at {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_DIFF_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Can't read the diff at {}: {error}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(|line| sanitize_line(line).into_owned())
        .collect())
}
