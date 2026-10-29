//! What a session may reach outside its project, and the never-list that holds wherever it
//! may. Every path here is absolute and free of `..`, and a path whose symlinks point elsewhere
//! is kept by its real path too, so neither a link nor `../` reaches into an entry.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::AgentEvent;
use crate::session::SessionRole;
use crate::settings::approvals::ApprovalMode;
use crate::settings::outside::OutsideSettings;

/// Every program reads these, so a never entry in one only keeps it from being written.
pub const SYSTEM_FOLDERS: &[&str] = &[
    "/System", "/usr", "/etc", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32",
];

/// Written as anywhere else, though a system folder holds it.
const WRITTEN_IN_SYSTEM: &[&str] = &["/usr/local"];

/// What a session does outside its project, without asking or once asked. The default reaches
/// nothing there, as before `[outside]` existed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outside {
    /// Reads anywhere without asking.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read: bool,
    /// Writes anywhere without asking: outside the project in place, and inside it only in the
    /// session's own folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub write: bool,
    /// Reads and writes anywhere once asked, for a direct turn whose CLI asks before it acts:
    /// the OS sandbox opens there for it as for `write`, and its asking holds it, so what it
    /// asks about still waits for you. A session that can't ask, such as a run's, reads there
    /// and writes only its project.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub after_asking: bool,
    /// Your checkout. A session working in a worktree of it writes there only through that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Never::is_empty")]
    pub never: Never,
}

impl Outside {
    /// What `[outside]` gives a session in `mode`. The never-list comes along only where the
    /// session may work outside: elsewhere what holds is what held before.
    pub fn for_mode(
        settings: &OutsideSettings,
        mode: ApprovalMode,
        project: Option<&Path>,
        home: &Path,
    ) -> Self {
        let read = settings.reads_in(mode);
        let write = settings.writes_in(mode);
        let after_asking = settings.asks_in(mode);
        if !read && !write && !after_asking {
            return Self::default();
        }
        Self {
            read,
            write,
            after_asking,
            project: project.map(normal),
            never: Never::resolve(&settings.never, home),
        }
    }

    pub fn is_open(&self) -> bool {
        self.read || self.write || self.after_asking
    }

    /// Whether a session in `role` may write outside the project at all, asked or not: what
    /// the OS sandbox opens. Once asked counts only for a direct turn, whose asking holds it.
    pub fn writes_outside(&self, role: SessionRole) -> bool {
        match role {
            SessionRole::ReadOnly => false,
            SessionRole::Direct => self.write || self.after_asking,
            SessionRole::Build => self.write,
        }
    }

    /// Whether the CLI's own permissions let a session in `role` read outside without asking.
    /// A direct turn's asking holds it where the mode opens outside only once asked.
    pub fn reads_unasked(&self, role: SessionRole) -> bool {
        self.read || (self.after_asking && role != SessionRole::Direct)
    }

    /// Whether the CLI's own permissions let a session in `role` write outside without asking.
    pub fn writes_unasked(&self, role: SessionRole) -> bool {
        role != SessionRole::ReadOnly && self.write
    }

    /// For a session whose CLI can't ask before it writes: where the mode opens outside only
    /// once asked, it reads there and writes only its project.
    pub fn without_asking(self) -> Self {
        if !self.after_asking {
            return self;
        }
        Self {
            read: true,
            after_asking: false,
            ..self
        }
    }

    pub fn is_closed(&self) -> bool {
        *self == Self::default()
    }

    /// Whether `path` lies outside the project. With no project, every path does.
    pub fn is_outside(&self, path: &Path) -> bool {
        let Some(project) = &self.project else {
            return true;
        };
        let project = forms(project);
        !forms(path)
            .iter()
            .any(|form| project.iter().any(|root| form.starts_with(root)))
    }
}

/// Where no agent reaches, resolved from `[outside] never`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Never {
    /// Folders and files never read or written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathBuf>,
    /// File names never read or written in any folder, such as `.env.*`, where `*` stands for
    /// any part of a name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    /// Names taken back out of a name with `*` in `names`, such as `.env.example` out of
    /// `.env.*`: a template a project commits. A name listed plainly stays closed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub except: Vec<String>,
    /// System folders, read but never written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unwritten: Vec<PathBuf>,
}

impl Never {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.names.is_empty() && self.unwritten.is_empty()
    }

    /// `entries` as `[outside] never` holds them: `~/` is `home`, a name without a `/` is matched
    /// in every folder, `!` before a name takes it back out of a name with `*`, and a relative
    /// path, which could mean any folder, is left out.
    pub fn resolve(entries: &[String], home: &Path) -> Self {
        let mut never = Self::default();
        for entry in entries.iter().map(|entry| entry.trim()) {
            if entry.is_empty() || entry.chars().any(char::is_control) {
                continue;
            }
            if let Some(name) = entry.strip_prefix('!') {
                if !name.is_empty() && !name.contains('/') {
                    push_unique(&mut never.except, name.to_owned());
                }
                continue;
            }
            if !entry.contains('/') {
                push_unique(&mut never.names, entry.to_owned());
                continue;
            }
            let path = match entry.strip_prefix("~/") {
                Some(rest) => home.join(rest),
                None if entry == "~" => home.to_path_buf(),
                None => PathBuf::from(entry),
            };
            if !path.is_absolute() {
                continue;
            }
            let path = normal(&path);
            if in_system_folder(&path) {
                for kept in written_around(&path) {
                    for form in forms(&kept) {
                        push_unique(&mut never.unwritten, form);
                    }
                }
            } else {
                for form in forms(&path) {
                    push_unique(&mut never.paths, form);
                }
            }
        }
        never
    }

    /// Whether `path`, by the way it's written or by its real path, is one no agent opens.
    pub fn refuses_read(&self, path: &Path) -> bool {
        forms(path).iter().any(|form| {
            self.paths.iter().any(|never| form.starts_with(never))
                || form
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| self.holds_name(name))
        })
    }

    /// Whether a file called `name` is never opened in any folder: a name in `names` matches it,
    /// and no name in `except` takes it back, which only a name with `*` gives up. macOS opens
    /// `.ENV` as `.env`, so there a name matches in any case.
    pub fn holds_name(&self, name: &str) -> bool {
        let matches = |glob: &str| match cfg!(target_os = "macos") {
            true => name_matches(&glob.to_lowercase(), &name.to_lowercase()),
            false => name_matches(glob, name),
        };
        let listed = self.names.iter().filter(|glob| matches(glob));
        let mut by_glob = false;
        for glob in listed {
            if !glob.contains('*') {
                return true;
            }
            by_glob = true;
        }
        by_glob && !self.except.iter().any(|kept| matches(kept))
    }

    /// The names in `except` that take a file back out of `glob`.
    pub fn taken_back_from(&self, glob: &str) -> Vec<&str> {
        if !glob.contains('*') {
            return Vec::new();
        }
        self.except
            .iter()
            .filter(|name| name_matches(glob, name))
            .map(String::as_str)
            .collect()
    }

    pub fn refuses_write(&self, path: &Path) -> bool {
        self.refuses_read(path)
            || forms(path)
                .iter()
                .any(|form| self.unwritten.iter().any(|never| form.starts_with(never)))
    }
}

fn in_system_folder(path: &Path) -> bool {
    SYSTEM_FOLDERS.iter().any(|folder| path.starts_with(folder))
}

