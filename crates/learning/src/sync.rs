//! `argonauts sync`: one source, `.argonauts/conventions.md`, rendered into one `AGENTS.md`
//! and a short `CLAUDE.md` that starts with `@AGENTS.md`, so every harness reads the same
//! conventions once. It also checks exported skills against their hashes. A file or skill
//! edited by hand is reported as drift and never overwritten.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use argonauts_core::hash::sha256_hex;

use crate::error::Result;
use crate::skills::Drift;
use crate::store::{AGENTS_FILE, CLAUDE_FILE, Store};

/// What a sync did or would do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncReport {
    /// Written, or under [`SyncMode::Check`], what would be.
    pub written: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    /// Such as `AGENTS.md` nearing Codex's 32 KiB or Antigravity CLI's 24,000-byte cut.
    pub warnings: Vec<String>,
    /// Files edited by hand since the last sync.
    pub edited: Vec<PathBuf>,
    pub skill_drift: Vec<Drift>,
    /// Which CLI reads which file, once the files are in place.
    pub readers: Option<String>,
}

/// The CLIs that read `AGENTS.md` by itself.
const AGENTS_READERS: &str = "Codex and Antigravity CLI";

/// Codex reads `AGENTS.md` files up to this many bytes combined and drops the rest.
pub const CODEX_CAP: usize = 32 * 1024;
/// Antigravity CLI cuts each instruction file at this many bytes.
pub const ANTIGRAVITY_CAP: usize = 24_000;
/// Warn once a file is this close to a cap, in percent.
const NEAR_CAP_PERCENT: usize = 90;

const MARKER_START: &str = "<!-- argonauts sync: written from .argonauts/conventions.md. Edit that file, then run argonauts sync. sha256=";
const MARKER_END: &str = " -->";

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^<!-- argonauts sync: [^\n]*sha256=([0-9a-f]{64}) -->[ \t]*\r?\n?")
        .unwrap_or_else(|err| panic!("sync marker pattern: {err}"))
});

static HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(#{1,6})[ \t]+(.*?)[ \t#]*$")
        .unwrap_or_else(|err| panic!("heading pattern: {err}"))
});

/// `AGENTS.md` and `CLAUDE.md` for these conventions. A section headed "Claude Code only" goes
/// only into `CLAUDE.md`, since the other harnesses would read it as theirs.
pub fn render(conventions: &str) -> (String, String) {
    let (shared, claude_only) = split_claude_only(conventions);
    let shared = tidy(&shared);
    let claude_only = tidy(&claude_only);

    let agents = with_marker("", &shared);
    let mut claude = String::from("@AGENTS.md\n");
    if !claude_only.is_empty() {
        claude.push('\n');
        claude.push_str(&claude_only);
    }
    let claude = with_marker("@AGENTS.md\n", &claude["@AGENTS.md\n".len()..]);
    (agents, claude)
}

/// `head`, the marker with the hash of the file without it, then `rest`.
fn with_marker(head: &str, rest: &str) -> String {
    let hash = sha256_hex(format!("{head}{rest}").as_bytes());
    format!("{head}{MARKER_START}{hash}{MARKER_END}\n{rest}")
}

/// Whether `text` is still exactly what sync wrote: it has the marker, and the rest matches its
/// hash.
fn intact(text: &str) -> bool {
    let Some(captures) = MARKER.captures(text) else {
        return false;
    };
    let (Some(whole), Some(hash)) = (captures.get(0), captures.get(1)) else {
        return false;
    };
    let rest = format!("{}{}", &text[..whole.start()], &text[whole.end()..]);
    sha256_hex(rest.as_bytes()) == hash.as_str()
}

/// The conventions without their "Claude Code only" sections, and those sections. Headings
/// inside fenced code don't count.
fn split_claude_only(conventions: &str) -> (String, String) {
    let mut shared = String::new();
    let mut claude = String::new();
    let mut fence: Option<&str> = None;
    let mut inside: Option<usize> = None;
    for line in conventions.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let opener = trimmed.trim_start();
        if let Some(mark) = fence {
            if opener.starts_with(mark) {
                fence = None;
            }
        } else if opener.starts_with("```") || opener.starts_with("~~~") {
            fence = Some(&opener[..3]);
        } else if let Some(captures) = HEADING.captures(trimmed) {
            let level = captures.get(1).map_or(0, |m| m.as_str().len());
            let title = captures.get(2).map_or("", |m| m.as_str());
            if inside.is_some_and(|open| level <= open) {
                inside = None;
            }
            if inside.is_none() && title.to_lowercase().contains("claude code only") {
                inside = Some(level);
            }
        }
        if inside.is_some() {
            claude.push_str(line);
        } else {
            shared.push_str(line);
        }
    }
    (shared, claude)
}

/// No runs of blank lines where a section came out, and one newline at the end.
fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 || out.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Writes what changed, leaving hand-edited files alone.
    Write,
    /// Only reports what would change.
    Check,
    /// Also overwrites hand-edited files. Only you can ask for it, never an agent session.
    Force,
}

