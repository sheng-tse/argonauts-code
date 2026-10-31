//! What neither ledger records, one line each time, in `~/.argonauts/usage/uses/<yyyy-mm>.jsonl`:
//! a goal the conversation keeps itself, ultracode on a turn or on a run started for it, a
//! command used, the workflow view opened, a crash and where, and how long a start took. A line
//! holds the day and a fixed word or two, nothing more. Only the day's event reads them.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use argonauts_core::input::SlashCommand;
use argonauts_core::runs::api::day_format;
use argonauts_core::{Paths, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

use crate::controller::Ctx;
use crate::usage::usage_ledger::local_day;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Use {
    /// The conversation began keeping a goal. A run's goals are counted from its log instead.
    Goal,
    /// A turn went out with ultracode, or a run started because of it.
    Ultracode,
    /// A built-in command, such as `/model`, by its name.
    Command { name: String },
    /// The workflow view opened.
    WorkflowView,
    /// Argonauts Code crashed in this part of its own code, such as `tui`.
    Crash { place: String },
    /// A start, by how long until the screen showed: `under_1s`, `1_to_3s`, `over_3s`.
    Started { bracket: String },
}

impl Use {
    /// A command by the name it is typed with, such as `/model`: the command's own word, never
    /// what was typed after it.
    pub(crate) fn command(command: &SlashCommand) -> Self {
        let word = serde_json::to_value(command)
            .ok()
            .and_then(|value| Some(value.get("command")?.as_str()?.replace('_', "-")))
            .unwrap_or_else(|| "other".to_owned());
        Self::Command {
            name: format!("/{word}"),
        }
    }

    /// The part of Argonauts Code's own code a panic at `file` came from: the crate's folder
    /// under `crates/`, `adapters` for any CLI's adapter, and `elsewhere` for a dependency's.
    pub(crate) fn crash_at(file: &str) -> Self {
        let mut folders = file.strip_prefix("crates/").unwrap_or_default().split('/');
        let place = match folders.next() {
            Some("adapters") => "adapters",
            Some(own) if OWN_CRATES.contains(&own) && folders.next().is_some() => own,
            _ => "elsewhere",
        };
        Self::Crash {
            place: place.to_owned(),
        }
    }

    /// The bracket a start of `took` falls in.
    pub(crate) fn started(took: std::time::Duration) -> Self {
        let bracket = match took.as_millis() {
            0..1_000 => "under_1s",
            1_000..3_000 => "1_to_3s",
            _ => "over_3s",
        };
        Self::Started {
            bracket: bracket.to_owned(),
        }
    }
}

/// The crates a release ships, by their folders.
const OWN_CRATES: &[&str] = &[
    "app",
    "cli",
    "conductor",
    "core",
    "crew",
    "endpoints",
    "learning",
    "mcp",
    "policy",
    "registry",
    "runner",
    "tui",
];

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

/// Notes one use today, while usage reports are on.
pub(crate) fn count(ctx: &Ctx, what: Use) {
    if ctx.settings().telemetry.enabled {
        used(&ctx.env.paths, what);
    }
}

/// From now on, a panic anywhere in this process is noted as a crash in its part of the code,
/// before the panic is reported as it would be without this. Only the place is kept, never the
/// message, and the line stays on this machine until a report reads it. Installed once, first
/// thing, so the screen's own hook, which gives the terminal back, runs before it.
pub fn count_crashes() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let (Some(at), Ok(paths)) = (info.location(), Paths::from_env()) {
                used(&paths, Use::crash_at(at.file()));
            }
            previous(info);
        }));
    });
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