/// `path` itself, or, where it holds a folder written as anywhere else, such as `/usr` holds
/// `/usr/local`, each of its entries but that one. One that can't be listed stays whole.
fn written_around(path: &Path) -> Vec<PathBuf> {
    let inside: Vec<&Path> = WRITTEN_IN_SYSTEM
        .iter()
        .map(Path::new)
        .filter(|kept| kept.starts_with(path) && *kept != path)
        .collect();
    if inside.is_empty() {
        return vec![path.to_path_buf()];
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return vec![path.to_path_buf()];
    };
    let mut around: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|entry| !inside.iter().any(|kept| kept.starts_with(entry)))
        .collect();
    around.sort();
    around
}

/// `path` with `.` and `..` taken out, and its real path where symlinks point elsewhere, found
/// as far as the path exists.
pub fn forms(path: &Path) -> Vec<PathBuf> {
    let written = normal(path);
    let real = real_path(&written);
    if real == written {
        vec![written]
    } else {
        vec![written, real]
    }
}

/// Without `.` and `..`, by its components alone.
pub fn normal(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// The real path of the part of `path` that exists, with the rest kept as written.
fn real_path(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(real, |path: PathBuf, name| path.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Two lines one event leaves, as the one line it has room for.
pub fn both(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first}; {second}")),
        (first, second) => first.or(second),
    }
}

/// How each file a session's write calls name looked as each call started, so a change its CLI
/// reports counts only once the disk shows it, and how each path outside its folder a command
/// names looked as it started, since no CLI reports what a command changed.
///
/// A command is read only for the paths its words name: `~`, `$HOME` and `${HOME}` are your
/// home, and a `*` in a path's last part is matched against what that folder holds as the
/// command starts and again once it ends. A path behind any other variable, such as `$TMPDIR`,
/// a pattern in a folder's part of the path, a path relative to a folder the command moved to
/// with `cd`, and a file a script it runs opens are never noted, so what changes there isn't
/// said. Nor is what a command left running in the background changes, since it goes on after
/// its call ends.
#[derive(Debug)]
pub struct BeforeWrites {
    /// When the session or turn it follows started. A file changed since may be the call's own
    /// write, made before its start was read, so only an older one is held to its size and time.
    since: SystemTime,
    /// Each file's size and time, or None where it wasn't there.
    files: HashMap<PathBuf, Option<Stamp>>,
    /// What each running command names outside its folder, by the call's id.
    commands: HashMap<String, Noted>,
}

/// What one running command names outside its folder.
#[derive(Debug, Default)]
struct Noted {
    /// Each path, in the order the command names it or its pattern matched it, with its size
    /// and time.
    paths: Vec<(PathBuf, Option<Stamp>)>,
    /// Each path whose last part is a pattern, whose new matches it made once it ends.
    patterns: Vec<PathBuf>,
    /// The paths its CLI reported changed while it ran, which that report already said.
    reported: Vec<PathBuf>,
}

/// How far before the session's start a file's time may read and still be the session's own
/// write: a FAT disk keeps times to two seconds.
const COARSE_TIMES: Duration = Duration::from_secs(2);

/// At most this many paths are noted for one command, so a long one can't hold the session up.
const MOST_NAMED: usize = 64;

/// At most this many paths a command changed are named in its line; the rest are counted.
const MOST_SHOWN: usize = 3;

/// A folder with more entries than this isn't matched against a command's pattern, so a long
/// one can't hold the session up.
const MOST_LISTED: usize = 4096;

impl Default for BeforeWrites {
    fn default() -> Self {
        Self {
            since: SystemTime::now(),
            files: HashMap::new(),
            commands: HashMap::new(),
        }
    }
}

impl BeforeWrites {
    /// Notes each file a write call names, or each path outside `folder` a command names, as
    /// the call starts. A relative path is `folder`'s.
    pub fn note(&mut self, folder: &Path, event: &AgentEvent, home: Option<&Path>) {
        let AgentEvent::ToolStarted {
            id, name, input, ..
        } = event
        else {
            return;
        };
        let named = Named::of(input);
        if writes_files(name, input) {
            for named in named.files {
                let path = resolved(&named, folder, home);
                let stamp = Stamp::of(&path);
                self.files.insert(path, stamp);
            }
            return;
        }
        // A CLI may report a call's start again once the command already ran, and one left
        // running in the background goes on past the end of its call.
        let in_background = input.get("run_in_background").and_then(Value::as_bool) == Some(true);
        if named.words.is_empty() || in_background || self.commands.contains_key(id) {
            return;
        }
        let mut noted = Noted::default();
        for written in named.files.iter().chain(&named.words) {
            if noted.paths.len() == MOST_NAMED {
                break;
            }
            let path = resolved(written, folder, home);
            if path.starts_with(folder) {
                continue;
            }
            let matched = if has_pattern(&path) {
                let Some(matched) = matches_of(&path) else {
                    continue;
                };
                noted.patterns.push(path);
                matched
            } else {
                vec![path]
            };
            for path in matched {
                if noted.paths.len() == MOST_NAMED {
                    break;
                }
                if !noted.paths.iter().any(|(was, _)| *was == path) {
                    let stamp = Stamp::of(&path);
                    noted.paths.push((path, stamp));
                }
            }
        }
        if !noted.paths.is_empty() || !noted.patterns.is_empty() {
            self.commands.insert(id.clone(), noted);
        }
    }

    /// The one line a file change outside every folder in `inside` leaves, such as `wrote
    /// ~/Downloads/notes.md, outside the project`. It reads the change as the CLI's adapter
    /// reports it once the write is done, whichever tool made it, and only as the disk shows
    /// it: agy 1.4.6 reports a write its own policy refused as done, to a file that may already
    /// be there. A running command that names the file leaves it out of its own line, which
    /// would say it again. A relative path is the session's own folder's, the first in `inside`.
    pub fn changed_as_reported(
        &mut self,
        inside: &[&Path],
        event: &AgentEvent,
        home: Option<&Path>,
    ) -> Option<String> {
        let AgentEvent::FileChanged { path, kind } = event else {
            return None;
        };
        let path = outside_of(inside, path)?;
        if !self.shows(&path, kind) {
            return None;
        }
        for noted in self.commands.values_mut() {
            noted.reported.push(path.clone());
        }
        let shown = shown(&path, home);
        let verb = if kind == "delete" { "deleted" } else { "wrote" };
        Some(format!("{verb} {shown}, outside the project"))
    }

    /// The line a command leaves once it ends, such as `wrote ~/Downloads/notes.md, outside
    /// the project`, for each path it named outside every folder in `inside` that the disk
    /// shows it made, changed in size or time, or removed. A folder there before and after
    /// counts for nothing, since what else changes in it may be anyone's.
    pub fn changed_by_command(
        &mut self,
        inside: &[&Path],
        event: &AgentEvent,
        home: Option<&Path>,
    ) -> Option<String> {
        let AgentEvent::ToolFinished { id, .. } = event else {
            return None;
        };
        let Noted {
            mut paths,
            patterns,
            reported,
        } = self.commands.remove(id)?;
        for pattern in &patterns {
            for path in matches_of(pattern).unwrap_or_default() {
                if !paths.iter().any(|(was, _)| *was == path) {
                    paths.push((path, None));
                }
            }
        }
        let mut wrote = Vec::new();
        let mut deleted = Vec::new();
        for (path, was) in paths {
            if reported.contains(&path) || outside_of(inside, &path.to_string_lossy()).is_none() {
                continue;
            }
            match (was, Stamp::of(&path)) {
                (Some(_), None) => deleted.push(shown(&path, home)),
                (None, Some(now)) if now.kind != Kind::Other => wrote.push(shown(&path, home)),
                (Some(was), Some(now)) if now.kind == Kind::File && was != now => {
                    wrote.push(shown(&path, home));
                }
                _ => {}
            }
        }
        let said: Vec<String> = [("wrote", wrote), ("deleted", deleted)]
            .into_iter()
            .filter(|(_, paths)| !paths.is_empty())
            .map(|(verb, paths)| format!("{verb} {}", listed(&paths)))
            .collect();
        (!said.is_empty()).then(|| format!("{}, outside the project", said.join(", ")))
    }

    /// Whether a change reported to `path` shows on the disk: a deleted file is gone, and a
    /// written one is there, changed in size or time where it was there before the session.
    fn shows(&self, path: &Path, kind: &str) -> bool {
        let now = Stamp::of(path);
        if kind == "delete" {
            return now.is_none();
        }
        let Some(now) = now else {
            return false;
        };
        match self.files.get(path) {
            Some(Some(was)) if was.older_than(self.since) => *was != now,
            // Made by the call, changed during the session, or named by no call this followed:
            // that it's there is all the disk can say.
            _ => true,
        }
    }
}

/// `a`, `a and b`, `a, b and c`, or the first few and how many more.
fn listed(paths: &[String]) -> String {
    match paths {
        [] => String::new(),
        [one] => one.clone(),
        [first @ .., last] if paths.len() <= MOST_SHOWN => {
            format!("{} and {last}", first.join(", "))
        }
        _ => format!(
            "{} and {} more",
            paths[..MOST_SHOWN].join(", "),
            paths.len() - MOST_SHOWN
        ),
    }
}

/// A file's size, modified time and kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    kind: Kind,
}

