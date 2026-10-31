//! The counts in each `@@` header say where a hunk ends, which is what tells a removed line
//! `-- x` apart from a `--- a/file` header. Diffs written by hand or by a model often get the
//! counts wrong, so lines that plainly continue a hunk after its counts run out are kept too.

use super::{DiffHunk, DiffKind, DiffLine, DiffParseError, DiffView};

const FILE_HEADERS: &[&str] = &[
    "diff ",
    "index ",
    "--- ",
    "+++ ",
    "new file mode",
    "deleted file mode",
    "old mode",
    "new mode",
    "similarity index",
    "dissimilarity index",
    "rename from",
    "rename to",
    "copy from",
    "copy to",
    "Binary files",
];

/// The hunk being read.
struct Open {
    hunk: DiffHunk,
    old: u32,
    new: u32,
    /// Lines the header still promises from the old and new file.
    old_left: u32,
    new_left: u32,
}

impl Open {
    fn expects_more(&self) -> bool {
        self.old_left > 0 || self.new_left > 0
    }

    /// Adds one hunk line, or returns false if it isn't one.
    fn push(&mut self, raw: &str) -> bool {
        let (kind, text) = match raw.chars().next() {
            None => (DiffKind::Context, ""),
            Some(' ') => (DiffKind::Context, &raw[1..]),
            Some('-') => (DiffKind::Removed, &raw[1..]),
            Some('+') => (DiffKind::Added, &raw[1..]),
            _ => return false,
        };
        let (old_line, new_line) = match kind {
            DiffKind::Context => (Some(self.old), Some(self.new)),
            DiffKind::Removed => (Some(self.old), None),
            DiffKind::Added => (None, Some(self.new)),
        };
        if old_line.is_some() {
            self.old = self.old.saturating_add(1);
            self.old_left = self.old_left.saturating_sub(1);
        }
        if new_line.is_some() {
            self.new = self.new.saturating_add(1);
            self.new_left = self.new_left.saturating_sub(1);
        }
        self.hunk.lines.push(DiffLine {
            kind,
            old_line,
            new_line,
            text: text.to_owned(),
        });
        true
    }
}

pub(super) fn unified(diff: &str) -> Result<DiffView, DiffParseError> {
    let mut hunks = Vec::new();
    let mut open: Option<Open> = None;
    let mut stray: Option<usize> = None;
    for (index, raw) in diff.lines().enumerate() {
        let line = index + 1;
        let error = |message: String| DiffParseError { line, message };
        if raw.starts_with("@@") {
            if let Some(done) = open.take() {
                hunks.push(done.hunk);
            }
            let (old, old_len, new, new_len) =
                hunk_header(raw).ok_or_else(|| error(format!("bad hunk header {raw:?}")))?;
            open = Some(Open {
                hunk: DiffHunk::default(),
                old,
                new,
                old_left: old_len,
                new_left: new_len,
            });
            continue;
        }
        // "\ No newline at end of file" belongs to the line before it.
        if raw.starts_with('\\') {
            continue;
        }
        match open.as_mut() {
            Some(hunk) if hunk.expects_more() => {
                if !hunk.push(raw) {
                    return Err(error(format!("not a diff line: {raw:?}")));
                }
            }
            Some(hunk) => {
                let header = FILE_HEADERS.iter().any(|h| raw.starts_with(h));
                if header || raw.trim().is_empty() {
                    continue;
                }
                if !hunk.push(raw) {
                    return Err(error(format!("unexpected line after a hunk: {raw:?}")));
                }
            }
            // Before the first hunk: commit messages, file headers, anything.
            None => {
                if stray.is_none() && !raw.trim().is_empty() {
                    let header = FILE_HEADERS.iter().any(|h| raw.starts_with(h));
                    if !header {
                        stray = Some(line);
                    }
                }
            }
        }
    }
    if let Some(done) = open.take() {
        hunks.push(done.hunk);
    }
    if hunks.is_empty()
        && let Some(line) = stray
    {
        return Err(DiffParseError {
            line,
            message: "no @@ hunk header found".to_owned(),
        });
    }
    Ok(DiffView {
        hunks,
        caption: None,
    })
}

/// Reads `@@ -a,b +c,d @@`, where a missing count means one line.
fn hunk_header(raw: &str) -> Option<(u32, u32, u32, u32)> {
    let body = raw.strip_prefix("@@")?;
    let end = body.find("@@")?;
    let mut parts = body[..end].split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let range = |part: &str| -> Option<(u32, u32)> {
        match part.split_once(',') {
            Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
            None => Some((part.parse().ok()?, 1)),
        }
    };
    let (old_start, old_len) = range(old)?;
    let (new_start, new_len) = range(new)?;
    Some((old_start, old_len, new_start, new_len))
}
