//! Reading a session file from where the import left it: whole lines only, and only the lines
//! that can carry counts, so the rest of a long transcript is never held.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The lines a pass kept, how many bytes of the file it went through, and how many lines it
/// couldn't read.
pub(super) struct Text {
    pub lines: Vec<String>,
    pub consumed: u64,
    /// Lines that looked like they held counts but weren't text.
    pub unreadable: u32,
}

/// Markers a line must hold to be worth parsing: Claude Code's answers carry their usage, and
/// Codex's counts, models and folders come in lines of these three kinds.
const MARKERS: [&str; 4] = [
    "\"usage\"",
    "\"token_count\"",
    "\"turn_context\"",
    "\"session_meta\"",
];

pub(super) fn is_compressed(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "zst")
}

/// The whole lines of `path` after byte `from`, the ones with a marker kept. A compressed file
/// is opened with `zstd`, and `from` counts bytes of what it holds once opened, so a file read
/// in part before it was compressed carries on where it was; without `zstd`, `None`.
pub(super) fn read_from(path: &Path, from: u64, zstd: Option<&Path>) -> io::Result<Option<Text>> {
    if is_compressed(path) {
        let Some(zstd) = zstd else {
            return Ok(None);
        };
        let mut child = Command::new(zstd)
            .args(["-dc", "--"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no output"))?;
        let mut reader = BufReader::new(stdout);
        let skipped = io::copy(&mut (&mut reader).take(from), &mut io::sink())?;
        let text = kept_lines(reader);
        let status = child.wait()?;
        let mut text = text?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "zstd couldn't open {}",
                path.display()
            )));
        }
        text.consumed += skipped;
        return Ok(Some(text));
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut text = kept_lines(BufReader::new(file))?;
    text.consumed += from;
    Ok(Some(text))
}

fn kept_lines(mut reader: impl BufRead) -> io::Result<Text> {
    let mut text = Text {
        lines: Vec::new(),
        consumed: 0,
        unreadable: 0,
    };
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        // A last line still being written waits for the next pass.
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        text.consumed += read as u64;
        match std::str::from_utf8(&line) {
            Ok(words) => {
                if MARKERS.iter().any(|marker| words.contains(marker)) {
                    text.lines.push(words.trim_end().to_owned());
                }
            }
            Err(_) => {
                let marked = MARKERS.iter().any(|marker| {
                    line.windows(marker.len())
                        .any(|window| window == marker.as_bytes())
                });
                if marked {
                    text.unreadable += 1;
                }
            }
        }
    }
    Ok(text)
}

/// `name` on `PATH`, when it's there and runs.
pub(super) fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Files under `dir` whose name `wanted` accepts, down to `depth` folders, in path order.
pub(super) fn walk(dir: &Path, depth: usize, wanted: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth > 0 {
            out.extend(walk(&path, depth - 1, wanted));
        } else if kind.is_file() && wanted(&path) {
            out.push(path);
        }
    }
    out.sort();
    out
}
