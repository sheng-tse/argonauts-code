//! Putting a release in place. Each version lives in its own folder,
//! `~/.local/share/argonauts/versions/<version>/argonauts`, and `~/.local/bin/argonauts` is a
//! link to one of them, swapped in a single rename under a lock. Overwriting the program in
//! place isn't safe: agent sessions start Argonauts Code's own helper from it, so an app still
//! open would get a newer helper. With the link, every open session keeps its version, and the
//! next start takes the new one.
//!
//! A release is checked before the swap: its archive's size and sha256 against the signed
//! manifest, then the program in it must run and print its own version. The version switched
//! from stays for `--rollback`, and so does the one running now.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::fcntl::{Flock, FlockArg};
use semver::Version;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::manifest::{Download, Release};
use super::{DISABLE_ENV, Here, shown};

/// The program's name in each version's folder and in a release archive.
const PROGRAM: &str = "argonauts";
/// Names the version `--rollback` goes back to, in the versions folder.
const PREVIOUS: &str = "previous";

/// In each version's folder: every process that runs that version holds it shared for as long
/// as it runs, so pruning, which needs it alone, leaves a version in use where it is.
const RUNNING: &str = ".running";
/// How long a release archive may take to download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// How long the new program may take to print its version.
const RUN_TIMEOUT: Duration = Duration::from_secs(15);

/// Where versions are kept and the link the shell finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Your home folder, for naming places with `~`.
    pub home: PathBuf,
    /// A folder for each version.
    pub versions: PathBuf,
    /// The link to the version that starts.
    pub link: PathBuf,
}

impl Layout {
    /// Under `home`, as dist's installer places the program: `$XDG_DATA_HOME` and
    /// `$XDG_BIN_HOME` when set, else `~/.local/share` and `~/.local/bin`.
    pub fn under(home: &Path, data_home: Option<PathBuf>, bin_home: Option<PathBuf>) -> Self {
        let data = data_home.unwrap_or_else(|| home.join(".local/share"));
        let bin = bin_home.unwrap_or_else(|| home.join(".local/bin"));
        Self {
            home: home.to_path_buf(),
            versions: data.join("argonauts/versions"),
            link: bin.join(PROGRAM),
        }
    }

    /// This process's, from `HOME` and the XDG folders. A test's lives inside its own
    /// Argonauts home, never your `~/.local`.
    pub fn for_home(paths: &argonauts_core::Paths) -> Self {
        if cfg!(test) {
            return Self::under(&paths.home().join("you"), None, None);
        }
        let absolute = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
        };
        let home = absolute("HOME").unwrap_or_else(|| PathBuf::from("/"));
        Self::under(&home, absolute("XDG_DATA_HOME"), absolute("XDG_BIN_HOME"))
    }

    /// With the program dist's installer put on your PATH, in the folder its receipt names, as
    /// the link: an update swaps that one, not the default the installer may not have used, as
    /// with only `XDG_DATA_HOME` set elsewhere. Without one there, the default stays.
    pub fn linked_where_installed(mut self, receipt_prefix: Option<&Path>) -> Self {
        if let Some(prefix) = receipt_prefix {
            let installed = [prefix.join("bin").join(PROGRAM), prefix.join(PROGRAM)]
                .into_iter()
                .find(|program| program.symlink_metadata().is_ok());
            if let Some(installed) = installed {
                self.link = installed;
            }
        }
        self
    }

    /// Whether `folder` is one of the kept versions' folders.
    fn holds(&self, folder: &Path) -> bool {
        let versions = fs::canonicalize(&self.versions).unwrap_or_else(|_| self.versions.clone());
        let parent = folder
            .parent()
            .map(|parent| fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf()));
        parent == Some(versions)
    }

    fn folder(&self, version: &Version) -> PathBuf {
        self.versions.join(version.to_string())
    }

    /// The link as the screen names it, such as `~/.local/bin/argonauts`.
    pub fn shown_link(&self) -> String {
        shown(&self.link, &self.home)
    }

    /// The version the link starts, when it is a link into the versions folder.
    pub fn linked(&self) -> Option<Version> {
        let target = fs::read_link(&self.link).ok()?;
        let folder = target.parent()?;
        if folder.parent()? != self.versions {
            return None;
        }
        Version::parse(&folder.file_name()?.to_string_lossy()).ok()
    }

    /// The version `--rollback` goes back to, while its folder is still there.
    pub fn previous(&self) -> Option<Version> {
        let text = fs::read_to_string(self.versions.join(PREVIOUS)).ok()?;
        let version = Version::parse(text.trim()).ok()?;
        self.folder(&version)
            .join(PROGRAM)
            .is_file()
            .then_some(version)
    }
}