/// Renders both files into the checkout and checks exported skills for drift.
pub fn sync(store: &Store, mode: SyncMode) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    match read_optional(&store.conventions())? {
        Some(conventions) => {
            let (agents, claude) = render(&conventions);
            report.warnings.extend(size_warnings(&agents));
            let claude_path = store.repo_root.join(CLAUDE_FILE);
            for (name, wanted) in [(AGENTS_FILE, agents), (CLAUDE_FILE, claude.clone())] {
                place(&store.repo_root.join(name), &wanted, mode, &mut report)?;
            }
            // A CLAUDE.md sync left alone is the one Claude Code reads.
            let claude = if report.edited.contains(&claude_path) {
                read_optional(&claude_path)?.unwrap_or_default()
            } else {
                claude
            };
            if imports_agents(&claude) {
                report.readers = Some(format!(
                    "Claude Code reads {CLAUDE_FILE}, which imports {AGENTS_FILE}. {AGENTS_READERS} \
                     read {AGENTS_FILE}."
                ));
            } else {
                report.warnings.push(format!(
                    "{CLAUDE_FILE} doesn't import {AGENTS_FILE}, so Claude Code won't read the \
                     conventions. Add @{AGENTS_FILE} as its first line."
                ));
                report.readers = Some(format!("{AGENTS_READERS} read {AGENTS_FILE}."));
            }
        }
        None => report.warnings.push(
            "There's no .argonauts/conventions.md, so AGENTS.md and CLAUDE.md were left as they \
             are."
                .to_owned(),
        ),
    }
    report.skill_drift = crate::skills::drift(store)?;
    for drift in &report.skill_drift {
        report.warnings.push(format!(
            "The {} skill was edited since it was exported, so it was left alone. Re-export its \
             recipe, or keep your edits.",
            drift.name
        ));
    }
    Ok(report)
}

fn place(path: &Path, wanted: &str, mode: SyncMode, report: &mut SyncReport) -> Result<()> {
    let current = read_optional(path)?;
    if current.as_deref() == Some(wanted) {
        report.unchanged.push(path.to_path_buf());
        return Ok(());
    }
    let hand_edited = current.as_deref().is_some_and(|text| !intact(text));
    if hand_edited && mode != SyncMode::Force {
        report.edited.push(path.to_path_buf());
        report.warnings.push(format!(
            "{} was edited by hand, so sync left it alone. Move the edits into \
             .argonauts/conventions.md, or run sync with --force to replace them.",
            file_name(path)
        ));
        return Ok(());
    }
    if mode != SyncMode::Check {
        write_atomically(path, wanted)?;
        if hand_edited {
            report
                .warnings
                .push(format!("Replaced the hand edits in {}.", file_name(path)));
        }
    }
    report.written.push(path.to_path_buf());
    Ok(())
}

/// Claude Code reads a CLAUDE.md line such as `@AGENTS.md` as an import. Without one it never
/// reads AGENTS.md where CLAUDE.md exists, unless you chose both in `/config`.
fn imports_agents(claude: &str) -> bool {
    claude.lines().any(|line| {
        line.trim()
            .strip_prefix('@')
            .map(|target| target.trim_start_matches("./"))
            == Some(AGENTS_FILE)
    })
}

fn size_warnings(agents: &str) -> Vec<String> {
    let bytes = agents.len();
    let mut warnings = Vec::new();
    for (harness, cap, cap_text) in [
        ("Codex", CODEX_CAP, "32 KiB"),
        ("Antigravity CLI", ANTIGRAVITY_CAP, "24,000 bytes"),
    ] {
        if bytes > cap {
            warnings.push(format!(
                "AGENTS.md is {bytes} bytes, over {harness}'s {cap_text}, so {harness} drops the \
                 rest. Shorten the conventions."
            ));
        } else if bytes * 100 >= cap * NEAR_CAP_PERCENT {
            warnings.push(format!(
                "AGENTS.md is {bytes} bytes, near {harness}'s {cap_text}, past which {harness} \
                 drops the rest."
            ));
        }
    }
    warnings
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Through a file beside it, so a reader never sees half the conventions.
fn write_atomically(path: &Path, text: &str) -> Result<()> {
    let temp = path.with_file_name(format!(
        ".{}.argonauts-{}",
        file_name(path),
        std::process::id()
    ));
    fs::write(&temp, text)?;
    if let Err(err) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(err.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_detect_edits() {
        let (agents, claude) = render("# Rules\n\nUse tabs.\n");
        assert!(intact(&agents));
        assert!(intact(&claude));
        assert!(!intact(&agents.replace("tabs", "spaces")));
        assert!(!intact("# Rules\n"));
    }

    #[test]
    fn fenced_headings_stay_put() {
        let (shared, claude) = split_claude_only(
            "# A\n```\n# Claude Code only\n```\n## Claude Code only\nx\n# B\ny\n",
        );
        assert_eq!(shared, "# A\n```\n# Claude Code only\n```\n# B\ny\n");
        assert_eq!(claude, "## Claude Code only\nx\n");
    }
}
