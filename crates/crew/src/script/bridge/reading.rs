//! What a reviewer or a judge of a script's work is handed, and on which model. A reader with
//! tools gets the change in its prompt up to a cap and reads the rest in its folder. A model
//! served without tools, as Ollama's are, reads the whole change and the checks in its prompt,
//! once its window is known to hold them; when it doesn't, a reviewer with tools takes that one
//! review, and the model keeps the ones it can hold.

use argonauts_core::names::ModelRef;
use argonauts_core::registry::{ModelAccess, ModelEntry};

use crate::engine::host::Host;

/// As much of each task's change as a reader with tools is handed in its prompt. It reads the
/// rest in the task's worktree.
pub(super) const DIFF_SHOWN: usize = 60_000;

/// Kept free in a window for the answer and for what the session adds to the prompt, such as
/// the answer's schema. A small window keeps a quarter.
const ANSWER_ROOM: u64 = 8_000;

/// Assumed of a model its server sizes as it loads, while it isn't loaded: the least a local
/// server gives a model with no cap of its own (Ollama's 4,096 on a machine with little
/// memory), so a prompt never overflows the model that reads it.
const LEAST_WINDOW: u64 = 4_096;

/// The prompt's bytes a token stands for, at the low end: code and diffs run about three
/// characters a token, and a CJK character, three bytes long, is about one token.
const BYTES_PER_TOKEN: u64 = 3;

/// Of a window, the share in percent a count by bytes may fill, since it's an estimate and a
/// local server cuts a prompt that overruns from its start, instructions first.
const SURE_PERCENT: u64 = 90;

/// The work a reader judges: one task's change, or each task's under its name, as they will
/// merge into the run.
pub(in crate::script) struct Change {
    pieces: Vec<Piece>,
}

struct Piece {
    name: Option<String>,
    diff: String,
}

impl Change {
    pub(in crate::script) fn of_one(diff: String) -> Self {
        Self {
            pieces: vec![Piece { name: None, diff }],
        }
    }

    pub(in crate::script) fn of_tasks(changes: Vec<(String, String)>) -> Option<Self> {
        let pieces: Vec<Piece> = changes
            .into_iter()
            .map(|(name, diff)| Piece {
                name: Some(name),
                diff,
            })
            .collect();
        (!pieces.is_empty()).then_some(Self { pieces })
    }

    /// As a reader with tools gets it: each task's change up to [`DIFF_SHOWN`], and a line
    /// that sends it to its folder for the rest.
    pub(super) fn for_a_reader_with_tools(&self) -> String {
        self.joined(|diff| match cut_at(diff, DIFF_SHOWN) {
            None => diff.to_owned(),
            Some(kept) => format!(
                "{kept}\n… the change goes on. Read the rest in this folder, which is the task's \
                 worktree."
            ),
        })
    }

    /// As a reader without tools gets it: all of it, or the first `most` bytes and a line that
    /// says the change was cut there, since it has no folder to read the rest in.
    pub(super) fn for_a_reader_without_tools(&self, most: Option<usize>) -> String {
        let whole = self.joined(str::to_owned);
        match most.and_then(|most| cut_at(&whole, most)) {
            None => whole,
            Some(kept) => format!("{kept}\n… the change was cut here."),
        }
    }

