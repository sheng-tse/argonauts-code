//! What a Linux session sees of the filesystem. Landlock can only add rights, never take one back
//! from a folder inside an allowed one, so reads are cut with mounts instead: in the session's own
//! mount namespace, each denied area is covered by an empty folder or file, and the exceptions
//! inside it are bound back. Landlock then limits writes to the writable paths.
//!
//! The steps are worked out from the profile and what is on disk at launch, by the same rules
//! [`can_read`](super::can_read) and [`can_write`](super::can_write) follow, so the most specific
//! path wins here too.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use argonauts_core::outside::name_matches;
use argonauts_core::session_runner::SandboxProfile;
use serde::{Deserialize, Serialize};

use super::profile::{PathRule, RuleKind, is_git_lock, ordered_rules, resolve_path};

/// What is at a path when the session starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Missing,
    Dir,
    /// Anything that isn't a folder: a file, a socket, a device.
    Other,
}

/// The filesystem as the launcher sees it, before anything is hidden.
pub trait Disk {
    /// Doesn't follow a final symlink, so a link is `Other` and never opens a folder it points to.
    fn entry(&self, path: &Path) -> Entry;
    fn children(&self, dir: &Path) -> Vec<PathBuf>;

    /// The file `path` opens, by its real path once symlinks are followed. None for a folder or
    /// a link to nothing.
    fn file_at(&self, path: &Path) -> Option<PathBuf> {
        (self.entry(path) == Entry::Other).then(|| path.to_path_buf())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    /// Made on disk before anything is hidden: a writable path inside a denied area, which the
    /// session couldn't otherwise make. A name with an extension is made as an empty file.
    Create { path: PathBuf, dir: bool },
    /// Covered by an empty folder. `pass` lets the session walk through it to an exception, but
    /// never list it.
    HideDir { path: PathBuf, pass: bool },
    /// Covered by an empty file nobody can open. A socket covered this way refuses connections.
    HideFile { path: PathBuf },
    /// Bound back inside a hidden folder.
    Show { path: PathBuf, dir: bool },
    /// Readable but not writable, though a writable folder holds it.
    ReadOnly { path: PathBuf },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub steps: Vec<Step>,
    /// Where Landlock allows writes, all present once the creates are done.
    pub writable: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cover {
    Hidden,
    Shown,
    /// A folder shown only for the files in it, inside a writable area: read-only, with each
    /// writable path in it bound again.
    Kept,
}

/// The mounts placed so far, to tell whether a path is still visible.
#[derive(Default)]
struct Covers(Vec<(PathBuf, Cover)>);

impl Covers {
    /// The deepest mount at or above `path`.
    fn nearest(&self, path: &Path) -> Option<(&Path, Cover)> {
        self.0
            .iter()
            .filter(|(at, _)| path.starts_with(at))
            .max_by_key(|(at, _)| at.components().count())
            .map(|(at, cover)| (at.as_path(), *cover))
    }

    /// Under an empty cover: gone from view unless shown again.
    fn hides(&self, path: &Path) -> bool {
        matches!(self.nearest(path), Some((at, Cover::Hidden)) if at != path)
    }

    fn shown(&self, path: &Path) -> bool {
        matches!(self.nearest(path), Some((at, Cover::Shown | Cover::Kept)) if at == path)
    }

    /// Inside a folder shown read-only, so a writable path there needs a bind of its own.
    fn kept(&self, path: &Path) -> bool {
        matches!(self.nearest(path), Some((at, Cover::Kept)) if at != path)
    }
}

/// Works out the steps for `profile` against `disk`.
pub fn plan(profile: &SandboxProfile, disk: &dyn Disk) -> View {
    let rules = ordered_rules(profile);
    let mut view = View::default();
    let mut covers = Covers::default();
    let mut created: Vec<(PathBuf, Entry)> = Vec::new();
    let entry = |created: &[(PathBuf, Entry)], path: &Path| {
        created
            .iter()
            .find(|(made, _)| made == path)
            .map(|(_, entry)| *entry)
            .unwrap_or_else(|| disk.entry(path))
    };

    for rule in &rules {
        let path = rule.resolved.as_path();
        match rule.kind {
            RuleKind::Stat | RuleKind::List => {}
            RuleKind::Deny => {
                if covers.hides(path) || covers.0.iter().any(|(at, _)| at == path) {
                    continue;
                }
                let pass = has_exception_below(&rules, path);
                if let Some(step) = hide(path, entry(&created, path), pass) {
                    covers.0.push((path.to_path_buf(), Cover::Hidden));
                    view.steps.push(step);
                }
            }
            RuleKind::Write | RuleKind::Read | RuleKind::ReadOnly => {
                let read = reads(&rules, path);
                let write = writes(&rules, path);
                if !read && !write {
                    continue;
                }
                let mut found = entry(&created, path);
                if found == Entry::Missing
                    && write
                    && !is_git_lock(path)
                    && parent_exists(path, disk)
                {
                    let dir = !has_extension(path);
                    view.steps.push(Step::Create {
                        path: path.to_path_buf(),
                        dir,
                    });
                    found = if dir { Entry::Dir } else { Entry::Other };
                    created.push((path.to_path_buf(), found));
                }
                // A closed folder that isn't there yet, inside a writable one, is made empty and
                // bound read-only, so the session can't plant one. A closed file can't be held
                // this way without making it, and the app checks those after the turn.
                let closed = profile
                    .closed
                    .iter()
                    .any(|closed| closed == &rule.path || closed == path);
                if found == Entry::Missing
                    && !write
                    && closed
                    && !has_extension(path)
                    && parent_exists(path, disk)
                    && under_writable(&rules, path)
                {
                    view.steps.push(Step::Create {
                        path: path.to_path_buf(),
                        dir: true,
                    });
                    found = Entry::Dir;
                    created.push((path.to_path_buf(), found));
                }
                if covers.hides(path) {
                    show(&rules, path, found, write, &mut covers, &mut view, disk);
                }
                if found == Entry::Missing {
                    continue;
                }
                if write && covers.kept(path) {
                    view.steps.push(Step::Show {
                        path: path.to_path_buf(),
                        dir: found == Entry::Dir,
                    });
                    covers.0.push((path.to_path_buf(), Cover::Shown));
                }
                if write && rule.kind == RuleKind::Write {
                    view.writable.push(path.to_path_buf());
                } else if !write && under_writable(&rules, path) {
                    view.steps.push(Step::ReadOnly {
                        path: path.to_path_buf(),
                    });
                }
            }
        }
    }
    view
}

/// At most this many entries are searched for the never-list's names in one launch, so a huge
/// tree can't hold a session up. The nearest are searched first, which is where such files sit.
const MOST_SEARCHED: usize = 100_000;

/// Folders that hold what git keeps or what was installed, never a file of yours.
const UNSEARCHED: &[&str] = &[".git", "node_modules"];

/// Covers each file the never-list names, such as `.env`, where the session would see it:
/// throughout `workdir` and your checkout, and in each folder above them. Mounts can't match a
/// name in every folder the way Seatbelt does, so the files are found at launch, and one made
/// later or past [`MOST_SEARCHED`] is held only by the CLI's own rules. A link is covered at the
/// file it opens, so that file is hidden by every name it has.
pub fn cover_names(view: &mut View, profile: &SandboxProfile, disk: &dyn Disk, workdir: &Path) {
    let names = &profile.outside.never.names;
    if names.is_empty() {
        return;
    }
    let named = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| names.iter().any(|glob| name_matches(glob, name)))
    };
    let rules = ordered_rules(profile);
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in std::iter::once(workdir).chain(profile.outside.project.as_deref()) {
        let root = resolve_path(root);
        if !roots.iter().any(|kept| root.starts_with(kept)) {
            roots.retain(|kept| !kept.starts_with(&root));
            roots.push(root);
        }
    }
    let mut above: Vec<PathBuf> = Vec::new();
    for folder in roots.iter().flat_map(|root| root.ancestors().skip(1)) {
        if !above.iter().any(|kept| kept == folder) {
            above.push(folder.to_path_buf());
        }
    }

