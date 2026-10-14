//! A sandbox probe: a script set as an agent's binary that tries reads and writes it must not
//! be able to do, reports each on stderr, and then runs the fake agent. The session's stderr
//! lands in its raw log, which the test reads afterwards.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::layout::{run_files, session_files};
use argonauts_testkit::journal;

use super::{Limits, write_file};

/// A line the probe prints for an action that worked, which a sandbox must prevent.
pub const LEAKED: &str = "probe leaked:";

/// A line the probe prints for an action the sandbox refused.
pub const BLOCKED: &str = "probe blocked:";

/// Writes an executable probe to `path` that runs `checks` and then `exec`s `program`, the
/// fake agent on `PATH`. A `--version` or `--help` call skips the checks, since doctor runs
/// those outside any sandbox.
pub fn write_probe(path: &Path, program: &str, checks: &[String]) -> anyhow::Result<PathBuf> {
    let script = format!(
        "#!/bin/sh\n\
         case \" $* \" in\n\
         *\" --version \"*|*\" --help \"*) ;;\n\
         *)\n\
         {}\n\
         ;;\n\
         esac\n\
         exec {program} \"$@\"\n",
        checks.join("\n")
    );
    write_file(path, &script)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making {} executable", path.display()))?;
    Ok(path.to_path_buf())
}

/// Tries to write `target`.
pub fn try_write(target: &Path) -> String {
    let target = quoted(target);
    format!(
        "if (echo leak > {target}) 2>/dev/null; then echo \"{LEAKED} write {target}\" >&2; \
         else echo \"{BLOCKED} write {target}\" >&2; fi"
    )
}

/// Tries to read `path`.
pub fn try_read(path: &Path) -> String {
    read_each(&quoted(path))
}

/// Tries to read every file matching `pattern` under `base`. `pattern` holds only plain path
/// characters and `*`. When the sandbox denies listing a folder, the pattern stays as it is,
/// and the probe reports that read as blocked.
pub fn try_read_matching(base: &Path, pattern: &str) -> String {
    read_each(&format!("{}/{pattern}", quoted(base)))
}

fn read_each(words: &str) -> String {
    format!(
        "for f in {words}; do if cat \"$f\" >/dev/null 2>&1; then echo \"{LEAKED} read $f\" >&2; \
         else echo \"{BLOCKED} read $f\" >&2; fi; done"
    )
}

/// Single-quoted for `sh`.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

impl Limits {
    /// Everything the newest run's sessions wrote on stderr.
    pub fn sessions_stderr(&self) -> anyhow::Result<String> {
        let run = journal::latest_run(self.repo.home()).context("no run yet")?;
        let sessions = run.join(run_files::SESSIONS);
        let mut text = String::new();
        let entries =
            fs::read_dir(&sessions).with_context(|| format!("listing {}", sessions.display()))?;
        for entry in entries {
            let file = entry?.path().join(session_files::RAW_STDERR);
            if let Ok(part) = fs::read_to_string(&file) {
                text.push_str(&part);
            }
        }
        Ok(text)
    }

    /// Plants `relative` under your Argonauts Code home, so a probe has something real to try.
    pub fn plant(&self, relative: &str) -> anyhow::Result<PathBuf> {
        let path = self.repo.home().join(relative);
        write_file(&path, "planted secret\n")?;
        Ok(path)
    }
}

/// The probe's lines about `what` (such as `read` or `write`), leaked or blocked.
pub fn probe_lines<'a>(stderr: &'a str, verdict: &str, what: &str) -> Vec<&'a str> {
    let prefix = format!("{verdict} {what} ");
    stderr
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect()
}