/// What a path holds: a device, a socket or a link to nothing is never a file a command wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Folder,
    Other,
}

impl Stamp {
    /// The file at `path`, through a link to what it points at, or None where nothing is there.
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path)
            .or_else(|_| std::fs::symlink_metadata(path))
            .ok()?;
        let kind = match meta.file_type() {
            kind if kind.is_file() => Kind::File,
            kind if kind.is_dir() => Kind::Folder,
            _ => Kind::Other,
        };
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            kind,
        })
    }

    fn older_than(&self, since: SystemTime) -> bool {
        self.modified
            .is_some_and(|modified| modified + COARSE_TIMES < since)
    }
}

/// Follows one session's calls for the reads and writes the never-list refused. Each CLI words
/// a refusal its own way, so it is told by a file tool that failed on the path it named, or by a
/// command that names one and says it was turned away, never by one CLI's words alone.
#[derive(Debug, Default)]
pub struct KeptOut {
    /// The never-listed paths each running call names, by its id.
    calls: HashMap<String, Vec<Kept>>,
}

/// A path a call names that the never-list keeps.
#[derive(Debug)]
struct Kept {
    path: PathBuf,
    /// As the call wrote it, which a refusal's words may repeat.
    written: String,
    /// Named by a file tool, which opens it exactly.
    exact: bool,
    /// Closed to reads as well, where a system folder is only kept from writes.
    unread: bool,
}

impl KeptOut {
    /// The line a refused call leaves after the CLI's own words, such as `kept out:
    /// ~/app/.env is on the never-list`, once a call that named a path on `never` was turned
    /// away. A relative path is `folder`'s.
    pub fn follow(
        &mut self,
        never: &Never,
        folder: &Path,
        event: &AgentEvent,
        home: Option<&Path>,
    ) -> Option<String> {
        match event {
            AgentEvent::ToolStarted {
                id, name, input, ..
            } => {
                let named = Named::of(input);
                let writes = writes_files(name, input);
                let files = named.files.iter().map(|written| (written, true));
                let words = named.words.iter().map(|written| (written, false));
                let kept: Vec<Kept> = files
                    .chain(words)
                    .filter_map(|(written, exact)| {
                        let path = resolved(written, folder, home);
                        let unread = never.refuses_read(&path);
                        // Every program reads a system folder, so a file tool that reads one
                        // was turned away by something else.
                        let unwritten = (writes || !exact) && never.refuses_write(&path);
                        (unread || unwritten).then(|| Kept {
                            path,
                            written: written.clone(),
                            exact,
                            unread,
                        })
                    })
                    .collect();
                if !kept.is_empty() {
                    self.calls.insert(id.clone(), kept);
                }
                None
            }
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                let kept = self.calls.remove(id)?;
                let said = output.as_deref().unwrap_or_default();
                let turned_away = reads_as_refused(said);
                let refused = kept
                    .iter()
                    .find(|kept| kept.exact && (turned_away || (!ok && !says_missing(said))))
                    .or_else(|| {
                        // A command names what it ran and read too, so the path its refusal
                        // names is the one, else one closed to every read.
                        let words = kept.iter().filter(|_| turned_away);
                        let named = words.clone().find(|kept| {
                            said.contains(&kept.written)
                                || said.contains(&*kept.path.to_string_lossy())
                        });
                        named.or_else(|| words.clone().find(|kept| kept.unread))
                    })?;
                Some(format!(
                    "kept out: {} is on the never-list",
                    shown(&refused.path, home)
                ))
            }
            _ => None,
        }
    }
}

/// Follows a session that reads outside its project but writes only in it, for the writes
/// there that were turned away, which you may then allow. A file tool that failed on a path
/// outside counts, and so does a command that names one and says it was refused, whatever CLI
/// words it. A path on the never-list or in a system folder isn't one a yes could open.
#[derive(Debug, Default)]
pub struct KeptIn {
    /// The paths outside each running call names, by its id, and whether it is a file tool's.
    calls: HashMap<String, (Vec<PathBuf>, bool)>,
    refused: Vec<PathBuf>,
}

/// The tools that write a file, by the names CLIs give them: Claude Code's, Codex's patch,
/// OpenCode's and agy's, `write_file` and `edit_file` among them since agy 1.4.5.
const WRITE_TOOLS: &[&str] = &[
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "file_change",
    "write",
    "edit",
    "patch",
    "apply_patch",
    "write_to_file",
    "write_file",
    "edit_file",
    "replace_file_content",
    "multi_replace_file_content",
    "sed_file",
];

impl KeptIn {
    /// Follows one event of a session in `folder` whose reach outside is `outside`, reading a
    /// reported change against `before`. A relative path is `folder`'s.
    pub fn follow(
        &mut self,
        outside: &Outside,
        folder: &Path,
        event: &AgentEvent,
        home: Option<&Path>,
        before: &BeforeWrites,
    ) {
        match event {
            AgentEvent::ToolStarted {
                id, name, input, ..
            } => {
                let file_tool = writes_files(name, input);
                let named = Named::of(input);
                let paths: Vec<PathBuf> = named
                    .files
                    .iter()
                    .chain(&named.words)
                    .map(|named| resolved(named, folder, home))
                    .filter(|path| outside.is_outside(path) && !outside.never.refuses_write(path))
                    .collect();
                if !paths.is_empty() {
                    self.calls.insert(id.clone(), (paths, file_tool));
                }
            }
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                let Some((paths, file_tool)) = self.calls.remove(id) else {
                    return;
                };
                let said = output.as_deref().unwrap_or_default();
                let refused = (file_tool && !ok) || reads_as_refused(said);
                if !refused {
                    return;
                }
                // A command names what it read too, so the path its refusal names is the one
                // it couldn't write, where it names one.
                let named: Vec<&PathBuf> = paths
                    .iter()
                    .filter(|path| said.contains(&*path.to_string_lossy()))
                    .collect();
                let refused = if named.is_empty() {
                    paths.iter().collect()
                } else {
                    named
                };
                for path in refused {
                    push_unique(&mut self.refused, path.clone());
                }
            }
            // A write the CLI called done that isn't on the disk was turned away all the same.
            AgentEvent::FileChanged { path, kind } if kind != "delete" => {
                let path = resolved(path, folder, home);
                if outside.is_outside(&path)
                    && !outside.never.refuses_write(&path)
                    && !before.shows(&path, kind)
                {
                    push_unique(&mut self.refused, path);
                }
            }
            _ => {}
        }
    }

    /// The paths outside a write was turned away from, in the order they came.
    pub fn refused(&self) -> &[PathBuf] {
        &self.refused
    }
}

