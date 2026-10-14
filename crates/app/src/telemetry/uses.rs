//! Features used that neither ledger records, one line each time, in
//! `~/.argonauts/usage/uses/<yyyy-mm>.jsonl`: a goal the conversation keeps itself, and ultracode
//! on a turn or on a run started for it. A line holds the day and the feature's name, nothing
//! more. Only the day's event reads them.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use argonauts_core::api::day_format;
use argonauts_core::{Paths, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

use crate::usage_ledger::local_day;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Use {
    /// The conversation began keeping a goal. A run's goals are counted from its log instead.
    Goal,
    /// A turn went out with ultracode, or a run started because of it.
    Ultracode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct UseLine {
    #[serde(with = "day_format")]
    pub day: Date,
    pub used: Use,
}

/// Notes one use today. A line that can't be written is only logged: a count is never worth
/// an error.
pub(crate) fn used(paths: &Paths, used: Use) {
    let line = UseLine {
        day: local_day(OffsetDateTime::now_utc()),
        used,
    };
    if let Err(err) = append(&dir(paths), &line) {
        tracing::debug!("couldn't note a feature's use: {err}");
    }
}

/// Every line from `first` on. Lines that don't read are skipped.
pub(crate) fn read_since(paths: &Paths, first: Date) -> Vec<UseLine> {
    let dir = dir(paths);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let first_month = month_name(first);
    let mut months: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".jsonl"))
                .is_some_and(|month| month >= first_month.as_str())
        })
        .collect();
    months.sort();
    months
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .flat_map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<UseLine>(line).ok())
                .collect::<Vec<_>>()
        })
        .filter(|line| line.day >= first)
        .collect()
}

fn dir(paths: &Paths) -> PathBuf {
    paths.usage_dir().join("uses")
}

/// One write per line, so two processes appending at once never interleave within a line.
fn append(dir: &Path, line: &UseLine) -> io::Result<()> {
    create_private_dir_all(dir)?;
    let mut text = serde_json::to_string(line).map_err(io::Error::other)?;
    text.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join(format!("{}.jsonl", month_name(line.day))))?
        .write_all(text.as_bytes())
}

fn month_name(day: Date) -> String {
    format!("{:04}-{:02}", day.year(), u8::from(day.month()))
}