    let mut covered: Vec<PathBuf> = view
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::HideFile { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    let mut cover = |path: &Path| {
        let Some(file) = disk.file_at(path) else {
            return;
        };
        if reads(&rules, &file) && !covered.contains(&file) {
            covered.push(file.clone());
            view.steps.push(Step::HideFile { path: file });
        }
    };

    for folder in above.iter().filter(|folder| reads(&rules, folder)) {
        for child in disk.children(folder) {
            if disk.entry(&child) == Entry::Other && named(&child) {
                cover(&child);
            }
        }
    }
    let mut queue = VecDeque::from(roots);
    let mut searched = 0;
    while let Some(folder) = queue.pop_front() {
        if !reads(&rules, &folder) {
            continue;
        }
        for child in disk.children(&folder) {
            searched += 1;
            if searched > MOST_SEARCHED {
                return;
            }
            match disk.entry(&child) {
                Entry::Dir if !unsearched(&child) => queue.push_back(child),
                Entry::Other if named(&child) => cover(&child),
                _ => {}
            }
        }
    }
}

fn unsearched(folder: &Path) -> bool {
    folder
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| UNSEARCHED.contains(&name))
}

fn hide(path: &Path, entry: Entry, pass: bool) -> Option<Step> {
    let path = path.to_path_buf();
    match entry {
        Entry::Missing => None,
        Entry::Dir => Some(Step::HideDir { path, pass }),
        Entry::Other => Some(Step::HideFile { path }),
    }
}

