//! The diff views: a card against its base, or its last session's changes.

use argonauts_core::CardId;
use argonauts_core::api::{DiffScope, DiffView};
use argonauts_core::layout;
use argonauts_core::paths::{create_private_dir_all, write_private_file};

use crate::card::support::record_of;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::integrate::branch::card_branch;

/// The view's text is cut here; the whole diff is at its path.
const VIEW_BYTES: usize = 2 * 1024 * 1024;

pub async fn card_diff(host: &Host, card: &CardId, scope: DiffScope) -> Result<DiffView, String> {
    diff_view(host, card, scope)
        .await
        .map_err(|err| err.to_string())
}

async fn diff_view(host: &Host, card: &CardId, scope: DiffScope) -> EngineResult<DiffView> {
    let record = record_of(host, card)?;
    let git = host.git();
    let root = &host.repo().root;
    let branch = record
        .branch
        .clone()
        .unwrap_or_else(|| card_branch(host.run_id(), card));
    let not_started = || EngineError::Refused(format!("{card} has no work yet"));
    let head = git
        .branch_head(root, &branch)
        .await?
        .ok_or_else(not_started)?;
    let (from, to, name) = match scope {
        DiffScope::Card => {
            let base = record.base.clone().ok_or_else(not_started)?;
            (base, head, "card.diff")
        }
        DiffScope::LastTurn => {
            let last = record
                .commits
                .iter()
                .rev()
                .find_map(|commit| commit.sha.clone())
                .ok_or_else(not_started)?;
            let parent = git
                .parents(root, &last)
                .await?
                .into_iter()
                .next()
                .ok_or_else(not_started)?;
            (parent, last, "last-turn.diff")
        }
    };
    let text = git.diff(root, &from, &to).await?;
    let stat = git.diff_stat(root, &from, &to).await?;
    let dir = layout::card_dir(&host.repo().run_dir, card).join("diffs");
    create_private_dir_all(&dir)?;
    let path = dir.join(name);
    write_private_file(&path, text.as_bytes())?;
    Ok(DiffView {
        card: card.clone(),
        stat,
        text: cut(text, VIEW_BYTES),
        path,
    })
}

/// At most `max` bytes, cut at a line end where there is one, and never inside a character.
pub(crate) fn cut(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').map_or(end, |newline| newline + 1);
    text.truncate(end);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_diff_is_cut_at_a_line_end() {
        assert_eq!(cut("short\n".into(), 100), "short\n");
        assert_eq!(cut("one\ntwo\nthree\n".into(), 10), "one\ntwo\n");
        assert_eq!(cut("ééé".into(), 3), "é");
    }
}