/// Each step of putting a release in place, as it's done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Downloaded {
        file: String,
        size: u64,
    },
    /// The new program ran and printed this, such as `argonauts-code 0.1.1`.
    Checked {
        said: String,
    },
    Switched(Switched),
}

/// The link moved: from the version it started before, to `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switched {
    pub link: String,
    pub to: Version,
    /// The version kept for `--rollback`.
    pub kept: Option<Version>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("couldn't download {file}: {why}")]
    Download { file: String, why: String },
    #[error("the download is {got} bytes, not the {want} its manifest says")]
    Size { want: u64, got: u64 },
    #[error("the download's sha256 doesn't match its manifest")]
    Checksum,
    #[error("couldn't unpack it: {0}")]
    Unpack(String),
    #[error("the archive holds no {PROGRAM} program")]
    NoProgram,
    #[error("the new program doesn't run: {0}")]
    DoesntRun(String),
    #[error("couldn't put it in place: {0}")]
    Place(String),
    #[error("no earlier version is kept to go back to")]
    NoPrevious,
    #[error("it was stopped before it took over")]
    Stopped,
}

impl From<io::Error> for ApplyError {
    fn from(err: io::Error) -> Self {
        Self::Place(err.to_string())
    }
}

/// Downloads `release`, checks it, and switches the link to it, telling `step` as each part is
/// done. On any failure the link and the versions already kept stay as they were.
pub async fn take(
    here: &Here,
    release: &Release,
    step: &mut (dyn FnMut(Step) + Send),
    stop: &CancellationToken,
) -> Result<Switched, ApplyError> {
    if stop.is_cancelled() {
        return Err(ApplyError::Stopped);
    }
    let archive = tokio::select! {
        archive = download(&release.download) => archive?,
        () = stop.cancelled() => return Err(ApplyError::Stopped),
    };
    step(Step::Downloaded {
        file: release.download.file_name().to_owned(),
        size: release.download.size,
    });
    let layout = here.layout.clone();
    fs::create_dir_all(&layout.versions)?;
    sweep_staged(&layout);
    let incoming = layout.versions.join(format!(
        "{STAGED}{}-{}",
        release.version,
        std::process::id()
    ));
    // From here on in a task of its own, which a caller that stops waiting can't cut short: it
    // never leaves a staged copy behind, and it hears the stop only before the switch.
    let placing = Placing {
        layout,
        archive,
        incoming: incoming.clone(),
        version: release.version.clone(),
        running: here.current.clone(),
        stop: stop.clone(),
    };
    let placed = tokio::spawn(async move {
        let placed = placing.place().await;
        if incoming.exists() {
            let _ = fs::remove_dir_all(&incoming);
        }
        placed
    })
    .await
    .map_err(|err| ApplyError::Place(err.to_string()))?;
    let (said, switched) = placed?;
    step(Step::Checked { said });
    step(Step::Switched(switched.clone()));
    Ok(switched)
}

/// What a staged copy's folder name starts with, before its version and the process's id.
const STAGED: &str = ".incoming-";

/// A release unpacked into `incoming`, checked, and switched to.
struct Placing {
    layout: Layout,
    archive: Vec<u8>,
    incoming: PathBuf,
    version: Version,
    running: Version,
    stop: CancellationToken,
}

