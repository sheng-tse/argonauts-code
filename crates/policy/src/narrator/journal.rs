//! `runs/<run>/narrator.jsonl`, one line per narration. Nothing here touches `events.jsonl`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use argonauts_core::Spend;
use argonauts_core::layout::run_files;
use argonauts_core::policy::{NarratorLine, NarratorLineKind};

use super::cap;

pub fn append(run_dir: &Path, line: &NarratorLine) -> io::Result<()> {
    fs::create_dir_all(run_dir)?;
    let mut json = serde_json::to_string(line).map_err(io::Error::other)?;
    json.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join(run_files::NARRATOR))?;
    // One write per line, so a reader never sees half of one.
    file.write_all(json.as_bytes())
}

/// Every line so far, oldest first. A missing journal is empty; a line that doesn't parse is
/// an error.
pub fn read(run_dir: &Path) -> io::Result<Vec<NarratorLine>> {
    let text = match fs::read_to_string(run_dir.join(run_files::NARRATOR)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(io::Error::other))
        .collect()
}

/// What earlier narrators of this run spent and said, so the cap covers the whole run and a
/// new narrator doesn't repeat itself.
#[derive(Debug, Default)]
pub struct Earlier {
    pub spent: Spend,
    /// Narrated lines, oldest first, notices left out.
    pub said: Vec<String>,
}

/// Reads the journal for [`Earlier`]. A line that doesn't parse, such as one cut off by a crash,
/// is skipped, since its spend can't be read either way.
pub fn earlier(run_dir: &Path) -> io::Result<Earlier> {
    let text = match fs::read_to_string(run_dir.join(run_files::NARRATOR)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Earlier::default()),
        Err(err) => return Err(err),
    };
    let mut earlier = Earlier::default();
    for line in text.lines() {
        let Ok(line) = serde_json::from_str::<NarratorLine>(line) else {
            continue;
        };
        cap::add(&mut earlier.spent, &line.spend);
        if line.kind != NarratorLineKind::Notice {
            earlier.said.push(line.text);
        }
    }
    Ok(earlier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    #[test]
    fn lines_append_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).unwrap().is_empty());
        let line = NarratorLine {
            ts: OffsetDateTime::UNIX_EPOCH,
            run: None,
            kind: NarratorLineKind::Summary,
            text: "Two cards are building.".to_owned(),
            model: "claude-code/haiku".parse().unwrap(),
            spend: Spend::default(),
        };
        append(dir.path(), &line).unwrap();
        append(dir.path(), &line).unwrap();
        assert_eq!(read(dir.path()).unwrap(), vec![line.clone(), line]);
    }

    #[test]
    fn earlier_spend_and_lines_are_summed_past_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(earlier(dir.path()).unwrap().spent, Spend::default());
        let spent = Spend {
            input_tokens: 100,
            cost_usd: 0.02,
            ..Spend::default()
        };
        let said = NarratorLine {
            ts: OffsetDateTime::UNIX_EPOCH,
            run: None,
            kind: NarratorLineKind::Update,
            text: "Card 1 is running.".to_owned(),
            model: "claude-code/haiku".parse().unwrap(),
            spend: spent,
        };
        let failed = NarratorLine {
            kind: NarratorLineKind::Notice,
            text: "couldn't narrate".to_owned(),
            ..said.clone()
        };
        append(dir.path(), &said).unwrap();
        append(dir.path(), &failed).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.path().join(run_files::NARRATOR))
            .unwrap();
        file.write_all(b"{\"ts\":\"cut off").unwrap();
        let earlier = earlier(dir.path()).unwrap();
        assert!((earlier.spent.cost_usd - 0.04).abs() < 1e-9);
        assert_eq!(earlier.spent.input_tokens, 200);
        assert_eq!(earlier.said, vec!["Card 1 is running.".to_owned()]);
    }
}
