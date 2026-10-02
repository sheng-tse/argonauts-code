//! Each running session's `status.json`, which its `argonauts_status` MCP tool reads. Written
//! after a relevant change, at most once a second per session.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use time::{Duration, OffsetDateTime};

use argonauts_core::api::StatusSummary;

use crate::error::EngineResult;

/// The shortest time between two writes of one session's file.
pub const MIN_INTERVAL: Duration = Duration::SECOND;

/// Remembers when each session's file was last written, and the newest summary a write skipped.
#[derive(Debug, Default)]
pub struct StatusFiles {
    written: BTreeMap<PathBuf, OffsetDateTime>,
    held: BTreeMap<PathBuf, StatusSummary>,
}

impl StatusFiles {
    /// Writes the file unless it was written less than a second ago. True when written. A
    /// skipped summary is held for [`StatusFiles::flush`], so the file never stays stale.
    pub fn write(
        &mut self,
        path: &Path,
        summary: &StatusSummary,
        now: OffsetDateTime,
    ) -> EngineResult<bool> {
        if !self.due(path, now) {
            self.held.insert(path.to_path_buf(), summary.clone());
            return Ok(false);
        }
        self.write_now(path, summary, now)?;
        Ok(true)
    }

    /// Writes every held summary whose second has passed. Returns how many were written.
    pub fn flush(&mut self, now: OffsetDateTime) -> EngineResult<usize> {
        let due: Vec<PathBuf> = self
            .held
            .keys()
            .filter(|path| self.due(path, now))
            .cloned()
            .collect();
        for path in &due {
            if let Some(summary) = self.held.remove(path) {
                self.write_now(path, &summary, now)?;
            }
        }
        Ok(due.len())
    }

    /// Stops tracking a session that ended.
    pub fn forget(&mut self, path: &Path) {
        self.written.remove(path);
        self.held.remove(path);
    }

    fn due(&self, path: &Path, now: OffsetDateTime) -> bool {
        self.written
            .get(path)
            .is_none_or(|last| now - *last >= MIN_INTERVAL)
    }

    fn write_now(
        &mut self,
        path: &Path,
        summary: &StatusSummary,
        now: OffsetDateTime,
    ) -> EngineResult<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        // Written aside and renamed, so the MCP tool never reads half a file.
        let mut staging = path.as_os_str().to_owned();
        staging.push(".tmp");
        let staging = PathBuf::from(staging);
        fs::write(&staging, serde_json::to_vec_pretty(summary)?)?;
        fs::rename(&staging, path)?;
        self.written.insert(path.to_path_buf(), now);
        self.held.remove(path);
        Ok(())
    }
}
