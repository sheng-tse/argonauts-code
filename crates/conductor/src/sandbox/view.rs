//! What a Linux session sees of the filesystem. Landlock can only add rights, never take one back
//! from a folder inside an allowed one, so reads are cut with mounts instead: in the session's own
//! mount namespace, each denied area is covered by an empty folder or file, and the exceptions
//! inside it are bound back. Landlock then limits writes to the writable paths.
//!
//! The steps are worked out from the profile and what is on disk at launch, by the same rules
//! [`can_read`](super::can_read) and [`can_write`](super::can_write) follow, so the most specific
//! path wins here too.

use std::path::{Path, PathBuf};

use argonauts_core::session_runner::SandboxProfile;
use serde::{Deserialize, Serialize};

use super::profile::{PathRule, RuleKind, is_git_lock, ordered_rules};

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
        matches!(self.nearest(path), Some((at, Cover::Shown)) if at == path)
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
    covers.0.push((folder.clone(), Cover::Shown));
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argonauts_core::session_runner::SandboxMode;

    use super::*;

    /// A made-up disk: each path is a folder when it ends in `/`, otherwise a file.
    struct FakeDisk(BTreeMap<PathBuf, Entry>);

    impl FakeDisk {
        fn new(paths: &[&str]) -> Self {
            let mut map = BTreeMap::new();
            for path in paths {
                let entry = if path.ends_with('/') {
                    Entry::Dir
                } else {
                    Entry::Other
                };
                let path = PathBuf::from(path.trim_end_matches('/'));
                for parent in path.ancestors().skip(1) {
                    map.entry(parent.to_path_buf()).or_insert(Entry::Dir);
                }
                map.insert(path, entry);
            }
            Self(map)
        }
    }

    impl Disk for FakeDisk {
        fn entry(&self, path: &Path) -> Entry {
            self.0.get(path).copied().unwrap_or(Entry::Missing)
        }

        fn children(&self, dir: &Path) -> Vec<PathBuf> {
            self.0
                .keys()
                .filter(|path| path.parent() == Some(dir))
                .cloned()
                .collect()
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
}