/// The keys CLIs give a call's paths under, and the ones that hold a command line.
const PATH_KEYS: &[&str] = &[
    "file_path",
    "filePath",
    "path",
    "Path",
    "notebook_path",
    "AbsolutePath",
    "TargetFile",
    "DirectoryPath",
    "SearchPath",
    "file",
    "filename",
];
const COMMAND_KEYS: &[&str] = &["command", "cmd", "CommandLine"];

/// Whether a call writes files: a file tool that writes, or Codex's patch.
fn writes_files(name: &str, input: &Value) -> bool {
    WRITE_TOOLS.contains(&name) || input.get("changes").is_some()
}

/// The paths a call's input names: a file tool's under a key for a path or in a list of
/// changes as Codex gives them, and each word of a command that could be one.
#[derive(Debug, Default)]
struct Named {
    files: Vec<String>,
    words: Vec<String>,
}

impl Named {
    fn of(input: &Value) -> Self {
        let mut files: Vec<String> = PATH_KEYS
            .iter()
            .filter_map(|key| input.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        if let Some(changes) = input.get("changes").and_then(Value::as_array) {
            files.extend(
                changes
                    .iter()
                    .filter_map(|change| change.get("path").and_then(Value::as_str))
                    .map(str::to_owned),
            );
        }
        let lines = COMMAND_KEYS.iter().filter_map(|key| match input.get(*key) {
            Some(Value::String(line)) => Some(line.clone()),
            Some(Value::Array(words)) => Some(
                words
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        });
        let words = lines.flat_map(|line| command_words(&line)).collect();
        Self { files, words }
    }
}

/// Options whose value is a pattern to match or leave out, never a file the command opens.
const PATTERN_OPTIONS: &[&str] = &[
    "--exclude",
    "--exclude-dir",
    "--exclude-from",
    "--include",
    "--glob",
    "--iglob",
    "-g",
    "--ignore",
    "-name",
    "-iname",
    "-path",
    "-not",
];

/// Each word of `line` that could name a file: a pattern a command matches or leaves out
/// (`--exclude=.env`, `-g '!.env'`, `! -name .env`) isn't one, and an option's value is.
fn command_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut skip_next = false;
    for word in line
        .split(|ch: char| ch.is_whitespace() || "'\";|&<>()`".contains(ch))
        .filter(|word| !word.is_empty())
    {
        if std::mem::take(&mut skip_next) || word.starts_with('!') {
            continue;
        }
        if PATTERN_OPTIONS.contains(&word) {
            skip_next = true;
            continue;
        }
        match word.split_once('=') {
            Some((option, _)) if PATTERN_OPTIONS.contains(&option) => {}
            Some((_, value)) => {
                if !value.is_empty() {
                    words.push(value.to_owned());
                }
            }
            None => words.push(word.to_owned()),
        }
    }
    words
}

/// A path as a call names it, made absolute: `~`, `$HOME` and `${HOME}` are `home`, and a
/// relative one is `folder`'s.
fn resolved(named: &str, folder: &Path, home: Option<&Path>) -> PathBuf {
    let in_home = ["~", "$HOME", "${HOME}"].iter().find_map(|prefix| {
        let rest = named.strip_prefix(prefix)?;
        match rest.strip_prefix('/') {
            Some(rest) => Some(rest),
            None => rest.is_empty().then_some(rest),
        }
    });
    let path = match (in_home, home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => folder.join(named),
    };
    normal(&path)
}

/// Whether the last part of `path` is a pattern, with a `*` in it.
fn has_pattern(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains('*'))
}

/// What the folder `pattern` ends in holds that its last part matches, as a shell matches
/// `*`: a name that starts with `.` only where the pattern does too. None where the folder
/// can't be listed or holds too much to list.
fn matches_of(pattern: &Path) -> Option<Vec<PathBuf>> {
    let folder = pattern.parent()?;
    let glob = pattern.file_name()?.to_str()?;
    let mut found = Vec::new();
    for (count, entry) in std::fs::read_dir(folder).ok()?.enumerate() {
        if count == MOST_LISTED {
            return None;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if (glob.starts_with('.') || !name.starts_with('.')) && name_matches(glob, name) {
            found.push(folder.join(name));
        }
    }
    found.sort();
    Some(found)
}

/// `path` with your home as `~`, as a line names it.
pub fn shown(path: &Path, home: Option<&Path>) -> String {
    home.and_then(|home| path.strip_prefix(home).ok())
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|| path.display().to_string())
}

/// Whether a call's output says it was turned away: the system's words, as every CLI passes
/// them on, a read-only mount's on Linux, and the CLIs' own rules', Claude Code's deny rules
/// and OpenCode's.
fn reads_as_refused(output: &str) -> bool {
    let output = output.to_lowercase();
    [
        "operation not permitted",
        "permission denied",
        "read-only file system",
        "eperm",
        "eacces",
        "erofs",
        "denied by your permission settings",
        "deny rule",
        "has been denied",
        "specified a rule",
        "rejected permission",
    ]
    .iter()
    .any(|said| output.contains(said))
}

/// Whether a file tool's failure says the file isn't there, which no list kept out.
fn says_missing(output: &str) -> bool {
    let output = output.to_lowercase();
    [
        "no such file",
        "does not exist",
        "not found",
        "enoent",
        "cannot find",
    ]
    .iter()
    .any(|said| output.contains(said))
}

/// `written` as an absolute path without `..`, when it's outside every folder in `inside`.
fn outside_of(inside: &[&Path], written: &str) -> Option<PathBuf> {
    let folder = inside.first()?;
    let path = normal(&folder.join(written));
    let forms_of_path = forms(&path);
    let outside = !inside.iter().any(|folder| {
        let folder = forms(folder);
        forms_of_path
            .iter()
            .any(|form| folder.iter().any(|root| form.starts_with(root)))
    });
    outside.then_some(path)
}

