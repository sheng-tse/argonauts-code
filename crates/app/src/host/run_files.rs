//! Files in a run's directory that the client writes and the run host reads back:
//! `start.json`, `settings.json`, `approve.token`, and `pending-answers.jsonl`. Also where to
//! reach the run's control socket.

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::RunId;
use argonauts_core::base::hash::sha256_hex;
use argonauts_core::base::layout::run_files;
use argonauts_core::base::paths::write_private_file;
use argonauts_core::runs::api::StartRun;
use argonauts_core::settings::Settings;
use nix::unistd::geteuid;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::control::ControlRequest;

/// What `start` asked for. The run host writes `RunStarted` from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StartRecord {
    pub run: RunId,
    pub request: StartRun,
    /// Exactly the overrides the run's settings were resolved with, which the settings reloader
    /// applies again.
    pub overrides: Vec<String>,
    /// Whether a person at a terminal vetted `overrides`, so a safety key among them applies.
    pub interactive: bool,
    /// One of the evolution loop's replays, whose goal and plan approvals follow your rules
    /// at every reload too, since nobody answers a replay.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replay: bool,
    pub repo_root: PathBuf,
    #[serde(with = "time::serde::rfc3339")]
    pub created: OffsetDateTime,
}

/// `settings.json`: the resolved settings the run started with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SettingsRecord {
    pub sha256: String,
    pub settings: Settings,
    /// The agents a settings layer turned on, which `settings` alone can't tell from an
    /// agent's default.
    #[serde(default)]
    pub turned_on: Vec<String>,
}

pub(crate) fn write_start(run_dir: &Path, record: &StartRecord) -> anyhow::Result<()> {
    write_json(&run_dir.join(run_files::START), record)
}

pub(crate) fn read_start(run_dir: &Path) -> anyhow::Result<StartRecord> {
    read_json(&run_dir.join(run_files::START))
}

pub(crate) fn write_settings(run_dir: &Path, record: &SettingsRecord) -> anyhow::Result<()> {
    write_json(&run_dir.join(run_files::SETTINGS), record)
}

pub(crate) fn read_settings(run_dir: &Path) -> anyhow::Result<SettingsRecord> {
    read_json(&run_dir.join(run_files::SETTINGS))
}

/// 32 random bytes as hex.
pub(crate) fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Mode 0600, so only you can read it.
pub(crate) fn write_token(run_dir: &Path, token: &str) -> io::Result<()> {
    write_private_file(&run_dir.join(run_files::APPROVE_TOKEN), token.as_bytes())
}

pub(crate) fn read_token(run_dir: &Path) -> io::Result<String> {
    Ok(
        std::fs::read_to_string(run_dir.join(run_files::APPROVE_TOKEN))?
            .trim()
            .to_owned(),
    )
}

/// Where the socket file lives. Bind and connect by `socket_address`, since this can be too
/// long for a socket address.
pub(crate) fn socket_path(run_dir: &Path) -> PathBuf {
    run_dir.join(run_files::SOCKET)
}

/// The longest path a socket address holds. `sun_path` is 104 bytes on macOS and 108 on Linux,
/// and ends with a NUL.
const MAX_SOCKET_PATH: usize = 103;

/// The path to bind or connect the run's socket by. It's `socket_path` when that fits in a
/// socket address, else the same file reached through a link in `/tmp/argonauts-<uid>`, a
/// folder only you can open.
pub(crate) fn socket_address(run_dir: &Path) -> io::Result<PathBuf> {
    socket_address_in(run_dir, Path::new("/tmp"))
}

fn socket_address_in(run_dir: &Path, tmp: &Path) -> io::Result<PathBuf> {
    let direct = socket_path(run_dir);
    if fits(&direct) {
        return Ok(direct);
    }
    let (run_dir, link) = link_path(run_dir, tmp)?;
    ensure_private_dir(&link, geteuid().as_raw())?;
    point_link(&link, &run_dir)?;
    let short = link.join(run_files::SOCKET);
    if !fits(&short) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the control socket's path {} is longer than the {MAX_SOCKET_PATH} bytes a \
                 socket address holds",
                short.display()
            ),
        ));
    }
    Ok(short)
}