impl Placing {
    async fn place(self) -> Result<(String, Switched), ApplyError> {
        let Self {
            layout,
            archive,
            incoming,
            version,
            running,
            stop,
        } = self;
        let into = incoming.clone();
        let program = tokio::task::spawn_blocking(move || unpack(&archive, &into))
            .await
            .map_err(|err| ApplyError::Unpack(err.to_string()))??;
        let said = runs(&program, &version).await?;
        if stop.is_cancelled() {
            return Err(ApplyError::Stopped);
        }
        let switched =
            tokio::task::spawn_blocking(move || switch_to(&layout, &incoming, &version, &running))
                .await
                .map_err(|err| ApplyError::Place(err.to_string()))??;
        Ok((said, switched))
    }
}

/// Removes the staged copies of processes that are gone, as one Ctrl+C ended mid-update.
fn sweep_staged(layout: &Layout) {
    let Ok(entries) = fs::read_dir(&layout.versions) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|name| name.strip_prefix(STAGED)) else {
            continue;
        };
        let pid = rest
            .rsplit_once('-')
            .and_then(|(_, pid)| pid.parse::<i32>().ok());
        if pid.is_some_and(|pid| !alive(pid)) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether a process `pid` is there, yours or not.
fn alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    !matches!(
        kill(Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

/// The archive, once its size and sha256 match what the manifest says.
async fn download(download: &Download) -> Result<Vec<u8>, ApplyError> {
    let failed = |why: String| ApplyError::Download {
        file: download.file_name().to_owned(),
        why,
    };
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(concat!("argonauts/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| failed(err.to_string()))?;
    let mut response = client
        .get(&download.url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|err| failed(err.without_url().to_string()))?;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| failed(err.without_url().to_string()))?
    {
        body.extend_from_slice(&chunk);
        // A server sending more than the manifest says is stopped there.
        if body.len() as u64 > download.size {
            break;
        }
    }
    if body.len() as u64 != download.size {
        return Err(ApplyError::Size {
            want: download.size,
            got: body.len() as u64,
        });
    }
    if !hex(&Sha256::digest(&body)).eq_ignore_ascii_case(download.sha256.trim()) {
        return Err(ApplyError::Checksum);
    }
    Ok(body)
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Takes the program out of a `.tar.xz` release archive into `into`, and nothing else in it,
/// so no path in the archive can reach outside.
fn unpack(archive: &[u8], into: &Path) -> Result<PathBuf, ApplyError> {
    let unpack_error = |err: io::Error| ApplyError::Unpack(err.to_string());
    let mut tar = tar::Archive::new(xz2::read::XzDecoder::new(archive));
    for entry in tar.entries().map_err(unpack_error)? {
        let mut entry = entry.map_err(unpack_error)?;
        let named = entry
            .path()
            .map_err(unpack_error)?
            .file_name()
            .is_some_and(|name| name == PROGRAM);
        if !named || !entry.header().entry_type().is_file() {
            continue;
        }
        fs::create_dir_all(into)?;
        let program = into.join(PROGRAM);
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(unpack_error)?;
        fs::write(&program, bytes)?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        return Ok(program);
    }
    Err(ApplyError::NoProgram)
}

/// The first line the program at `program` prints for `--version`, once it names `version`.
async fn runs(program: &Path, version: &Version) -> Result<String, ApplyError> {
    let output = tokio::time::timeout(
        RUN_TIMEOUT,
        tokio::process::Command::new(program)
            .arg("--version")
            .env(DISABLE_ENV, "1")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ApplyError::DoesntRun("it didn't answer --version".to_owned()))?
    .map_err(|err| ApplyError::DoesntRun(err.to_string()))?;
    let said = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !output.status.success() {
        return Err(ApplyError::DoesntRun(format!(
            "--version {}",
            output.status
        )));
    }
    if !said
        .split_whitespace()
        .any(|word| word == version.to_string())
    {
        return Err(ApplyError::DoesntRun(format!(
            "it says {said:?}, not {version}"
        )));
    }
    Ok(said)
}

/// The versions folder held for this process alone while the link moves. Another process
/// switching at the same time waits here.
fn lock(versions: &Path) -> io::Result<Flock<File>> {
    fs::create_dir_all(versions)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(versions.join(".lock"))?;
    Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, errno)| io::Error::from(errno))
}

/// Moves the program staged in `staged` to its version's folder and the link to it, keeping
/// the version the link started before and the one running now.
fn switch_to(
    layout: &Layout,
    staged: &Path,
    version: &Version,
    running: &Version,
) -> Result<Switched, ApplyError> {
    let _held = lock(&layout.versions)?;
    let folder = layout.folder(version);
    if folder.exists() {
        fs::remove_dir_all(&folder)?;
    }
    fs::rename(staged, &folder)?;
    let before = adopt(layout, running)?;
    point(&layout.link, &folder.join(PROGRAM))?;
    let kept = before.filter(|before| before != version);
    if let Some(kept) = &kept {
        fs::write(layout.versions.join(PREVIOUS), format!("{kept}\n"))?;
    }
    let keep: BTreeSet<Version> = [Some(version.clone()), kept.clone(), Some(running.clone())]
        .into_iter()
        .flatten()
        .collect();
    prune(layout, &keep);
    Ok(Switched {
        link: layout.shown_link(),
        to: version.clone(),
        kept: kept.or_else(|| layout.previous()),
    })
}

/// The version the link starts now. dist's installer wrote the program itself where the link
/// goes, so the first switch moves that program into the running version's folder.
fn adopt(layout: &Layout, running: &Version) -> io::Result<Option<Version>> {
    let Ok(meta) = fs::symlink_metadata(&layout.link) else {
        return Ok(None);
    };
    if meta.file_type().is_symlink() {
        return Ok(layout.linked());
    }
    let folder = layout.folder(running);
    fs::create_dir_all(&folder)?;
    let program = folder.join(PROGRAM);
    if fs::rename(&layout.link, &program).is_err() {
        // Another disk: copy, then leave the link's place free for the link.
        fs::copy(&layout.link, &program)?;
        fs::remove_file(&layout.link)?;
    }
    Ok(Some(running.clone()))
}

/// Points `link` at `program` in one rename, so the shell finds the old version or the new
/// one and never neither.
fn point(link: &Path, program: &Path) -> io::Result<()> {
    if let Some(dir) = link.parent() {
        fs::create_dir_all(dir)?;
    }
    let partial = link.with_file_name(format!(".{PROGRAM}.{}", std::process::id()));
    let _ = fs::remove_file(&partial);
    std::os::unix::fs::symlink(program, &partial)?;
    fs::rename(&partial, link)
}

/// Removes the versions not in `keep` that no process runs now. One that can't be removed now
/// stays for next time.
fn prune(layout: &Layout, keep: &BTreeSet<Version>) {
    let Ok(entries) = fs::read_dir(&layout.versions) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(version) = Version::parse(&name.to_string_lossy()) else {
            continue;
        };
        if keep.contains(&version) || !entry.path().is_dir() {
            continue;
        }
        // Held alone while it goes, so no process starts on it meanwhile and finds it gone.
        let Some(_alone) = unused(&entry.path()) else {
            continue;
        };
        let _ = fs::remove_dir_all(entry.path());
    }
}

/// The version in `folder`, held alone, when no process runs it.
fn unused(folder: &Path) -> Option<File> {
    let mark = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join(RUNNING))
        .ok()?;
    mark.try_lock().ok().map(|()| mark)
}