    fn joined(&self, shown: impl Fn(&str) -> String) -> String {
        self.pieces
            .iter()
            .map(|piece| match &piece.name {
                Some(name) => format!("# {name}\n{}", shown(&piece.diff)),
                None => shown(&piece.diff),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// The text up to `most` bytes, at a character's edge, when it's longer.
fn cut_at(text: &str, most: usize) -> Option<&str> {
    if text.len() <= most {
        return None;
    }
    let mut cut = most;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(&text[..cut])
}

/// What a model's window can hold of a reader's prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Room {
    /// The whole prompt, with room left to answer.
    Holds,
    /// Only `most` bytes of the change, which is less than all of it. `why` says it after the
    /// model's name, as the reader who takes the review in its place is told.
    Short { most: usize, why: String },
}

/// The window a session on `worker` is held to, where anyone can say: its entry's, else, for
/// a model its server sizes as it loads, the window it's loaded with now, which asking loads
/// nothing to learn. `Err` while such a model isn't loaded.
async fn window_of(host: &Host, worker: &ModelRef) -> Result<Option<u64>, NotLoaded> {
    let registry = &host.services().registry;
    let entry = registry.get(worker);
    if let Some(window) = entry
        .as_ref()
        .and_then(ModelEntry::session_window)
        .filter(|window| *window > 0)
    {
        return Ok(Some(window));
    }
    let sized_as_it_loads = entry
        .as_ref()
        .is_some_and(|entry| entry.discovered && entry.access == ModelAccess::Local);
    if !sized_as_it_loads {
        return Ok(None);
    }
    match registry.loaded_window(worker).await {
        Some(window) => Ok(Some(window)),
        None => Err(NotLoaded),
    }
}

struct NotLoaded;

/// Whether a reader on `worker` holds the rest of its prompt, `brief` bytes long, beside the
/// whole `change`. A model whose window nobody knows, as a hosted endpoint's may be, is held
/// to nothing, as the endpoint's own check holds it.
pub(super) async fn room(host: &Host, worker: &ModelRef, brief: usize, change: &str) -> Room {
    let (window, not_loaded) = match window_of(host, worker).await {
        Ok(Some(window)) => (window, false),
        Ok(None) => return Room::Holds,
        Err(NotLoaded) => (LEAST_WINDOW, true),
    };
    let most = change_bytes_held(window, brief);
    if change.len() <= most {
        return Room::Holds;
    }
    let why = if not_loaded {
        "whose window isn't known until it loads".to_owned()
    } else {
        format!(
            "whose {} window can't hold this change",
            window_words(window)
        )
    };
    Room::Short { most, why }
}

/// How many bytes of change a `window` tokens wide holds beside `brief` bytes of the rest of
/// the prompt, with room left for the answer. Counted and cut in bytes alike.
fn change_bytes_held(window: u64, brief: usize) -> usize {
    let free = window.saturating_sub(ANSWER_ROOM.min(window / 4));
    let sure = free.saturating_mul(SURE_PERCENT) / 100;
    let held = sure.saturating_mul(BYTES_PER_TOKEN);
    usize::try_from(held)
        .unwrap_or(usize::MAX)
        .saturating_sub(brief)
}

/// A window as its model's tag says it, such as `64k` for 65,536 tokens.
fn window_words(window: u64) -> String {
    if window >= 1_024 && window.is_multiple_of(1_024) {
        format!("{}k", window / 1_024)
    } else if window >= 1_000 {
        format!("{}k", window / 1_000)
    } else {
        window.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_with_tools_reads_the_rest_in_its_folder() {
        let long = "+line\n".repeat(DIFF_SHOWN / 3);
        let shown = Change::of_one(long.clone()).for_a_reader_with_tools();
        assert!(shown.len() < long.len());
        assert!(shown.ends_with("Read the rest in this folder, which is the task's worktree."));
        let short = Change::of_one("+one\n".to_owned());
        assert_eq!(short.for_a_reader_with_tools(), "+one\n");
    }

    #[test]
    fn a_reader_without_tools_is_told_where_the_change_was_cut() {
        let change = Change::of_tasks(vec![
            ("cap".to_owned(), "+a\n".to_owned()),
            ("per-key".to_owned(), "+b\n".to_owned()),
        ])
        .unwrap();
        assert_eq!(
            change.for_a_reader_without_tools(None),
            "# cap\n+a\n\n\n# per-key\n+b\n"
        );
        let cut = change.for_a_reader_without_tools(Some(8));
        assert_eq!(cut, "# cap\n+a\n… the change was cut here.");
        assert!(!cut.contains("folder"));
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        assert_eq!(cut_at("ab−c", 3), Some("ab"));
        assert_eq!(cut_at("abc", 3), None);
    }

    #[test]
    fn a_window_holds_three_bytes_a_token_with_a_tenth_kept_spare() {
        // 64k less the answer's 8,000 is 57,536 tokens, nine tenths of that 51,782, three
        // bytes each 155,346, less the rest of the prompt.
        assert_eq!(change_bytes_held(65_536, 0), 155_346);
        assert_eq!(change_bytes_held(65_536, 1_346), 154_000);
        // A small window keeps a quarter for the answer.
        assert_eq!(change_bytes_held(4_096, 0), 8_292);
        assert_eq!(change_bytes_held(4_096, 9_000), 0);
    }

    #[test]
    fn a_change_is_held_to_its_bytes_so_wide_characters_count_whole() {
        // Each of these characters is three bytes and about one token.
        let wide = "変".repeat(3_000);
        assert_eq!(wide.len(), 9_000);
        let most = change_bytes_held(4_096, 0);
        assert!(wide.len() > most);
        let cut = Change::of_one(wide).for_a_reader_without_tools(Some(most));
        let kept = cut.strip_suffix("\n… the change was cut here.").unwrap();
        assert_eq!(kept.len(), most);
    }

    #[test]
    fn windows_read_as_their_tags_say() {
        assert_eq!(window_words(65_536), "64k");
        assert_eq!(window_words(32_768), "32k");
        assert_eq!(window_words(4_096), "4k");
        assert_eq!(window_words(200_000), "200k");
        assert_eq!(window_words(512), "512");
    }
}