/// Brings `path` back into view inside a hidden area. A folder is bound itself. A file is shown
/// through its folder, since Argonauts Code replaces some files by renaming a new one over them,
/// and a bound file would keep showing the old one. The folder's other entries stay hidden.
fn show(
    rules: &[PathRule],
    path: &Path,
    entry: Entry,
    write: bool,
    covers: &mut Covers,
    view: &mut View,
    disk: &dyn Disk,
) {
    let folder = match entry {
        Entry::Dir => None,
        Entry::Missing | Entry::Other => showable_folder(path, covers),
    };
    let Some(folder) = folder else {
        if entry != Entry::Missing {
            view.steps.push(Step::Show {
                path: path.to_path_buf(),
                dir: entry == Entry::Dir,
            });
            covers.0.push((path.to_path_buf(), Cover::Shown));
        }
        return;
    };
    if covers.shown(&folder) {
        return;
    }
    if !write && entry == Entry::Missing && disk.entry(&folder) != Entry::Dir {
        return;
    }
    view.steps.push(Step::Show {
        path: folder.clone(),
        dir: true,
    });
    // A writable area around the folder would let the session add to it or change what else
    // it holds, which only the paths shown in it may be.
    if under_writable(rules, &folder) && !writes(rules, &folder) {
        view.steps.push(Step::ReadOnly {
            path: folder.clone(),
        });
        covers.0.push((folder.clone(), Cover::Kept));
    } else {
        covers.0.push((folder.clone(), Cover::Shown));
    }
    for child in disk.children(&folder) {
        if reads(rules, &child) || writes(rules, &child) {
            continue;
        }
        let pass = has_exception_below(rules, &child);
        if let Some(step) = hide(&child, disk.entry(&child), pass) {
            covers.0.push((child, Cover::Hidden));
            view.steps.push(step);
        }
    }
}

/// The folder that holds `path`, when it sits strictly inside the hidden area, so showing it
/// shows nothing beyond the area's own contents.
fn showable_folder(path: &Path, covers: &Covers) -> Option<PathBuf> {
    let folder = path.parent()?;
    match covers.nearest(path) {
        Some((area, Cover::Hidden)) if folder != area && folder.starts_with(area) => {
            Some(folder.to_path_buf())
        }
        _ => None,
    }
}

/// Whether the folder that would hold `path` is there to make it in.
fn parent_exists(path: &Path, disk: &dyn Disk) -> bool {
    path.parent()
        .is_some_and(|parent| disk.entry(parent) == Entry::Dir)
}

/// The rules that apply to `path`, shallowest first, as [`ordered_rules`] sorts them.
fn applying<'a>(rules: &'a [PathRule], path: &'a Path) -> impl Iterator<Item = &'a PathRule> {
    rules
        .iter()
        .filter(move |rule| path.starts_with(&rule.resolved))
}

/// [`can_read`](super::can_read) for a resolved path.
pub(crate) fn reads(rules: &[PathRule], path: &Path) -> bool {
    applying(rules, path).fold(true, |allowed, rule| match rule.kind {
        RuleKind::Deny => false,
        RuleKind::Read | RuleKind::ReadOnly => true,
        RuleKind::Write | RuleKind::Stat | RuleKind::List => allowed,
    })
}

/// [`can_write`](super::can_write) for a resolved path.
pub(crate) fn writes(rules: &[PathRule], path: &Path) -> bool {
    applying(rules, path).fold(false, |allowed, rule| match rule.kind {
        RuleKind::Deny | RuleKind::ReadOnly => false,
        RuleKind::Write => true,
        RuleKind::Read | RuleKind::Stat | RuleKind::List => allowed,
    })
}