/// Removes the socket file, and the link `socket_address` made for it if there is one.
pub(crate) fn remove_socket(run_dir: &Path) -> io::Result<()> {
    remove_if_present(&socket_path(run_dir))?;
    if fits(&socket_path(run_dir)) {
        return Ok(());
    }
    let (run_dir, link) = link_path(run_dir, Path::new("/tmp"))?;
    if std::fs::read_link(&link).is_ok_and(|target| target == run_dir) {
        remove_if_present(&link)?;
    }
    Ok(())
}

fn fits(path: &Path) -> bool {
    path.as_os_str().len() <= MAX_SOCKET_PATH
}

/// The canonical run folder, and `<tmp>/argonauts-<uid>/<hash of it>` for this user. The name
/// comes from the run folder, so every process of yours that asks for this run finds the same
/// link.
fn link_path(run_dir: &Path, tmp: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let run_dir = std::fs::canonicalize(run_dir)?;
    let name = &sha256_hex(run_dir.as_os_str().as_bytes())[..16];
    let link = tmp.join(format!("argonauts-{}", geteuid())).join(name);
    Ok((run_dir, link))
}

/// Makes the link's folder with mode 0700, owned by `uid`. Anyone can write to /tmp, and a
/// folder someone else owns or could write to would let them point a run's link at their own
/// socket, which would then receive the approval token.
fn ensure_private_dir(link: &Path, uid: u32) -> io::Result<()> {
    let Some(dir) = link.parent() else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };
    match DirBuilder::new().mode(0o700).create(dir) {
        // The umask may have taken bits away, and a folder you can't open is no use either.
        Ok(()) => std::fs::set_permissions(dir, Permissions::from_mode(0o700))?,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    // Not followed, so a link someone left in its place is refused rather than used.
    let found = std::fs::symlink_metadata(dir)?;
    if !found.is_dir() || found.uid() != uid || found.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} must be a folder that you own and only you can open, so run sockets can't \
                 be redirected. Remove it and try again.",
                dir.display()
            ),
        ));
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

fn point_link(link: &Path, run_dir: &Path) -> io::Result<()> {
    let points_here = || std::fs::read_link(link).is_ok_and(|target| target == run_dir);
    if points_here() {
        return Ok(());
    }
    remove_if_present(link)?;
    match std::os::unix::fs::symlink(run_dir, link) {
        Ok(()) => Ok(()),
        // Another process made the same link first.
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && points_here() => Ok(()),
        Err(err) => Err(err),
    }
}

/// Appends one request for the next run host. The caller holds the run lock.
pub(crate) fn append_pending(run_dir: &Path, request: &ControlRequest) -> anyhow::Result<()> {
    let path = run_dir.join(run_files::PENDING_ANSWERS);
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(&line)
        .with_context(|| format!("writing {}", path.display()))
}

/// Where a run host keeps pending requests while it feeds them to its engine. Until they're
/// all in, a run host that crashes leaves them here for the next one.
const PENDING_TAKEN: &str = "pending-answers.taking";

/// The pending requests one run host took, oldest first.
#[derive(Debug)]
pub(crate) struct Pending {
    pub requests: Vec<ControlRequest>,
    /// Lines that didn't parse, such as a torn append or a frame from another version, each
    /// named by its file and line number.
    pub skipped: Vec<String>,
    files: Vec<PathBuf>,
}

impl Pending {
    /// Removes the taken files once their requests are in the engine's inbox.
    pub(crate) fn delivered(self) -> anyhow::Result<()> {
        for path in &self.files {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(err).with_context(|| format!("removing {}", path.display()));
                }
            }
        }
        Ok(())
    }
}

