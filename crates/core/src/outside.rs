//! What a session may reach outside its project, and the never-list that holds wherever it
//! may. Every path here is absolute and free of `..`, and a path whose symlinks point elsewhere
//! is kept by its real path too, so neither a link nor `../` reaches into an entry.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::event::AgentEvent;
use crate::settings::approvals::ApprovalMode;
use crate::settings::outside::OutsideSettings;

/// Every program reads these, so a never entry in one only keeps it from being written.
pub const SYSTEM_FOLDERS: &[&str] = &["/System", "/usr", "/etc", "/bin", "/sbin", "/lib"];

/// Written as anywhere else, though a system folder holds it.
const WRITTEN_IN_SYSTEM: &[&str] = &["/usr/local"];

/// What a session does outside its project without asking. The default reaches nothing there,
/// as before `[outside]` existed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outside {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read: bool,
    /// Outside the project in place, and inside it only in the session's own folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub write: bool,
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
        if !read && !write {
            return Self::default();
        }
        Self {
            read,
            write,
            project: project.map(normal),
            never: Never::resolve(&settings.never, home),
        }
    }

    pub fn is_open(&self) -> bool {
        self.read || self.write
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
    /// System folders, read but never written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unwritten: Vec<PathBuf>,
}

impl Never {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.names.is_empty() && self.unwritten.is_empty()
    }

    /// `entries` as `[outside] never` holds them: `~/` is `home`, a name without a `/` is matched
    /// in every folder, and a relative path, which could mean any folder, is left out.
    pub fn resolve(entries: &[String], home: &Path) -> Self {
        let mut never = Self::default();
        for entry in entries.iter().map(|entry| entry.trim()) {
            if entry.is_empty() || entry.chars().any(char::is_control) {
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
                    .is_some_and(|name| self.names.iter().any(|glob| name_matches(glob, name)))
        })
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

/// The one line a file change outside every folder in `inside` leaves, such as `wrote
/// ~/Downloads/notes.md, outside the project`. It reads the change as the CLI's adapter
/// reports it once the write is done, whichever tool made it. A relative path is the
/// session's own folder's, the first in `inside`.
pub fn changed_outside(
    inside: &[&Path],
    event: &AgentEvent,
    home: Option<&Path>,
) -> Option<String> {
    let AgentEvent::FileChanged { path, kind } = event else {
        return None;
    };
    let path = outside_of(inside, path)?;
    let shown = home
        .and_then(|home| path.strip_prefix(home).ok())
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|| path.display().to_string());
    let verb = if kind == "delete" { "deleted" } else { "wrote" };
    Some(format!("{verb} {shown}, outside the project"))
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
    use super::*;

    fn never_in(home: &Path) -> Never {
        Never::resolve(&OutsideSettings::default().never, home)
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
        let app = Path::new("/home/you/app");
        let tree = Path::new("/home/you/.argonauts/worktrees/app/run-1/task-a");
        let home = Some(Path::new("/home/you"));
        let said = |inside: &[&Path], path: &str, kind: &str| {
            let change = AgentEvent::FileChanged {
                path: path.into(),
                kind: kind.into(),
            };
            changed_outside(inside, &change, home)
        };
        let line = |text: &str| Some(text.to_owned());
        assert_eq!(
            said(&[app], "/home/you/Downloads/notes.md", "add"),
            line("wrote ~/Downloads/notes.md, outside the project")
        );
        assert_eq!(
            said(&[app], "../notes.md", "update"),
            line("wrote ~/notes.md, outside the project")
        );
        assert_eq!(
            said(&[app], "/home/you/Downloads/old.md", "delete"),
            line("deleted ~/Downloads/old.md, outside the project")
        );
        assert_eq!(
            said(&[app], "/srv/out.txt", "add"),
            line("wrote /srv/out.txt, outside the project")
        );
        assert_eq!(said(&[app], "src/main.rs", "update"), None);
        assert_eq!(said(&[tree, app], "src/lib.rs", "add"), None);
        assert_eq!(
            said(&[tree, app], "/home/you/app/README.md", "update"),
            None
        );
        let started = AgentEvent::ToolStarted {
            id: "w1".into(),
            name: "Write".into(),
            input: serde_json::json!({ "file_path": "/home/you/Downloads/notes.md" }),
            parent: None,
        };
        assert_eq!(changed_outside(&[app], &started, home), None);
    }

    #[test]
    fn a_session_in_supervised_mode_gets_nothing_outside() {
        let home = Path::new("/home/you");
        let settings = OutsideSettings::default();
        let supervised = Outside::for_mode(&settings, ApprovalMode::Supervised, None, home);
        assert_eq!(supervised, Outside::default());
        let autopilot = Outside::for_mode(
            &settings,
            ApprovalMode::Autopilot,
            Some(Path::new("/home/you/app")),
            home,
        );
        assert!(autopilot.read && autopilot.write);
        assert!(!autopilot.never.is_empty());
        assert!(autopilot.is_outside(Path::new("/home/you/Downloads/notes.md")));
        assert!(!autopilot.is_outside(Path::new("/home/you/app/src/main.rs")));
        assert!(autopilot.is_outside(Path::new("/home/you/app/../notes.md")));
    }
}