/// `*` stands for any run of characters in a name, and nothing else is special.
pub fn name_matches(glob: &str, name: &str) -> bool {
    let mut parts = glob.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

fn push_unique<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn never_in(home: &Path) -> Never {
        Never::resolve(&OutsideSettings::default().never, home)
    }

    /// A project's committed templates, such as `.env.example`, open as any file does: a name
    /// with `*` gives them up, so `.env.local` stays closed, and a template you list plainly is
    /// closed again.
    #[test]
    fn env_templates_are_taken_back_out_of_the_env_files() {
        let home = tempfile::tempdir().unwrap();
        let never = never_in(home.path());
        let app = home.path().join("work/app");
        for template in [".env.example", ".env.sample", ".env.template", ".env.dist"] {
            assert!(!never.refuses_read(&app.join(template)), "{template}");
            assert!(!never.refuses_write(&app.join(template)), "{template}");
        }
        for secret in [
            ".env",
            ".env.local",
            ".env.production",
            ".env.example.local",
        ] {
            assert!(never.refuses_read(&app.join(secret)), "{secret}");
        }
        assert_eq!(
            never.taken_back_from(".env.*"),
            [".env.example", ".env.sample", ".env.template", ".env.dist"]
        );
        assert!(never.taken_back_from(".env").is_empty());
        let mut entries = OutsideSettings::default().never;
        entries.push(".env.example".to_owned());
        let closed_again = Never::resolve(&entries, home.path());
        assert!(closed_again.refuses_read(&app.join(".env.example")));
        assert!(!closed_again.refuses_read(&app.join(".env.sample")));
        let odd = Never::resolve(
            &["!".to_owned(), "!~/.ssh".to_owned(), ".env.*".to_owned()],
            home.path(),
        );
        assert!(odd.except.is_empty());
        assert!(odd.refuses_read(&app.join(".env.example")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_macos_a_name_matches_in_any_case_as_the_disk_opens_it() {
        let home = tempfile::tempdir().unwrap();
        let never = never_in(home.path());
        assert!(never.refuses_read(&home.path().join("work/app/.ENV")));
        assert!(never.refuses_read(&home.path().join("work/app/.Env.Local")));
        assert!(!never.refuses_read(&home.path().join("work/app/.ENV.EXAMPLE")));
    }

    /// A write outside that a session reading only there was turned from is kept for you to
    /// allow, whichever way its CLI said so. A read there, a write inside, a path no yes opens
    /// and a write that worked aren't.
    #[test]
    fn a_write_outside_turned_away_is_kept_for_you_to_allow() {
        // Not under /home, which macOS mounts from a system folder.
        let home = Path::new("/Users/you");
        let app = Path::new("/Users/you/app");
        let outside = Outside::for_mode(
            &OutsideSettings::default(),
            ApprovalMode::Supervised,
            Some(app),
            home,
        )
        .without_asking();
        let started = |id: &str, name: &str, input: Value| AgentEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
            input,
            parent: None,
        };
        let finished = |id: &str, ok: bool, output: Option<&str>| AgentEvent::ToolFinished {
            id: id.into(),
            ok,
            output: output.map(str::to_owned),
            parent: None,
        };
        let calls = [
            // Codex's patch says nothing when it fails.
            (
                started(
                    "p",
                    "file_change",
                    json!({"changes": [{"path": "/Users/you/Downloads/notes.md", "kind": "add"}]}),
                ),
                finished("p", false, None),
            ),
            (
                started(
                    "c",
                    "Bash",
                    json!({"command": "cp ~/Downloads/brief.txt ~/Downloads/copy.txt"}),
                ),
                finished(
                    "c",
                    false,
                    Some("cp: /Users/you/Downloads/copy.txt: Operation not permitted"),
                ),
            ),
            (
                started(
                    "r",
                    "Read",
                    json!({"file_path": "/Users/you/Downloads/gone.txt"}),
                ),
                finished("r", false, Some("File does not exist.")),
            ),
            (
                started("i", "Write", json!({"file_path": "notes.md"})),
                finished("i", false, Some("Operation not permitted")),
            ),
            (
                started("n", "Write", json!({"file_path": "/Users/you/.ssh/config"})),
                finished("n", false, None),
            ),
            (
                started("s", "Bash", json!({"command": "echo x > /etc/hosts"})),
                finished("s", false, Some("permission denied: /etc/hosts")),
            ),
            (
                started(
                    "w",
                    "Write",
                    json!({"file_path": "/Users/you/Downloads/ok.md"}),
                ),
                finished("w", true, None),
            ),
            // agy 1.4.6's own policy, which names the path under `Path`.
            (
                started(
                    "a",
                    "write_file",
                    json!({"Path": "/Users/you/Downloads/agy.md"}),
                ),
                finished(
                    "a",
                    false,
                    Some(
                        "permission check failed for write_file \"/Users/you/Downloads/agy.md\": \
                         user denied permission for write_file(/Users/you/Downloads/agy.md)",
                    ),
                ),
            ),
        ];
        let mut kept = KeptIn::default();
        let before = BeforeWrites::default();
        for (start, finish) in &calls {
            kept.follow(&outside, app, start, Some(home), &before);
            kept.follow(&outside, app, finish, Some(home), &before);
        }
        // agy 1.4.6 calls a write its policy refused done, and reports the file changed.
        let reported = AgentEvent::FileChanged {
            path: "/Users/you/Downloads/reported.md".into(),
            kind: "add".into(),
        };
        kept.follow(&outside, app, &reported, Some(home), &before);
        assert_eq!(
            kept.refused(),
            [
                PathBuf::from("/Users/you/Downloads/notes.md"),
                PathBuf::from("/Users/you/Downloads/copy.txt"),
                PathBuf::from("/Users/you/Downloads/agy.md"),
                PathBuf::from("/Users/you/Downloads/reported.md")
            ]
        );
    }

    /// A refused write to a startup file, a tool config, or where programs are installed is
    /// kept for you to allow as any other write outside. Only the never-list's aren't.
    #[test]
    fn a_refused_install_or_startup_file_edit_is_kept_for_you_to_allow() {
        let home = Path::new("/Users/you");
        let app = Path::new("/Users/you/app");
        let outside = Outside::for_mode(
            &OutsideSettings::default(),
            ApprovalMode::Supervised,
            Some(app),
            home,
        )
        .without_asking();
        let calls = [
            ("z", "Edit", json!({"file_path": "/Users/you/.zshrc"})),
            (
                "g",
                "file_change",
                json!({"changes": [{"path": "/Users/you/.gitconfig", "kind": "update"}]}),
            ),
            (
                "l",
                "write_file",
                json!({"Path": "/Users/you/Library/LaunchAgents/tool.plist"}),
            ),
            (
                "c",
                "write",
                json!({"filePath": "/Users/you/.cargo/bin/tool"}),
            ),
            ("b", "Write", json!({"file_path": "/opt/homebrew/bin/tool"})),
            ("s", "Write", json!({"file_path": "/usr/bin/tool"})),
            ("k", "Write", json!({"file_path": "/Users/you/.ssh/rc"})),
        ];
        let mut kept = KeptIn::default();
        let before = BeforeWrites::default();
        for (id, name, input) in calls {
            let started = AgentEvent::ToolStarted {
                id: id.into(),
                name: name.into(),
                input,
                parent: None,
            };
            let finished = AgentEvent::ToolFinished {
                id: id.into(),
                ok: false,
                output: None,
                parent: None,
            };
            kept.follow(&outside, app, &started, Some(home), &before);
            kept.follow(&outside, app, &finished, Some(home), &before);
        }
        assert_eq!(
            kept.refused(),
            [
                PathBuf::from("/Users/you/.zshrc"),
                PathBuf::from("/Users/you/.gitconfig"),
                PathBuf::from("/Users/you/Library/LaunchAgents/tool.plist"),
                PathBuf::from("/Users/you/.cargo/bin/tool"),
                PathBuf::from("/opt/homebrew/bin/tool"),
            ]
        );
    }

    #[test]
    fn names_match_with_a_star_for_any_part() {
        assert!(name_matches(".env", ".env"));
        assert!(!name_matches(".env", ".envrc"));
        assert!(name_matches(".env.*", ".env.local"));
        assert!(!name_matches(".env.*", ".env"));
        assert!(name_matches("*.pem", "server.pem"));
        assert!(name_matches("id_*_key", "id_ed25519_key"));
        assert!(!name_matches("id_*_key", "id_ed25519"));
    }

    #[test]
    fn keys_and_env_files_are_never_read_and_your_other_files_are() {
        let home = tempfile::tempdir().unwrap();
        let never = never_in(home.path());
        assert!(never.refuses_read(&home.path().join(".ssh/id_rsa")));
        assert!(never.refuses_read(&home.path().join("work/app/.env")));
        assert!(never.refuses_read(&home.path().join("work/app/.env.production")));
        assert!(never.refuses_read(&home.path().join(".codex/auth.json")));
        assert!(!never.refuses_read(&home.path().join("Downloads/x.png")));
        assert!(!never.refuses_write(&home.path().join("Downloads/y.md")));
    }

    /// A call that named a never-listed path and failed leaves one line, however its CLI worded
    /// the refusal; one that worked, or named nothing on the list, leaves none.
    #[test]
    fn a_read_the_never_list_refused_is_kept_out_whatever_the_cli_said() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let never = never_in(home);
        let folder = home.join("work/app");
        let started = |id: &str, name: &str, input: Value| AgentEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
            input,
            parent: None,
        };
        let finished = |id: &str, ok: bool, output: &str| AgentEvent::ToolFinished {
            id: id.into(),
            ok,
            output: Some(output.into()),
            parent: None,
        };
        let line = |path: &str| Some(format!("kept out: {path} is on the never-list"));
        let mut kept = KeptOut::default();
        let mut follow = |event: AgentEvent| kept.follow(&never, &folder, &event, Some(home));

        // Claude Code
        follow(started(
            "r1",
            "Read",
            json!({ "file_path": folder.join(".env") }),
        ));
        assert_eq!(
            follow(finished("r1", false, "denied by your permission settings")),
            line("~/work/app/.env")
        );
        // agy, through a command
        follow(started(
            "r2",
            "run_command",
            json!({ "CommandLine": "cat ~/.ssh/id_rsa" }),
        ));
        assert_eq!(
            follow(finished("r2", false, "cat: operation not permitted")),
            line("~/.ssh/id_rsa")
        );
        // OpenCode, by a relative path
        follow(started("r3", "read", json!({ "filePath": ".env.local" })));
        assert_eq!(
            follow(finished("r3", false, "Unknown: FileSystem.stat")),
            line("~/work/app/.env.local")
        );
        // A call reported as done whose output says the system turned it away.
        follow(started(
            "r4",
            "shell",
            json!({ "command": ["cat", ".env"] }),
        ));
        assert_eq!(
            follow(finished("r4", true, "cat: .env: Operation not permitted")),
            line("~/work/app/.env")
        );
        // A read that worked, and a refusal of a path not on the list, leave nothing.
        follow(started(
            "r5",
            "Read",
            json!({ "file_path": folder.join("README.md") }),
        ));
        assert_eq!(follow(finished("r5", false, "denied")), None);
        follow(started(
            "r6",
            "Read",
            json!({ "file_path": folder.join(".env") }),
        ));
        assert_eq!(follow(finished("r6", true, "KEY=1")), None);
    }

    /// Only a refusal leaves the line: a command that names a path on the list and fails for
    /// another reason, or matches it only as a pattern, leaves none, and neither does a file the
    /// list names that isn't there. A refusal of one leaves it however the call named the path,
    /// a system folder's write included.
    #[test]
    fn only_a_refusal_of_a_path_the_list_keeps_leaves_the_line() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let never = never_in(home);
        let folder = home.join("work/app");
        let mut kept = KeptOut::default();
        let mut call = |name: &str, input: Value, ok: bool, output: &str| {
            let started = AgentEvent::ToolStarted {
                id: "c".into(),
                name: name.into(),
                input,
                parent: None,
            };
            let finished = AgentEvent::ToolFinished {
                id: "c".into(),
                ok,
                output: Some(output.into()),
                parent: None,
            };
            assert_eq!(kept.follow(&never, &folder, &started, Some(home)), None);
            kept.follow(&never, &folder, &finished, Some(home))
        };
        let shell = |line: &str| json!({ "command": line });
        let line = |path: String| Some(format!("kept out: {path} is on the never-list"));

        for (input, output) in [
            (shell("grep -rn TODO . --exclude=.env"), ""),
            (shell("rg -g '!.env' TODO"), ""),
            (
                shell("find . ! -name .env -type f"),
                "find: ./tmp: No such file",
            ),
            (shell("ls .env"), "ls: .env: No such file or directory"),
            (
                shell("node --test --env-file .env.test"),
                "AssertionError: expected 1 to equal 2",
            ),
            (
                shell("/usr/bin/env python3 run.py"),
                "sh: run.py: Permission denied",
            ),
        ] {
            let said = call("Bash", input.clone(), false, output);
            assert_eq!(said, None, "{input}");
        }
        assert_eq!(
            call(
                "Read",
                json!({ "file_path": ".env" }),
                false,
                "File does not exist."
            ),
            None
        );
        assert_eq!(
            call(
                "Read",
                json!({ "file_path": "/etc/hosts" }),
                false,
                "is a directory"
            ),
            None
        );

        assert_eq!(
            call(
                "Bash",
                shell("cat $HOME/.ssh/id_rsa"),
                false,
                &format!(
                    "cat: {}/.ssh/id_rsa: Operation not permitted",
                    home.display()
                ),
            ),
            line("~/.ssh/id_rsa".to_owned())
        );
        assert_eq!(
            call(
                "Bash",
                shell("printf x > /etc/hosts"),
                false,
                "sh: /etc/hosts: Operation not permitted",
            ),
            line("/etc/hosts".to_owned())
        );
        assert_eq!(
            call("Write", json!({ "file_path": "/etc/hosts" }), false, ""),
            line("/etc/hosts".to_owned())
        );
        assert_eq!(
            call(
                "Read",
                json!({ "file_path": folder.join(".env") }),
                false,
                "File is in a directory that is denied by your permission settings.",
            ),
            line("~/work/app/.env".to_owned())
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                call(
                    "read",
                    json!({ "filePath": ".ENV" }),
                    false,
                    "Unknown: FileSystem.stat"
                ),
                line("~/work/app/.ENV".to_owned())
            );
        }
    }

    #[test]
    fn neither_dot_dot_nor_a_symlink_reaches_into_the_never_list() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
        std::fs::create_dir_all(home.path().join("Downloads")).unwrap();
        std::os::unix::fs::symlink(home.path().join(".ssh"), home.path().join("Downloads/keys"))
            .unwrap();
        let never = never_in(home.path());
        assert!(never.refuses_read(&home.path().join("Downloads/../.ssh/id_rsa")));
        assert!(never.refuses_read(&home.path().join("Downloads/keys/id_rsa")));
        assert!(never.refuses_write(&home.path().join("Downloads/keys/authorized_keys")));
    }

    #[test]
    fn a_never_entry_that_is_a_symlink_is_kept_by_its_real_path_too() {
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), home.path().join(".aws")).unwrap();
        let never = never_in(home.path());
        let real = elsewhere.path().canonicalize().unwrap();
        assert!(never.paths.contains(&real));
        assert!(never.refuses_read(&real.join("credentials")));
    }

    #[test]
    fn system_folders_are_read_but_never_written_and_usr_local_is_written() {
        let home = tempfile::tempdir().unwrap();
        let never = never_in(home.path());
        assert!(!never.refuses_read(Path::new("/usr/bin/git")));
        assert!(never.refuses_write(Path::new("/usr/bin/git")));
        assert!(never.refuses_write(Path::new("/etc/hosts")));
        assert!(never.refuses_write(Path::new("/System/Library/x")));
        assert!(!never.refuses_write(Path::new("/usr/local/bin/tool")));
        for folder in ["/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32"] {
            let tool = Path::new(folder).join("tool");
            assert!(never.unwritten.contains(&PathBuf::from(folder)), "{folder}");
            assert!(!never.refuses_read(&tool), "{folder}");
            assert!(never.refuses_write(&tool), "{folder}");
        }
    }

    #[test]
    fn a_relative_path_is_left_out_and_a_bare_name_matches_everywhere() {
        let home = Path::new("/home/you");
        let never = Never::resolve(
            &[
                "keys/main.pem".to_owned(),
                "*.pem".to_owned(),
                " ".to_owned(),
            ],
            home,
        );
        assert!(never.paths.is_empty());
        assert_eq!(never.names, ["*.pem"]);
        assert!(never.refuses_read(Path::new("/srv/certs/server.pem")));
    }

    #[test]
    fn a_change_outside_the_folders_is_said_whichever_way_it_is_named() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        for file in ["Downloads/notes.md", "notes.md", "app/src/main.rs"] {
            std::fs::create_dir_all(you.join(file).parent().unwrap()).unwrap();
            std::fs::write(you.join(file), "made up").unwrap();
        }
        let app = &you.join("app");
        let tree = &you.join(".argonauts/worktrees/app/run-1/task-a");
        let home = Some(you.as_path());
        let at = |path: &str| you.join(path).display().to_string();
        let mut before = BeforeWrites::default();
        let mut said = |inside: &[&Path], path: &str, kind: &str| {
            let change = AgentEvent::FileChanged {
                path: path.into(),
                kind: kind.into(),
            };
            before.changed_as_reported(inside, &change, home)
        };
        let line = |text: &str| Some(text.to_owned());
        assert_eq!(
            said(&[app], &at("Downloads/notes.md"), "add"),
            line("wrote ~/Downloads/notes.md, outside the project")
        );
        assert_eq!(
            said(&[app], "../notes.md", "update"),
            line("wrote ~/notes.md, outside the project")
        );
        assert_eq!(
            said(&[app], &at("Downloads/old.md"), "delete"),
            line("deleted ~/Downloads/old.md, outside the project")
        );
        assert_eq!(said(&[app], "src/main.rs", "update"), None);
        assert_eq!(said(&[tree, app], "src/lib.rs", "add"), None);
        assert_eq!(said(&[tree, app], &at("app/README.md"), "update"), None);
        // Only what the disk shows: a write reported done that isn't there, and a delete of a
        // file still there, leave no line.
        assert_eq!(said(&[app], &at("Downloads/refused.md"), "add"), None);
        assert_eq!(said(&[app], &at("Downloads/notes.md"), "delete"), None);
        let started = AgentEvent::ToolStarted {
            id: "w1".into(),
            name: "Write".into(),
            input: serde_json::json!({ "file_path": at("Downloads/notes.md") }),
            parent: None,
        };
        assert_eq!(before.changed_as_reported(&[app], &started, home), None);
    }

    fn command(id: &str, line: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: "Bash".into(),
            input: json!({ "command": line }),
            parent: None,
        }
    }

    fn ended(id: &str) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: None,
            parent: None,
        }
    }

    /// No CLI reports what a command changed, so the paths outside it names are noted as it
    /// starts and looked at again once it ends: a file it made, changed or removed, and a
    /// folder it made, leave one line. A file it only read, a folder that was there before and
    /// after, a device and its own folder leave none.
    #[test]
    fn a_command_that_changed_paths_outside_says_so_once_it_ends() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        let app = you.join("app");
        let downloads = you.join("Downloads");
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::create_dir_all(&downloads).unwrap();
        for name in ["kept.md", "old.md", "read.md"] {
            std::fs::write(downloads.join(name), "made up").unwrap();
        }
        let home = Some(you.as_path());
        let mut before = BeforeWrites::default();
        before.note(
            &app,
            &command(
                "c1",
                "cp ~/Downloads/read.md ~/Downloads/new.md && printf more >> $HOME/Downloads/kept.md \
                 && rm ../Downloads/old.md && mkdir ~/Downloads/made && ls ~/Downloads > /dev/null \
                 && printf x > src/lib.rs",
            ),
            home,
        );
        std::fs::write(downloads.join("new.md"), "made up").unwrap();
        std::fs::write(downloads.join("kept.md"), "made up, and more").unwrap();
        std::fs::remove_file(downloads.join("old.md")).unwrap();
        std::fs::create_dir(downloads.join("made")).unwrap();
        std::fs::write(app.join("src/lib.rs"), "x").unwrap();
        assert_eq!(
            before.changed_by_command(&[&app], &ended("c1"), home),
            Some(
                "wrote ~/Downloads/new.md, ~/Downloads/kept.md and ~/Downloads/made, \
                 deleted ~/Downloads/old.md, outside the project"
                    .to_owned()
            )
        );
        assert_eq!(
            before.changed_by_command(&[&app], &ended("c1"), home),
            None,
            "said once"
        );

        before.note(&app, &command("c2", "cat ~/Downloads/read.md"), home);
        assert_eq!(before.changed_by_command(&[&app], &ended("c2"), home), None);
    }

    /// What a run's worker writes in your checkout or its worktree is the project's, a start
    /// reported again after the command ran keeps what it noted first, and a long list of
    /// paths is counted past the first few.
    #[test]
    fn a_command_names_only_what_it_changed_outside_every_folder_of_the_project() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        let app = you.join("app");
        let tree = you.join(".argonauts/worktrees/app/run-1/task-a");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&tree).unwrap();
        let home = Some(you.as_path());
        let mut before = BeforeWrites::default();
        let line = "touch ~/app/notes.md ~/a.md ~/b.md ~/c.md ~/d.md ~/e.md";
        before.note(&tree, &command("c1", line), home);
        for name in ["app/notes.md", "a.md", "b.md", "c.md", "d.md", "e.md"] {
            std::fs::write(you.join(name), "").unwrap();
        }
        before.note(&tree, &command("c1", line), home);
        assert_eq!(
            before.changed_by_command(&[&tree, &app], &ended("c1"), home),
            Some("wrote ~/a.md, ~/b.md, ~/c.md and 2 more, outside the project".to_owned())
        );
    }

    /// A `*` in a path's last part is matched as a shell matches it, as the command starts and
    /// again once it ends, so what it removed and what it made there are said. A name with a
    /// leading dot is left to a pattern that starts with one.
    #[test]
    fn a_command_pattern_says_what_it_removed_and_made_among_its_matches() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        let app = you.join("app");
        let logs = you.join("logs");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&logs).unwrap();
        for name in ["a.log", "b.log", ".hidden.log", "notes.md"] {
            std::fs::write(logs.join(name), "made up").unwrap();
        }
        let home = Some(you.as_path());
        let mut before = BeforeWrites::default();
        before.note(
            &app,
            &command("c1", "rm ~/logs/*.log; rotate > ~/logs/*.log"),
            home,
        );
        std::fs::remove_file(logs.join("a.log")).unwrap();
        std::fs::remove_file(logs.join("b.log")).unwrap();
        std::fs::remove_file(logs.join(".hidden.log")).unwrap();
        std::fs::write(logs.join("c.log"), "made up").unwrap();
        assert_eq!(
            before.changed_by_command(&[&app], &ended("c1"), home),
            Some(
                "wrote ~/logs/c.log, deleted ~/logs/a.log and ~/logs/b.log, outside the project"
                    .to_owned()
            )
        );
    }

    #[test]
    fn home_alone_is_your_home_however_a_command_writes_it() {
        let home = Path::new("/home/you");
        let folder = Path::new("/home/you/app");
        for named in ["~", "$HOME", "${HOME}", "~/", "$HOME/"] {
            assert_eq!(resolved(named, folder, Some(home)), home, "{named}");
        }
        assert_eq!(resolved("~you", folder, Some(home)), folder.join("~you"));
        assert_eq!(
            resolved("$HOMEWORK/a", folder, Some(home)),
            folder.join("$HOMEWORK/a")
        );
    }

    /// A command left running in the background goes on after its call ends, so nothing is
    /// said for it then. A change its CLI reported while a command ran is said once, by the
    /// report, and the command's line leaves it out.
    #[test]
    fn a_background_command_says_nothing_and_a_reported_change_is_said_once() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        let app = you.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let home = Some(you.as_path());
        let mut before = BeforeWrites::default();
        let background = AgentEvent::ToolStarted {
            id: "c1".into(),
            name: "Bash".into(),
            input: json!({ "command": "sleep 5; touch ~/later.md", "run_in_background": true }),
            parent: None,
        };
        before.note(&app, &background, home);
        std::fs::write(you.join("later.md"), "").unwrap();
        assert_eq!(before.changed_by_command(&[&app], &ended("c1"), home), None);

        before.note(&app, &command("c2", "touch ~/one.md ~/two.md"), home);
        std::fs::write(you.join("one.md"), "").unwrap();
        std::fs::write(you.join("two.md"), "").unwrap();
        let reported = AgentEvent::FileChanged {
            path: you.join("one.md").display().to_string(),
            kind: "add".into(),
        };
        assert_eq!(
            before.changed_as_reported(&[&app], &reported, home),
            Some("wrote ~/one.md, outside the project".to_owned())
        );
        assert_eq!(
            before.changed_by_command(&[&app], &ended("c2"), home),
            Some("wrote ~/two.md, outside the project".to_owned())
        );
    }

    #[test]
    fn two_lines_of_one_event_read_as_one() {
        let line = |text: &str| Some(text.to_owned());
        assert_eq!(both(line("a"), line("b")), line("a; b"));
        assert_eq!(both(None, line("b")), line("b"));
        assert_eq!(both(line("a"), None), line("a"));
        assert_eq!(both(None, None), None);
    }

    /// A file that was there before the session is said to be written only once its size or
    /// time changed, as agy 1.4.6 calls an edit its own policy refused done, and that edit is
    /// kept for you to allow. One the call made counts once it's there, and so does one changed
    /// during the session, since a call's start may be read after its write.
    #[test]
    fn a_write_reported_to_a_file_already_there_counts_once_it_changed() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        let app = you.join("app");
        let home = Some(you.as_path());
        let file = |name: &str, age: Duration| {
            let path = you.join("Downloads").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "made up\n").unwrap();
            let opened = std::fs::File::options().write(true).open(&path).unwrap();
            opened.set_modified(SystemTime::now() - age).unwrap();
            path
        };
        let hour = Duration::from_secs(3600);
        let (kept, retimed, grown) = (
            file("kept.md", hour),
            file("retimed.md", hour),
            file("grown.md", hour),
        );
        let fresh = file("fresh.md", Duration::ZERO);
        let made = you.join("Downloads/made.md");
        let mut before = BeforeWrites::default();
        for path in [&kept, &retimed, &grown, &fresh, &made] {
            let start = AgentEvent::ToolStarted {
                id: "w".into(),
                name: "edit_file".into(),
                input: json!({ "Path": path }),
                parent: None,
            };
            before.note(&app, &start, home);
        }
        let opened = std::fs::File::options().write(true).open(&retimed).unwrap();
        opened.set_modified(SystemTime::now()).unwrap();
        std::fs::write(&grown, "made up, and more\n").unwrap();
        std::fs::write(&made, "made up\n").unwrap();
        let reported = |path: &Path| AgentEvent::FileChanged {
            path: path.display().to_string(),
            kind: "update".into(),
        };
        let mut said = |path: &Path| before.changed_as_reported(&[&app], &reported(path), home);
        let wrote = |name: &str| Some(format!("wrote ~/Downloads/{name}, outside the project"));
        assert_eq!(said(&kept), None, "as it was");
        assert_eq!(said(&retimed), wrote("retimed.md"));
        assert_eq!(said(&grown), wrote("grown.md"));
        assert_eq!(said(&made), wrote("made.md"));
        assert_eq!(
            said(&fresh),
            wrote("fresh.md"),
            "changed during the session"
        );

        let outside = Outside::for_mode(
            &OutsideSettings::default(),
            ApprovalMode::Supervised,
            Some(&app),
            &you,
        )
        .without_asking();
        let mut kept_in = KeptIn::default();
        for path in [&kept, &retimed, &grown, &fresh, &made] {
            kept_in.follow(&outside, &app, &reported(path), home, &before);
        }
        assert_eq!(kept_in.refused(), std::slice::from_ref(&kept));
    }

    /// A direct turn's asking holds it outside. A run's session can't ask, so there it reads
    /// and never writes: nothing it writes outside would wait for you.
    #[test]
    fn supervised_works_outside_once_asked_and_an_empty_ask_keeps_it_in() {
        let home = Path::new("/home/you");
        let project = Some(Path::new("/home/you/app"));
        let settings = OutsideSettings::default();
        for mode in [ApprovalMode::Supervised, ApprovalMode::AutoPreview] {
            let asked = Outside::for_mode(&settings, mode, project, home);
            assert!(asked.after_asking && asked.is_open(), "{mode:?}");
            assert!(!asked.read && !asked.write, "{mode:?}");
            assert!(asked.never.refuses_read(&home.join(".ssh/id_rsa")));
            assert!(asked.writes_outside(SessionRole::Direct));
            assert!(!asked.reads_unasked(SessionRole::Direct));
            assert!(!asked.writes_unasked(SessionRole::Direct));
            assert!(!asked.writes_outside(SessionRole::Build), "{mode:?}");
            assert!(asked.reads_unasked(SessionRole::Build));
            assert!(!asked.writes_unasked(SessionRole::Build), "{mode:?}");
            assert!(asked.reads_unasked(SessionRole::ReadOnly));
            assert!(!asked.writes_outside(SessionRole::ReadOnly));
            assert!(!asked.writes_unasked(SessionRole::ReadOnly));
        }
        let kept_in = OutsideSettings {
            ask: Vec::new(),
            ..OutsideSettings::default()
        };
        let supervised = Outside::for_mode(&kept_in, ApprovalMode::Supervised, project, home);
        assert_eq!(supervised, Outside::default());
        assert!(!supervised.writes_outside(SessionRole::Direct));
    }

    #[test]
    fn a_session_that_cant_ask_reads_where_asking_would_open_and_writes_only_its_project() {
        let home = Path::new("/home/you");
        let project = Some(Path::new("/home/you/app"));
        let settings = OutsideSettings::default();
        let asked = Outside::for_mode(&settings, ApprovalMode::Supervised, project, home);
        let unasked = asked.clone().without_asking();
        assert!(unasked.read && !unasked.write && !unasked.after_asking);
        assert_eq!(unasked.never, asked.never);
        assert!(!unasked.writes_outside(SessionRole::Direct));
        assert!(unasked.reads_unasked(SessionRole::Direct));
        let autopilot = Outside::for_mode(&settings, ApprovalMode::Autopilot, project, home);
        assert_eq!(autopilot.clone().without_asking(), autopilot);
        assert_eq!(Outside::default().without_asking(), Outside::default());
    }

    #[test]
    fn autopilot_works_outside_without_asking() {
        let home = Path::new("/home/you");
        let settings = OutsideSettings::default();
        let autopilot = Outside::for_mode(
            &settings,
            ApprovalMode::Autopilot,
            Some(Path::new("/home/you/app")),
            home,
        );
        assert!(autopilot.read && autopilot.write && !autopilot.after_asking);
        assert!(autopilot.writes_unasked(SessionRole::Direct));
        assert!(!autopilot.writes_unasked(SessionRole::ReadOnly));
        assert!(!autopilot.never.is_empty());
        assert!(autopilot.is_outside(Path::new("/home/you/Downloads/notes.md")));
        assert!(!autopilot.is_outside(Path::new("/home/you/app/src/main.rs")));
        assert!(autopilot.is_outside(Path::new("/home/you/app/../notes.md")));
    }
}