/// Sets the pending requests aside and reads them, leaving them on disk until `delivered`.
/// Requests an earlier run host set aside come first. The caller holds the run lock.
pub(crate) fn set_aside_pending(run_dir: &Path) -> anyhow::Result<Pending> {
    let fresh = run_dir.join(run_files::PENDING_ANSWERS);
    let taken = run_dir.join(PENDING_TAKEN);
    if !taken.exists() {
        match std::fs::rename(&fresh, &taken) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("setting aside {}", fresh.display()));
            }
        }
    }
    let mut pending = Pending {
        requests: Vec::new(),
        skipped: Vec::new(),
        files: Vec::new(),
    };
    for path in [taken, fresh] {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            if line.trim_ascii().is_empty() {
                continue;
            }
            // The error isn't kept: it can quote the line, and a line can hold the run's token.
            match serde_json::from_slice(line) {
                Ok(request) => pending.requests.push(request),
                Err(_) => pending
                    .skipped
                    .push(format!("line {} of {}", index + 1, path.display())),
            }
        }
        pending.files.push(path);
    }
    Ok(pending)
}

/// Every pending request, oldest first, and removes them.
#[cfg(test)]
pub(crate) fn take_pending(run_dir: &Path) -> anyhow::Result<Vec<ControlRequest>> {
    let pending = set_aside_pending(run_dir)?;
    let requests = pending.requests.clone();
    pending.delivered()?;
    Ok(requests)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write_private_file(path, &text).with_context(|| format!("writing {}", path.display()))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn guidance(text: &str) -> ControlRequest {
        ControlRequest::Guidance {
            text: text.to_owned(),
            outside_agent: None,
        }
    }

    #[test]
    fn pending_requests_come_back_in_order_once() {
        let dir = tempfile::tempdir().unwrap();
        append_pending(dir.path(), &guidance("use the v2 API")).unwrap();
        append_pending(dir.path(), &ControlRequest::Stop).unwrap();
        assert_eq!(
            take_pending(dir.path()).unwrap(),
            vec![guidance("use the v2 API"), ControlRequest::Stop]
        );
        assert!(take_pending(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn pending_requests_stay_on_disk_until_delivered() {
        let dir = tempfile::tempdir().unwrap();
        append_pending(dir.path(), &guidance("first")).unwrap();
        let crashed = set_aside_pending(dir.path()).unwrap();
        assert_eq!(crashed.requests, vec![guidance("first")]);
        drop(crashed);
        append_pending(dir.path(), &guidance("second")).unwrap();

        let next = set_aside_pending(dir.path()).unwrap();
        assert_eq!(next.requests, vec![guidance("first"), guidance("second")]);
        next.delivered().unwrap();
        assert!(set_aside_pending(dir.path()).unwrap().requests.is_empty());
        assert!(!dir.path().join(run_files::PENDING_ANSWERS).exists());
        assert!(!dir.path().join(PENDING_TAKEN).exists());
    }

    #[test]
    fn unreadable_pending_lines_are_skipped_and_named() {
        let dir = tempfile::tempdir().unwrap();
        append_pending(dir.path(), &guidance("kept")).unwrap();
        let path = dir.path().join(run_files::PENDING_ANSWERS);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"type\":\"from_a_later_version\"}\n")
            .unwrap();
        append_pending(dir.path(), &ControlRequest::Stop).unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"type\":\"gui").unwrap();

        let pending = set_aside_pending(dir.path()).unwrap();
        assert_eq!(
            pending.requests,
            vec![guidance("kept"), ControlRequest::Stop]
        );
        let taken = dir.path().join(PENDING_TAKEN);
        assert_eq!(
            pending.skipped,
            vec![
                format!("line 2 of {}", taken.display()),
                format!("line 4 of {}", taken.display()),
            ]
        );
    }

    #[test]
    fn a_short_run_dir_binds_in_place() {
        let dir = tempfile::Builder::new()
            .prefix("ah")
            .tempdir_in("/tmp")
            .unwrap();
        assert_eq!(
            socket_address_in(dir.path(), dir.path()).unwrap(),
            socket_path(dir.path())
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    fn long_run_dir(dir: &Path) -> PathBuf {
        let run_dir = dir
            .join("state")
            .join("a-repository-folder-with-a-long-name-0123-5f0c2a9e4b71")
            .join("runs")
            .join("run-20260930-010000-0001");
        std::fs::create_dir_all(&run_dir).unwrap();
        assert!(socket_path(&run_dir).as_os_str().len() > MAX_SOCKET_PATH);
        run_dir
    }

    #[test]
    fn a_long_run_dir_is_reached_through_a_private_link() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = long_run_dir(dir.path());
        let tmp = tempfile::Builder::new()
            .prefix("ah")
            .tempdir_in("/tmp")
            .unwrap();

        let address = socket_address_in(&run_dir, tmp.path()).unwrap();
        assert!(address.as_os_str().len() <= MAX_SOCKET_PATH);
        assert_eq!(address.file_name(), socket_path(&run_dir).file_name());
        let link = address.parent().unwrap();
        assert_eq!(
            std::fs::read_link(link).unwrap(),
            std::fs::canonicalize(&run_dir).unwrap()
        );
        let links = std::fs::metadata(link.parent().unwrap()).unwrap();
        assert_eq!(links.mode() & 0o777, 0o700);

        // A stale link under the same name is pointed back at the run.
        std::fs::remove_file(link).unwrap();
        std::os::unix::fs::symlink(dir.path(), link).unwrap();
        assert_eq!(socket_address_in(&run_dir, tmp.path()).unwrap(), address);
        assert_eq!(
            std::fs::read_link(link).unwrap(),
            std::fs::canonicalize(&run_dir).unwrap()
        );
    }

    #[test]
    fn a_links_folder_others_can_open_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = long_run_dir(dir.path());
        let tmp = tempfile::Builder::new()
            .prefix("ah")
            .tempdir_in("/tmp")
            .unwrap();
        let links = tmp.path().join(format!("argonauts-{}", geteuid()));
        DirBuilder::new().mode(0o755).create(&links).unwrap();
        std::fs::set_permissions(&links, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = socket_address_in(&run_dir, tmp.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("only you can open"), "{err}");
    }

    #[test]
    fn a_links_folder_someone_else_owns_is_refused() {
        let tmp = tempfile::Builder::new()
            .prefix("ah")
            .tempdir_in("/tmp")
            .unwrap();
        let link = tmp.path().join("argonauts-x").join("0123456789abcdef");
        let mine = geteuid().as_raw();
        ensure_private_dir(&link, mine).unwrap();
        let folder = std::fs::metadata(link.parent().unwrap()).unwrap();
        assert_eq!(folder.mode() & 0o777, 0o700);
        assert_eq!(folder.uid(), mine);

        let err = ensure_private_dir(&link, mine + 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("you own"), "{err}");
    }

    #[test]
    fn a_link_in_place_of_the_links_folder_is_refused() {
        let tmp = tempfile::Builder::new()
            .prefix("ah")
            .tempdir_in("/tmp")
            .unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        DirBuilder::new().mode(0o700).create(&elsewhere).unwrap();
        let folder = tmp.path().join("argonauts-x");
        std::os::unix::fs::symlink(&elsewhere, &folder).unwrap();
        let err =
            ensure_private_dir(&folder.join("0123456789abcdef"), geteuid().as_raw()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_missing_run_dir_reads_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = long_run_dir(dir.path());
        std::fs::remove_dir(&run_dir).unwrap();
        let err = socket_address_in(&run_dir, dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn tokens_are_long_random_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let token = new_token().unwrap();
        assert_eq!(token.len(), 64);
        assert_ne!(token, new_token().unwrap());
        write_token(dir.path(), &token).unwrap();
        assert_eq!(read_token(dir.path()).unwrap(), token);
        let mode = std::fs::metadata(dir.path().join(run_files::APPROVE_TOKEN))
            .unwrap()
            .permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o077, 0);
    }
}
