//! The review diff is cut at `review.max_diff_lines` into per-file chunks, with a note saying
//! what was cut.

/// The chunks to send, one per file, and the note (`None` when nothing was cut). Whole files
/// go in order while they fit; the first that doesn't is shortened to the lines left, and the
/// rest are left out and named in the note.
pub fn cut(unified_diff: &str, max_lines: u32) -> (Vec<String>, Option<String>) {
    let files = split_files(unified_diff);
    let mut left = max_lines as usize;
    let mut chunks = Vec::new();
    let mut shortened = Vec::new();
    let mut omitted = Vec::new();
    for file in &files {
        let lines = file.text.lines().count();
        if lines <= left {
            chunks.push(file.text.to_owned());
            left -= lines;
        } else if left > 0 {
            let kept: Vec<&str> = file.text.lines().take(left).collect();
            chunks.push(format!("{}\n", kept.join("\n")));
            shortened.push(format!("{} (cut from {lines} lines to {left})", file.name));
            left = 0;
        } else {
            omitted.push(file.name.clone());
        }
    }
    if shortened.is_empty() && omitted.is_empty() {
        return (chunks, None);
    }
    let total = files.iter().map(|f| f.text.lines().count()).sum::<usize>();
    let mut note = format!("The diff has {total} lines and was cut at {max_lines}.");
    if !shortened.is_empty() {
        note.push_str(&format!(" Shortened: {}.", shortened.join(", ")));
    }
    if !omitted.is_empty() {
        note.push_str(&format!(
            " Left out, {} file{}: {}.",
            omitted.len(),
            if omitted.len() == 1 { "" } else { "s" },
            omitted.join(", ")
        ));
    }
    (chunks, Some(note))
}

struct FileDiff<'a> {
    name: String,
    text: &'a str,
}

/// Splits at each `diff --git` header. Text before the first header, or a diff with none,
/// is one chunk of its own.
fn split_files(diff: &str) -> Vec<FileDiff<'_>> {
    let mut starts: Vec<usize> = diff
        .match_indices("diff --git ")
        .filter(|(at, _)| *at == 0 || diff.as_bytes()[at - 1] == b'\n')
        .map(|(at, _)| at)
        .collect();
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    let mut files = Vec::new();
    for (i, start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(diff.len());
        let text = &diff[*start..end];
        if text.is_empty() {
            continue;
        }
        files.push(FileDiff {
            name: file_name(text),
            text,
        });
    }
    files
}

/// `b/<path>` from the header, else the first line.
fn file_name(chunk: &str) -> String {
    let header = chunk.lines().next().unwrap_or_default();
    match header.strip_prefix("diff --git ") {
        Some(paths) => paths
            .rsplit_once(" b/")
            .map_or(paths, |(_, path)| path)
            .to_owned(),
        None => "the diff's preamble".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, lines: usize) -> String {
        let mut text = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
        for n in 0..lines {
            text.push_str(&format!("+line {n}\n"));
        }
        text
    }

    #[test]
    fn a_short_diff_goes_whole_in_per_file_chunks() {
        let diff = format!("{}{}", file("src/a.rs", 3), file("src/b.rs", 2));
        let (chunks, note) = cut(&diff, 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.concat(), diff);
        assert!(note.is_none());
    }

    #[test]
    fn a_long_diff_is_cut_and_the_note_names_what_was_cut() {
        let diff = format!(
            "{}{}{}",
            file("src/a.rs", 5),
            file("src/b.rs", 20),
            file("src/c.rs", 5)
        );
        let (chunks, note) = cut(&diff, 15);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], file("src/a.rs", 5));
        assert_eq!(chunks[1].lines().count(), 7);
        assert!(chunks[1].starts_with("diff --git a/src/b.rs"));
        let note = note.unwrap();
        assert!(note.contains("src/b.rs (cut from 23 lines to 7)"), "{note}");
        assert!(note.contains("1 file: src/c.rs"), "{note}");
        let total: usize = chunks.iter().map(|c| c.lines().count()).sum();
        assert!(total <= 15);
    }

    #[test]
    fn a_diff_without_headers_is_cut_by_lines() {
        let diff = "a\nb\nc\nd\n";
        let (chunks, note) = cut(diff, 2);
        assert_eq!(chunks, vec!["a\nb\n".to_owned()]);
        assert!(note.unwrap().contains("cut at 2"));
    }

    #[test]
    fn paths_with_spaces_keep_their_name() {
        assert_eq!(
            file_name("diff --git a/my dir/x.rs b/my dir/x.rs\n"),
            "my dir/x.rs"
        );
    }

    #[test]
    fn an_empty_diff_has_no_chunks() {
        assert_eq!(cut("", 10), (Vec::new(), None));
    }
}