/// Some exception sits below `path`, so a cover there must let the session walk through.
fn has_exception_below(rules: &[PathRule], path: &Path) -> bool {
    rules.iter().any(|rule| {
        matches!(
            rule.kind,
            RuleKind::Write | RuleKind::Read | RuleKind::ReadOnly
        ) && rule.resolved != path
            && rule.resolved.starts_with(path)
            && (reads(rules, &rule.resolved) || writes(rules, &rule.resolved))
    })
}

/// A writable folder above `path` gives Landlock write rights that reach it.
fn under_writable(rules: &[PathRule], path: &Path) -> bool {
    rules.iter().any(|rule| {
        rule.kind == RuleKind::Write
            && rule.resolved != path
            && path.starts_with(&rule.resolved)
            && writes(rules, &rule.resolved)
    })
}

fn has_extension(path: &Path) -> bool {
    path.extension().is_some_and(|ext| !ext.is_empty())
}

/// The real disk, for the launcher.
pub struct RealDisk;

impl Disk for RealDisk {
    fn entry(&self, path: &Path) -> Entry {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => Entry::Dir,
            Ok(_) => Entry::Other,
            Err(_) => Entry::Missing,
        }
    }

    fn children(&self, dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        children.sort();
        children
    }

    fn file_at(&self, path: &Path) -> Option<PathBuf> {
        let file = std::fs::canonicalize(path).ok()?;
        std::fs::metadata(&file).ok()?.is_file().then_some(file)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argonauts_core::outside::{Never, Outside};
    use argonauts_core::session_runner::SandboxMode;

    use super::*;

    /// A made-up disk: each path is a folder when it ends in `/`, otherwise a file.
    #[derive(Default)]
    struct FakeDisk {
        entries: BTreeMap<PathBuf, Entry>,
        /// Symlinks, by where they point.
        links: BTreeMap<PathBuf, PathBuf>,
    }

    impl FakeDisk {
        fn new(paths: &[&str]) -> Self {
            let mut disk = Self::default();
            for path in paths {
                let entry = if path.ends_with('/') {
                    Entry::Dir
                } else {
                    Entry::Other
                };
                disk.add(PathBuf::from(path.trim_end_matches('/')), entry);
            }
            disk
        }

        fn link(mut self, link: &str, target: &str) -> Self {
            self.add(p(link), Entry::Other);
            self.links.insert(p(link), p(target));
            self
        }

        fn add(&mut self, path: PathBuf, entry: Entry) {
            for parent in path.ancestors().skip(1) {
                self.entries
                    .entry(parent.to_path_buf())
                    .or_insert(Entry::Dir);
            }
            self.entries.insert(path, entry);
        }
    }

    impl Disk for FakeDisk {
        fn entry(&self, path: &Path) -> Entry {
            self.entries.get(path).copied().unwrap_or(Entry::Missing)
        }

        fn children(&self, dir: &Path) -> Vec<PathBuf> {
            self.entries
                .keys()
                .filter(|path| path.parent() == Some(dir))
                .cloned()
                .collect()
        }

        fn file_at(&self, path: &Path) -> Option<PathBuf> {
            let file = self.links.get(path).map_or(path, PathBuf::as_path);
            (self.entry(file) == Entry::Other && !self.links.contains_key(file))
                .then(|| file.to_path_buf())
        }
    }

    fn profile(writable: &[&str], readable: &[&str], deny: &[&str]) -> SandboxProfile {
        let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect();
        SandboxProfile {
            mode: SandboxMode::Os,
            writable: paths(writable),
            readable: paths(readable),
            deny_read: paths(deny),
            network: false,
            proxy_port: None,
            ask_port: None,
            closed: Vec::new(),
            listable: Vec::new(),
            loopback: false,
            outside: Default::default(),
        }
    }

    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    // The paths below don't exist on the machine running the tests, so resolving them leaves
    // them as written.
    const HOME: &str = "/nonexistent-argonauts-test/home";

    fn at(path: &str) -> String {
        format!("{HOME}{path}")
    }

    /// The Linux view of a direct turn, as the seatbelt test
    /// `a_direct_turn_writes_your_checkout_but_not_what_configures_agents` checks macOS: your
    /// checkout is writable, and what configures agents stays read-only whether or not it
    /// exists. A protected folder that isn't there is made empty and bound read-only; a protected
    /// file that isn't there is the app's to check after the turn.
    #[test]
    fn a_direct_turn_writes_your_checkout_but_not_what_configures_agents() {
        let repo = at("/repo");
        let disk = FakeDisk::new(&[
            &at("/repo/src/money.py"),
            &at("/repo/.claude/settings.json"),
            &at("/repo/.git/hooks/"),
            &at("/repo/.git/config"),
        ]);
        let closed: Vec<String> = [
            "/.claude",
            "/.codex",
            "/.agents",
            "/.argonauts",
            "/.opencode",
            "/.husky",
            "/.mcp.json",
            "/opencode.json",
        ]
        .iter()
        .map(|path| at(&format!("/repo{path}")))
        .collect();
        let mut readable = vec![
            repo.clone(),
            at("/repo/.git/hooks"),
            at("/repo/.git/config"),
        ];
        readable.extend(closed.iter().cloned());
        let readable: Vec<&str> = readable.iter().map(String::as_str).collect();
        let mut profile = profile(&[&repo], &readable, &[]);
        profile.closed = closed.iter().map(PathBuf::from).collect();
        let view = plan(&profile, &disk);
        assert_eq!(view.writable, [p(&repo)]);
        for folder in [
            "/.codex",
            "/.agents",
            "/.argonauts",
            "/.opencode",
            "/.husky",
        ] {
            let path = p(&at(&format!("/repo{folder}")));
            let made = Step::Create {
                path: path.clone(),
                dir: true,
            };
            assert!(view.steps.contains(&made), "{folder}: {:?}", view.steps);
            assert!(
                view.steps.contains(&Step::ReadOnly { path }),
                "{folder}: {:?}",
                view.steps
            );
        }
        for kept in ["/.claude", "/.git/hooks", "/.git/config"] {
            let path = p(&at(&format!("/repo{kept}")));
            assert!(view.steps.contains(&Step::ReadOnly { path }), "{kept}");
        }
        for file in ["/.mcp.json", "/opencode.json"] {
            let path = p(&at(&format!("/repo{file}")));
            assert!(
                !view.steps.iter().any(|step| match step {
                    Step::Create { path: made, .. } => made == &path,
                    _ => false,
                }),
                "{file} is never made"
            );
        }
    }

    #[test]
    fn a_denied_folder_is_covered_and_its_worktree_shown_again() {
        let disk = FakeDisk::new(&[
            &at("/.argonauts/state/token"),
            &at("/.argonauts/wt/other/secret.rs"),
            &at("/.argonauts/wt/mine/src/"),
            &at("/code/"),
        ]);
        let view = plan(
            &profile(
                &[&at("/.argonauts/wt/mine")],
                &[&at("/.argonauts/wt/mine")],
                &[&at("/.argonauts")],
            ),
            &disk,
        );
        assert_eq!(
            view.steps,
            vec![
                Step::HideDir {
                    path: p(&at("/.argonauts")),
                    pass: true
                },
                Step::Show {
                    path: p(&at("/.argonauts/wt/mine")),
                    dir: true
                },
            ]
        );
        assert_eq!(view.writable, vec![p(&at("/.argonauts/wt/mine"))]);
    }

    #[test]
    fn a_folder_with_nothing_shown_inside_cant_be_walked_through() {
        let disk = FakeDisk::new(&[&at("/.ssh/id_ed25519"), &at("/.netrc")]);
        let view = plan(&profile(&[], &[], &[&at("/.ssh"), &at("/.netrc")]), &disk);
        assert_eq!(
            view.steps,
            vec![
                Step::HideDir {
                    path: p(&at("/.ssh")),
                    pass: false
                },
                Step::HideFile {
                    path: p(&at("/.netrc"))
                },
            ]
        );
    }

    #[test]
    fn missing_denied_paths_need_nothing() {
        let disk = FakeDisk::new(&[&at("/code/")]);
        let view = plan(&profile(&[], &[], &[&at("/.aws")]), &disk);
        assert!(view.steps.is_empty());
    }

    #[test]
    fn session_files_show_through_their_folder_with_private_ones_hidden() {
        let session = at("/.argonauts/runs/r1/sessions/s1");
        let disk = FakeDisk::new(&[
            &format!("{session}/status.json"),
            &format!("{session}/mcp.json"),
            &format!("{session}/raw.stdout.jsonl"),
            &format!("{session}/process.json"),
            &format!("{session}/home/"),
            &at("/.argonauts/runs/r1/sessions/s2/raw.stdout.jsonl"),
        ]);
        let view = plan(
            &profile(
                &[
                    &format!("{session}/report.json"),
                    &format!("{session}/home"),
                ],
                &[
                    &format!("{session}/status.json"),
                    &format!("{session}/mcp.json"),
                    &format!("{session}/report.json"),
                    &format!("{session}/home"),
                ],
                &[&at("/.argonauts")],
            ),
            &disk,
        );
        let session = p(&session);
        assert_eq!(
            view.steps,
            vec![
                Step::HideDir {
                    path: p(&at("/.argonauts")),
                    pass: true
                },
                Step::Create {
                    path: session.join("report.json"),
                    dir: false
                },
                Step::Show {
                    path: session.clone(),
                    dir: true
                },
                Step::HideFile {
                    path: session.join("process.json")
                },
                Step::HideFile {
                    path: session.join("raw.stdout.jsonl")
                },
            ]
        );
        assert_eq!(
            view.writable,
            vec![session.join("report.json"), session.join("home")]
        );
    }

    #[test]
    fn a_missing_readable_file_still_shows_its_folder() {
        let session = at("/.argonauts/s1");
        let disk = FakeDisk::new(&[&format!("{session}/raw.stdout.jsonl")]);
        let view = plan(
            &profile(
                &[],
                &[&format!("{session}/status.json")],
                &[&at("/.argonauts")],
            ),
            &disk,
        );
        assert!(view.steps.contains(&Step::Show {
            path: p(&session),
            dir: true
        }));
        assert!(view.steps.contains(&Step::HideFile {
            path: p(&format!("{session}/raw.stdout.jsonl"))
        }));
    }

    #[test]
    fn a_file_right_inside_a_denied_area_is_shown_alone() {
        let disk = FakeDisk::new(&[&at("/.argonauts/token"), &at("/.argonauts/notes.md")]);
        let view = plan(
            &profile(&[], &[&at("/.argonauts/notes.md")], &[&at("/.argonauts")]),
            &disk,
        );
        assert_eq!(
            view.steps,
            vec![
                Step::HideDir {
                    path: p(&at("/.argonauts")),
                    pass: true
                },
                Step::Show {
                    path: p(&at("/.argonauts/notes.md")),
                    dir: false
                },
            ]
        );
    }

    #[test]
    fn config_inside_writable_state_is_read_only_and_records_hidden() {
        let disk = FakeDisk::new(&[
            &at("/.claude/settings.json"),
            &at("/.claude/hooks/"),
            &at("/.claude/history.jsonl"),
            &at("/.claude/projects/other/log.jsonl"),
        ]);
        let view = plan(
            &profile(
                &[&at("/.claude")],
                &[
                    &at("/.claude/settings.json"),
                    &at("/.claude/hooks"),
                    &at("/.claude/skills"),
                ],
                &[&at("/.claude/history.jsonl"), &at("/.claude/projects")],
            ),
            &disk,
        );
        assert_eq!(
            view.steps,
            vec![
                Step::HideFile {
                    path: p(&at("/.claude/history.jsonl"))
                },
                Step::HideDir {
                    path: p(&at("/.claude/projects")),
                    pass: false
                },
                Step::ReadOnly {
                    path: p(&at("/.claude/settings.json"))
                },
                Step::ReadOnly {
                    path: p(&at("/.claude/hooks"))
                },
            ]
        );
        assert_eq!(view.writable, vec![p(&at("/.claude"))]);
    }

    /// A worker that writes anywhere: Landlock allows writes under `/`, the never-list is
    /// covered, and your checkout and the system folders are bound read-only.
    #[test]
    fn writing_anywhere_keeps_the_never_list_hidden_and_the_checkout_read_only() {
        let disk = FakeDisk::new(&[
            &at("/.ssh/id_rsa"),
            &at("/work/app/src/"),
            &at("/Downloads/"),
            "/usr/bin/",
        ]);
        let view = plan(
            &profile(
                &["/", &at("/.argonauts/worktrees/app/run/task")],
                &[&at("/work/app"), "/usr/bin"],
                &[&at("/.ssh")],
            ),
            &disk,
        );
        assert!(view.writable.contains(&p("/")));
        for step in [
            Step::HideDir {
                path: p(&at("/.ssh")),
                pass: false,
            },
            Step::ReadOnly {
                path: p(&at("/work/app")),
            },
            Step::ReadOnly {
                path: p("/usr/bin"),
            },
        ] {
            assert!(view.steps.contains(&step), "{step:?} in {:?}", view.steps);
        }
        assert!(
            !view
                .steps
                .iter()
                .any(|step| matches!(step, Step::ReadOnly { path } if path.ends_with("Downloads")))
        );
    }

    #[test]
    fn a_writable_path_that_is_also_denied_stays_hidden_and_unwritable() {
        let disk = FakeDisk::new(&[&at("/both/file")]);
        let view = plan(&profile(&[&at("/both")], &[], &[&at("/both")]), &disk);
        assert_eq!(
            view.steps,
            vec![Step::HideDir {
                path: p(&at("/both")),
                pass: false
            }]
        );
        assert!(view.writable.is_empty());
    }

    #[test]
    fn missing_writable_paths_are_made_when_their_folder_exists() {
        let disk = FakeDisk::new(&[&at("/")]);
        let view = plan(
            &profile(&[&at("/.codex"), &at("/Library/Caches/tool")], &[], &[]),
            &disk,
        );
        assert_eq!(
            view.steps,
            vec![Step::Create {
                path: p(&at("/.codex")),
                dir: true
            }]
        );
        assert_eq!(view.writable, vec![p(&at("/.codex"))]);
    }

    #[test]
    fn a_missing_lock_is_never_made_ahead_of_git() {
        let refs = at("/repo/.git/refs/heads/");
        let disk = FakeDisk::new(&[&refs]);
        let lock = format!("{refs}task-a.lock");
        let view = plan(&profile(&[&lock], &[], &[]), &disk);
        assert!(view.steps.is_empty(), "{:?}", view.steps);
        assert!(view.writable.is_empty());
    }

    #[test]
    fn a_denied_file_already_covered_with_its_folder_is_covered_once() {
        let session = at("/.argonauts/s1");
        let disk = FakeDisk::new(&[
            &format!("{session}/status.json"),
            &format!("{session}/raw.stdout.jsonl"),
        ]);
        let view = plan(
            &profile(
                &[],
                &[&format!("{session}/status.json")],
                &[&at("/.argonauts"), &format!("{session}/raw.stdout.jsonl")],
            ),
            &disk,
        );
        let covers = view
            .steps
            .iter()
            .filter(|step| matches!(step, Step::HideFile { path } if path.ends_with("raw.stdout.jsonl")))
            .count();
        assert_eq!(covers, 1, "{:?}", view.steps);
    }

    #[test]
    fn a_denied_folder_inside_a_shown_worktree_is_covered_again() {
        let disk = FakeDisk::new(&[
            &at("/.argonauts/wt/mine/.git/hooks/pre-commit"),
            &at("/.argonauts/wt/mine/src/"),
        ]);
        let view = plan(
            &profile(
                &[&at("/.argonauts/wt/mine")],
                &[&at("/.argonauts/wt/mine")],
                &[&at("/.argonauts"), &at("/.argonauts/wt/mine/.git/hooks")],
            ),
            &disk,
        );
        assert_eq!(
            view.steps.last(),
            Some(&Step::HideDir {
                path: p(&at("/.argonauts/wt/mine/.git/hooks")),
                pass: false
            })
        );
    }

    /// Writing anywhere puts a session's own folder inside a writable area. It is shown for the
    /// files in it, read-only, so the session can't add to it or change what else it holds, and
    /// what it writes there is bound again.
    #[test]
    fn writing_anywhere_keeps_the_session_folder_read_only_but_for_its_own_files() {
        let session = at("/.argonauts/state/app/s-1");
        let disk = FakeDisk::new(&[
            &format!("{session}/report.json"),
            &format!("{session}/home/"),
            &format!("{session}/status.json"),
            &format!("{session}/stdout.raw"),
        ]);
        let view = plan(
            &profile(
                &[
                    "/",
                    &format!("{session}/report.json"),
                    &format!("{session}/home"),
                ],
                &[&format!("{session}/status.json")],
                &[&at("/.argonauts")],
            ),
            &disk,
        );
        let position = |step: Step| {
            view.steps
                .iter()
                .position(|made| *made == step)
                .unwrap_or_else(|| panic!("{step:?} in {:?}", view.steps))
        };
        let shown = position(Step::Show {
            path: p(&session),
            dir: true,
        });
        let kept = position(Step::ReadOnly { path: p(&session) });
        let report = position(Step::Show {
            path: p(&format!("{session}/report.json")),
            dir: false,
        });
        let home = position(Step::Show {
            path: p(&format!("{session}/home")),
            dir: true,
        });
        assert!(
            shown < kept && kept < report && kept < home,
            "{:?}",
            view.steps
        );
        position(Step::HideFile {
            path: p(&format!("{session}/stdout.raw")),
        });
        assert!(
            !view.steps.iter().any(|step| matches!(
                step,
                Step::Show { path, .. } if path.ends_with("status.json")
            )),
            "status.json is read through the read-only folder: {:?}",
            view.steps
        );
    }

    fn with_never_names(mut profile: SandboxProfile, project: &str) -> SandboxProfile {
        profile.outside = Outside {
            read: true,
            write: true,
            project: Some(p(project)),
            never: Never {
                names: vec![".env".to_owned(), ".env.*".to_owned()],
                ..Never::default()
            },
        };
        profile
    }

    fn covered(view: &View) -> Vec<String> {
        view.steps
            .iter()
            .filter_map(|step| match step {
                Step::HideFile { path } => Some(path.display().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Mounts can't match a name everywhere the way Seatbelt does, so the never-list's names are
    /// found at launch: throughout the folder, and in each folder above it.
    #[test]
    fn the_never_lists_names_are_covered_in_the_folder_and_above_it() {
        let repo = at("/repo");
        let disk = FakeDisk::new(&[
            &at("/.env"),
            &at("/repo/.env"),
            &at("/repo/app/config/.env.local"),
            &at("/repo/app/.envrc"),
            &at("/repo/src/main.rs"),
            &at("/repo/tools/.env/bin/python"),
            &at("/repo/.git/.env"),
            &at("/repo/node_modules/pkg/.env"),
            &at("/other/.env"),
        ]);
        let profile = with_never_names(profile(&["/"], &[], &[]), &repo);
        let mut view = plan(&profile, &disk);
        cover_names(&mut view, &profile, &disk, Path::new(&repo));
        assert_eq!(
            covered(&view),
            [
                at("/.env"),
                at("/repo/.env"),
                at("/repo/app/config/.env.local")
            ]
        );
    }

    #[test]
    fn a_worker_has_your_checkout_searched_and_each_file_is_covered_once() {
        let checkout = at("/work/app");
        let worktree = at("/.argonauts/worktrees/app/run/task");
        let disk = FakeDisk::new(&[
            &at("/work/app/.env"),
            &at("/work/app/src/lib.rs"),
            &at("/.argonauts/worktrees/app/run/task/src/lib.rs"),
        ]);
        let profile = with_never_names(profile(&["/", &worktree], &[&checkout], &[]), &checkout);
        for workdir in [worktree.clone(), at("/work/app/src")] {
            let mut view = plan(&profile, &disk);
            cover_names(&mut view, &profile, &disk, Path::new(&workdir));
            assert_eq!(covered(&view), [at("/work/app/.env")], "from {workdir}");
        }
    }

    /// A link is covered at the file it opens, so neither name reads it. A link to a folder or
    /// to nothing has nothing to cover.
    #[test]
    fn a_named_link_is_covered_at_the_file_it_opens() {
        let repo = at("/repo");
        let disk = FakeDisk::new(&[&at("/secrets/prod.env"), &at("/shared/")])
            .link(&at("/repo/.env"), &at("/secrets/prod.env"))
            .link(&at("/repo/app/.env"), &at("/gone"))
            .link(&at("/repo/lib/.env"), &at("/shared"));
        let profile = with_never_names(profile(&["/"], &[], &[]), &repo);
        let mut view = plan(&profile, &disk);
        cover_names(&mut view, &profile, &disk, Path::new(&repo));
        assert_eq!(covered(&view), [at("/secrets/prod.env")]);
    }

    #[test]
    fn a_file_already_hidden_gets_no_second_cover() {
        let repo = at("/repo");
        let disk = FakeDisk::new(&[
            &at("/repo/.env"),
            &at("/repo/.env.prod"),
            &at("/repo/private/.env"),
        ])
        .link(&at("/repo/app/.env"), &at("/repo/private/.env"));
        let profile = with_never_names(
            profile(&["/"], &[], &[&at("/repo/private"), &at("/repo/.env.prod")]),
            &repo,
        );
        let mut view = plan(&profile, &disk);
        cover_names(&mut view, &profile, &disk, Path::new(&repo));
        assert_eq!(covered(&view), [at("/repo/.env.prod"), at("/repo/.env")]);
    }

    #[test]
    fn without_never_names_nothing_is_searched() {
        let repo = at("/repo");
        let disk = FakeDisk::new(&[&at("/repo/.env")]);
        let mut profile = with_never_names(profile(&["/"], &[], &[]), &repo);
        profile.outside.never.names.clear();
        let mut view = plan(&profile, &disk);
        cover_names(&mut view, &profile, &disk, Path::new(&repo));
        assert!(view.steps.is_empty(), "{:?}", view.steps);
    }
}
