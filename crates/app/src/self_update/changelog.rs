//! What's new, from the changelog built into the program: the repository's `CHANGELOG.md`,
//! through the link `crates/app/CHANGELOG.md`, so the crate's package holds it too. It is in the
//! Keep a Changelog form, newest release first. A copy that can't reach the internet still knows
//! what changed in it.

/// The changelog this build was made with.
const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// `/release-notes`' lines: the version running, then each release with something in it, its
/// sections and their items, as plain text.
pub(crate) fn release_notes() -> Vec<String> {
    notes_from(CHANGELOG, env!("CARGO_PKG_VERSION"))
}

fn notes_from(changelog: &str, running: &str) -> Vec<String> {
    let mut lines = vec![format!("This is Argonauts Code {running}.")];
    // Each release's lines wait until something is in them, so an empty one is left out.
    let mut release: Vec<String> = Vec::new();
    let mut said = false;
    let mut in_release = false;
    let mut flush = |release: &mut Vec<String>, said: &mut bool| {
        if *said {
            lines.push(String::new());
            lines.append(release);
        }
        release.clear();
        *said = false;
    };
    for line in changelog.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            flush(&mut release, &mut said);
            in_release = true;
            release.push(release_heading(heading));
            continue;
        }
        if !in_release || trimmed.is_empty() || is_link_reference(trimmed) {
            continue;
        }
        if let Some(section) = trimmed.strip_prefix("### ") {
            release.push(format!("{}:", plain(section)));
        } else if let Some(item) = trimmed.strip_prefix("- ") {
            release.push(format!("- {}", plain(item)));
            said = true;
        } else if let Some(last) = release.last_mut().filter(|_| line.starts_with("  ")) {
            // An item or a paragraph carried on to the next line.
            last.push(' ');
            last.push_str(&plain(trimmed));
        } else {
            release.push(plain(trimmed));
            said = true;
        }
    }
    flush(&mut release, &mut said);
    lines
}

/// `[0.1.1] - 2026-11-02` as `Argonauts Code 0.1.1 · 2026-11-02`, and `[Unreleased]` as what's
/// coming. A version that links somewhere, as `[0.1.1](https://…)`, reads as itself.
fn release_heading(heading: &str) -> String {
    let (name, date) = heading
        .split_once(" - ")
        .map_or((heading, None), |(name, date)| (name, Some(date.trim())));
    let name = plain(name.trim());
    let name = name.trim_start_matches('[').trim_end_matches(']');
    if name.eq_ignore_ascii_case("unreleased") {
        return "Coming next".to_owned();
    }
    match date {
        Some(date) => format!("Argonauts Code {name} · {date}"),
        None => format!("Argonauts Code {name}"),
    }
}

/// `[0.1.0]: https://…`, the links the headings point to.
fn is_link_reference(line: &str) -> bool {
    line.starts_with('[') && line.contains("]: ")
}

/// Markdown's marks taken off: `[text](link)` reads as its text, and code as itself.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](").map(|at| open + at) else {
            break;
        };
        let Some(end) = rest[close..].find(')').map(|at| close + at) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..close]);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out.replace('`', "").replace("**", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGELOG: &str = "# Changelog

Notable changes, newest first.

## [Unreleased]

## [0.1.1] - 2026-11-02

### Fixed

- `/update` keeps the rows so far when Esc stops it, and reads
  `stopped` on the rest.

## [0.1.0] - 2026-10-30

The first public release.

### Added

- One anonymous usage report a day (see [PRIVACY.md](PRIVACY.md)).

[Unreleased]: https://github.com/sheng-tse/argonauts-code/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/sheng-tse/argonauts-code/releases/tag/v0.1.1
";

    #[test]
    fn each_release_reads_as_plain_lines_newest_first() {
        assert_eq!(
            notes_from(CHANGELOG, "0.1.1"),
            [
                "This is Argonauts Code 0.1.1.",
                "",
                "Argonauts Code 0.1.1 · 2026-11-02",
                "Fixed:",
                "- /update keeps the rows so far when Esc stops it, and reads stopped on the rest.",
                "",
                "Argonauts Code 0.1.0 · 2026-10-30",
                "The first public release.",
                "Added:",
                "- One anonymous usage report a day (see PRIVACY.md).",
            ]
        );
    }

    #[test]
    fn a_heading_that_links_its_version_reads_as_the_version() {
        let linked = "# Changelog

## [0.1.2](https://github.com/sheng-tse/argonauts-code/compare/v0.1.1...v0.1.2) - 2026-11-09

### Fixed

- a fix
";
        assert_eq!(
            notes_from(linked, "0.1.2"),
            [
                "This is Argonauts Code 0.1.2.",
                "",
                "Argonauts Code 0.1.2 · 2026-11-09",
                "Fixed:",
                "- a fix",
            ]
        );
    }

    #[test]
    fn the_changelog_built_in_names_this_release() {
        let notes = release_notes();
        assert_eq!(
            notes[0],
            format!("This is Argonauts Code {}.", env!("CARGO_PKG_VERSION"))
        );
        let heading = format!("Argonauts Code {}", env!("CARGO_PKG_VERSION"));
        assert!(
            notes.iter().any(|line| line.starts_with(&heading)),
            "{notes:#?}"
        );
    }
}