/// Marks the version this process runs as in use for as long as it runs, so no update prunes it
/// from under it or the helpers it starts. A program that isn't one of the kept versions, as
/// one Homebrew or cargo installed, has nothing to mark.
pub fn hold_this_version(layout: &Layout) {
    static HELD: std::sync::OnceLock<File> = std::sync::OnceLock::new();
    let Ok(program) = argonauts_core::paths::this_program() else {
        return;
    };
    let Some(folder) = program.parent().filter(|folder| layout.holds(folder)) else {
        return;
    };
    let Ok(mark) = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join(RUNNING))
    else {
        return;
    };
    if mark.lock_shared().is_ok() {
        let _ = HELD.set(mark);
    }
}

/// Where something outside, such as an agent's config, starts this program later: the link an
/// update keeps at the newest version, when this program is one of the kept versions, else the
/// path it was started by. Never a version's own file, which a later update removes.
pub fn program_to_save(layout: &Layout) -> io::Result<PathBuf> {
    let program = argonauts_core::paths::this_program()?;
    Ok(
        match program.parent().filter(|folder| layout.holds(folder)) {
            Some(_) => layout.link.clone(),
            None => std::env::current_exe()?,
        },
    )
}

/// Switches the link back to the version kept for `--rollback`, keeping the one it leaves for
/// the next `--rollback`.
pub fn roll_back(layout: &Layout) -> Result<Switched, ApplyError> {
    let _held = lock(&layout.versions)?;
    let previous = layout.previous().ok_or(ApplyError::NoPrevious)?;
    let now = layout.linked();
    point(&layout.link, &layout.folder(&previous).join(PROGRAM))?;
    match &now {
        Some(now) => fs::write(layout.versions.join(PREVIOUS), format!("{now}\n"))?,
        None => {
            let _ = fs::remove_file(layout.versions.join(PREVIOUS));
        }
    }
    Ok(Switched {
        link: layout.shown_link(),
        to: previous,
        kept: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::test_server::{FakeServer, release_archive};
    use crate::self_update::{Install, Source};

    /// A version another process runs stays where it is, and goes once that process ends.
    #[test]
    fn a_version_in_use_is_never_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::under(dir.path(), None, None);
        let old = layout.folder(&v("0.1.0"));
        fs::create_dir_all(&old).unwrap();
        let running = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(old.join(RUNNING))
            .unwrap();
        running.lock_shared().unwrap();
        prune(&layout, &BTreeSet::new());
        assert!(old.exists(), "pruned while in use");
        drop(running);
        // A child another test forks holds a copy of the lock until its exec, a few
        // milliseconds, so the version goes at the first prune once nothing holds it.
        let started = std::time::Instant::now();
        while old.exists() && started.elapsed() < Duration::from_secs(10) {
            prune(&layout, &BTreeSet::new());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!old.exists());
    }

    #[test]
    fn the_link_is_the_program_the_receipt_says_was_installed() {
        let dir = tempfile::tempdir().unwrap();
        let layout = || Layout::under(&dir.path().join("home"), None, None);
        let prefix = dir.path().join("data");
        // Nothing installed there: the default stays.
        assert_eq!(
            layout().linked_where_installed(Some(&prefix)).link,
            layout().link
        );
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(prefix.join("bin").join(PROGRAM), "the program").unwrap();
        assert_eq!(
            layout().linked_where_installed(Some(&prefix)).link,
            prefix.join("bin").join(PROGRAM)
        );
        assert_eq!(layout().linked_where_installed(None).link, layout().link);
    }

    #[test]
    fn a_kept_versions_folder_is_known_by_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::under(dir.path(), None, None);
        let folder = layout.folder(&v("0.1.2"));
        fs::create_dir_all(&folder).unwrap();
        assert!(layout.holds(&folder));
        assert!(!layout.holds(dir.path()));
        assert!(!layout.holds(&dir.path().join("elsewhere/0.1.2")));
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn here(root: &Path, running: &str) -> Here {
        Here {
            source: Source::new("http://127.0.0.1:1", Vec::new()),
            layout: Layout::under(&root.join("home"), None, None),
            install: Install::Shell,
            state_file: root.join("argonauts/update.json"),
            current: v(running),
            platform: Some("aarch64-apple-darwin"),
        }
    }

    async fn served(archive: Vec<u8>, version: &str) -> (FakeServer, Release) {
        let server = FakeServer::start().await;
        let download = server.release(version, archive);
        let release = Release {
            version: v(version),
            download,
            required: false,
        };
        (server, release)
    }

    /// The program dist's installer left in `~/.local/bin`, for the first switch to move.
    fn installed_plainly(here: &Here) {
        fs::create_dir_all(here.layout.link.parent().unwrap()).unwrap();
        argonauts_testkit::write_script(
            &here.layout.link,
            "#!/bin/sh\necho argonauts-code 0.1.0\n",
        )
        .unwrap();
    }

    #[tokio::test]
    async fn a_release_is_checked_then_linked_and_the_installers_copy_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let here = here(dir.path(), "0.1.0");
        installed_plainly(&here);
        let (_server, release) = served(release_archive("0.1.1"), "0.1.1").await;
        let mut steps = Vec::new();
        let switched = take(
            &here,
            &release,
            &mut |step| steps.push(step),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            steps[..2],
            [
                Step::Downloaded {
                    file: "argonauts-aarch64-apple-darwin.tar.xz".to_owned(),
                    size: release.download.size,
                },
                Step::Checked {
                    said: "argonauts-code 0.1.1".to_owned()
                },
            ]
        );
        assert_eq!(switched.link, "~/.local/bin/argonauts");
        assert_eq!(switched.to, v("0.1.1"));
        assert_eq!(switched.kept, Some(v("0.1.0")));
        let layout = &here.layout;
        assert_eq!(layout.linked(), Some(v("0.1.1")));
        assert_eq!(layout.previous(), Some(v("0.1.0")));
        assert!(layout.folder(&v("0.1.0")).join(PROGRAM).is_file());
        let staged_left = fs::read_dir(&layout.versions)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".incoming"));
        assert!(!staged_left, "nothing staged is left");
    }

    fn staged_left(layout: &Layout) -> bool {
        fs::read_dir(&layout.versions)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(STAGED))
    }

    /// A stop that comes once the download is done is heard before the switch: the old version
    /// keeps starting, and nothing staged is left, since the rest runs to its end on its own.
    #[tokio::test]
    async fn a_stop_after_the_download_switches_nothing_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let here = here(dir.path(), "0.1.0");
        installed_plainly(&here);
        let (_server, release) = served(release_archive("0.1.1"), "0.1.1").await;
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let taken = take(
            &here,
            &release,
            &mut |step| {
                if matches!(step, Step::Downloaded { .. }) {
                    stopping.cancel();
                }
            },
            &stop,
        )
        .await;
        assert_eq!(taken, Err(ApplyError::Stopped));
        assert_eq!(
            here.layout.linked(),
            None,
            "the installer's copy still starts"
        );
        assert!(!staged_left(&here.layout));
    }

    /// What a process that's gone staged, as one a Ctrl+C ended mid-update, goes with the next.
    #[tokio::test]
    async fn a_gone_processs_staged_copy_is_swept() {
        let dir = tempfile::tempdir().unwrap();
        let here = here(dir.path(), "0.1.0");
        installed_plainly(&here);
        let mut ended = std::process::Command::new("true").spawn().unwrap();
        let gone = ended.id();
        ended.wait().unwrap();
        let left = here.layout.versions.join(format!("{STAGED}0.1.1-{gone}"));
        fs::create_dir_all(&left).unwrap();
        let ours = here
            .layout
            .versions
            .join(format!("{STAGED}0.1.1-{}", std::process::id()));
        let (_server, release) = served(release_archive("0.1.1"), "0.1.1").await;
        take(&here, &release, &mut |_| {}, &CancellationToken::new())
            .await
            .unwrap();
        assert!(!left.exists(), "the gone process's copy was swept");
        assert!(!ours.exists());
    }

    #[tokio::test]
    async fn a_bad_checksum_or_size_or_program_leaves_the_old_version_starting() {
        let dir = tempfile::tempdir().unwrap();
        let here = here(dir.path(), "0.1.0");
        installed_plainly(&here);
        let (_server, mut release) = served(release_archive("0.1.1"), "0.1.1").await;
        release.download.sha256 = "0".repeat(64);
        assert_eq!(
            take(&here, &release, &mut |_| {}, &CancellationToken::new()).await,
            Err(ApplyError::Checksum)
        );
        release.download.size += 1;
        assert!(matches!(
            take(&here, &release, &mut |_| {}, &CancellationToken::new()).await,
            Err(ApplyError::Size { .. })
        ));
        // A program that says another version isn't the release.
        let (_server, release) = served(release_archive("0.1.2"), "0.1.1").await;
        assert!(matches!(
            take(&here, &release, &mut |_| {}, &CancellationToken::new()).await,
            Err(ApplyError::DoesntRun(_))
        ));
        let meta = fs::symlink_metadata(&here.layout.link).unwrap();
        assert!(meta.is_file(), "the installer's program is still there");
        assert!(!here.layout.folder(&v("0.1.1")).exists());
    }

    #[tokio::test]
    async fn rollback_goes_back_and_comes_forward_again() {
        let dir = tempfile::tempdir().unwrap();
        let here = here(dir.path(), "0.1.0");
        installed_plainly(&here);
        let (_server, release) = served(release_archive("0.1.1"), "0.1.1").await;
        take(&here, &release, &mut |_| {}, &CancellationToken::new())
            .await
            .unwrap();
        let back = roll_back(&here.layout).unwrap();
        assert_eq!((back.to, back.kept), (v("0.1.0"), Some(v("0.1.1"))));
        assert_eq!(here.layout.linked(), Some(v("0.1.0")));
        let forward = roll_back(&here.layout).unwrap();
        assert_eq!(forward.to, v("0.1.1"));
        let fresh = Layout::under(&dir.path().join("elsewhere"), None, None);
        assert_eq!(roll_back(&fresh), Err(ApplyError::NoPrevious));
    }

    /// Only the version switched to, the one before it and the one running stay.
    #[tokio::test]
    async fn older_versions_go_once_a_newer_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let here = here(dir.path(), "0.1.1");
        for old in ["0.0.9", "0.1.0", "0.1.1"] {
            let folder = here.layout.folder(&v(old));
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join(PROGRAM), "").unwrap();
        }
        point(
            &here.layout.link,
            &here.layout.folder(&v("0.1.0")).join(PROGRAM),
        )
        .unwrap();
        let (_server, release) = served(release_archive("0.1.2"), "0.1.2").await;
        take(&here, &release, &mut |_| {}, &CancellationToken::new())
            .await
            .unwrap();
        let mut left: Vec<String> = fs::read_dir(&here.layout.versions)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.') && name != PREVIOUS)
            .collect();
        left.sort();
        assert_eq!(left, ["0.1.0", "0.1.1", "0.1.2"]);
    }

    /// A second process holding the lock keeps the link where it is until it lets go.
    #[test]
    fn the_link_moves_only_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::under(&dir.path().join("home"), None, None);
        fs::create_dir_all(&layout.versions).unwrap();
        let mut holder = std::process::Command::new("perl")
            .arg("-e")
            .arg(
                "use Fcntl ':flock'; open(my $f, '>>', $ARGV[0]) or die; flock($f, LOCK_EX) \
                 or die; $| = 1; print \"held\\n\"; sleep 1",
            )
            .arg(layout.versions.join(".lock"))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut held = [0u8; 5];
        holder.stdout.take().unwrap().read_exact(&mut held).unwrap();
        assert_eq!(&held, b"held\n");
        let staged = layout.versions.join(".incoming-0.1.1");
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join(PROGRAM), "").unwrap();
        let started = std::time::Instant::now();
        let switched = switch_to(&layout, &staged, &v("0.1.1"), &v("0.1.0")).unwrap();
        let waited = started.elapsed();
        let status = holder.wait().unwrap();
        assert!(status.success());
        assert!(
            waited >= Duration::from_millis(500),
            "the switch waited for the other process: {waited:?}"
        );
        assert_eq!(switched.to, v("0.1.1"));
        assert_eq!(layout.linked(), Some(v("0.1.1")));
    }
}
