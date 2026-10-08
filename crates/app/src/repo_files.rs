//! The repo's files for the `@` list, and the files a message mentions, which go to the agent
//! with it: always the path, and a text file's contents when they're small.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::input::{at_word, at_words};

use crate::git::git;

/// Plenty for a large monorepo, and still quick to send and filter at every key.
const MAX_LISTED: usize = 100_000;
/// A mentioned file's contents go along up to this size. A bigger one goes as its path.
const MAX_FILE_BYTES: u64 = 32 * 1024;
/// Every mentioned file's contents together, so a long list of mentions can't flood the turn.
const MAX_TOTAL_BYTES: u64 = 96 * 1024;
/// A mentioned folder names this many of its entries.
const MAX_FOLDER_ENTRIES: usize = 40;

/// Files git tracks or would track (untracked and not ignored), relative to the repo with `/`.
/// A tracked file deleted from the folder is left out.
pub(crate) fn list(root: &Path) -> anyhow::Result<Vec<String>> {
    if !crate::git::has_git(root) {
        return Ok(walk(root));
    }
    let listed = git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let deleted = git(root, &["ls-files", "-z", "--deleted"])?;
    let deleted: BTreeSet<&str> = deleted.split('\0').filter(|p| !p.is_empty()).collect();
    let mut seen = BTreeSet::new();
    let paths: Vec<&str> = listed
        .split('\0')
        .filter(|path| !path.is_empty() && !deleted.contains(path))
        .filter(|path| seen.insert(*path))
        .collect();
    let real_root = root.canonicalize()?;
    Ok(paths
        .iter()
        .filter(|path| !is_link(root, path) || link_offered(root, &real_root, path, &seen))
        .take(MAX_LISTED)
        .map(|path| (*path).to_owned())
        .collect())
}

/// The files of a folder with no git, relative to it with `/`, by name. With nothing to say
/// what's ignored, every file counts but those inside a nested repository's `.git`, and links
/// are named without being followed.
fn walk(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut folders = vec![PathBuf::new()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(root.join(&folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let path = folder.join(&name);
            match entry.file_type() {
                Ok(kind) if kind.is_dir() && name != ".git" => folders.push(path),
                Ok(kind) if kind.is_file() || kind.is_symlink() => {
                    if let Some(path) = path.to_str() {
                        found.push(path.replace(std::path::MAIN_SEPARATOR, "/"));
                    }
                }
                _ => {}
            }
        }
        if found.len() >= MAX_LISTED {
            break;
        }
    }
    found.sort();
    found.truncate(MAX_LISTED);
    found
}

fn is_link(root: &Path, path: &str) -> bool {
    fs::symlink_metadata(root.join(path)).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// A link is offered only when what it reaches is: a file or folder git lists, inside the
/// repo. One that leads out of it, or to an ignored file, would send nothing, or a secret.
fn link_offered(root: &Path, real_root: &Path, path: &str, listed: &BTreeSet<&str>) -> bool {
    let Some(target) = inside_repo(real_root, &root.join(path)) else {
        return false;
    };
    let folder = format!("{target}/");
    listed.contains(target.as_str())
        || listed
            .range(folder.as_str()..)
            .next()
            .is_some_and(|first| first.starts_with(&folder))
}

/// Where `full` leads once every link on the way is followed, relative to the repo with `/`.
/// `None` when that's outside the repo or inside `.git`.
fn inside_repo(real_root: &Path, full: &Path) -> Option<String> {
    let real = full.canonicalize().ok()?;
    let relative = real.strip_prefix(real_root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return None;
        };
        let part = part.to_str()?;
        if part == ".git" {
            return None;
        }
        parts.push(part);
    }
    Some(parts.join("/"))
}

/// A file or folder a message names with `@`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mentioned {
    /// Relative to the repo, with `/`, and a folder's ending in `/`.
    pub path: String,
    pub folder: bool,
    /// Where a link leads, relative to the repo with `/`, when `path` goes through one.
    pub target: Option<String>,
}

impl Mentioned {
    /// The path git knows the contents by: the link's target, or the path itself.
    fn real(&self) -> &str {
        self.target
            .as_deref()
            .unwrap_or_else(|| self.path.trim_end_matches('/'))
    }
}

/// The `@` words in `text` that name a file or folder inside the repo, each once. A name that
/// leaves the repo, through `..` or a link, or reaches into `.git`, doesn't count.
pub(crate) fn mentioned(root: &Path, text: &str) -> Vec<Mentioned> {
    let mut found: Vec<Mentioned> = Vec::new();
    for word in at_words(text) {
        if let Some(mention) = resolve(root, &word.name)
            && !found.iter().any(|seen| seen.path == mention.path)
        {
            found.push(mention);
        }
    }
    found
}

/// Whether `text` starts with `@` and the name of a file or folder in the repo, which makes
/// it a message about that file rather than one for an agent.
pub(crate) fn leads_with_file(root: &Path, text: &str) -> bool {
    let text = text.trim_start();
    at_words(text)
        .first()
        .is_some_and(|word| word.range.start == 0 && resolve(root, &word.name).is_some())
}

fn resolve(root: &Path, name: &str) -> Option<Mentioned> {
    let path = Path::new(name);
    let relative = if path.is_absolute() {
        path.strip_prefix(root).ok()?.to_owned()
    } else {
        path.to_owned()
    };
    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() || parts.iter().any(|part| part == ".git") {
        return None;
    }
    let full: PathBuf = parts
        .iter()
        .fold(root.to_owned(), |path, part| path.join(part));
    // A link may point anywhere; what it reaches must still be in the repo.
    let target = inside_repo(&root.canonicalize().ok()?, &full)?;
    let folder = full.is_dir();
    let mut path = parts.join("/");
    let target = (target != path).then_some(target);
    if folder {
        path.push('/');
    }
    Some(Mentioned {
        path,
        folder,
        target,
    })
}

/// One part per mention, for the agent's prompt after the request. `can_read` is false for an
/// endpoint, which has no tools, so it isn't told to read what's left out.
pub(crate) fn parts(root: &Path, mentions: &[Mentioned], can_read: bool) -> Vec<InputPart> {
    if mentions.is_empty() {
        return Vec::new();
    }
    let listed = listed_under(root, mentions);
    let mut budget = MAX_TOTAL_BYTES;
    mentions
        .iter()
        .map(|mention| {
            let text = if mention.folder {
                folder_text(mention, &listed, can_read)
            } else {
                file_text(root, mention, &listed, can_read, &mut budget)
            };
            InputPart {
                kind: InputPartKind::File,
                label: at_word(&mention.path),
                approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
                text,
                source: Some(mention.path.clone()),
            }
        })
        .collect()
}

/// Which of the mentioned paths git lists, and everything it lists under the folders.
fn listed_under(root: &Path, mentions: &[Mentioned]) -> BTreeSet<String> {
    let mut args = vec![
        "--literal-pathspecs",
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    for mention in mentions {
        args.push(mention.path.trim_end_matches('/'));
        if let Some(target) = &mention.target {
            args.push(target);
        }
    }
    match git(root, &args) {
        Ok(out) => out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect(),
        Err(err) => {
            tracing::warn!("listing mentioned files: {err:#}");
            BTreeSet::new()
        }
    }
}

fn file_text(
    root: &Path,
    mention: &Mentioned,
    listed: &BTreeSet<String>,
    can_read: bool,
    budget: &mut u64,
) -> String {
    let path = &mention.path;
    let head = format!("{} is the file {path} in the repo", at_word(path));
    let left_out = |why: String| {
        let read = if can_read {
            " Read it if you need it."
        } else {
            ""
        };
        format!("{head} ({why}).{read}")
    };
    // An ignored file, such as `.env`, may hold secrets, so it goes only by name, and so does
    // a link to one.
    if !listed.contains(path) || !listed.contains(mention.real()) {
        return left_out("git ignores it, so its contents aren't included here".into());
    }
    let full = root.join(path);
    let size = fs::metadata(&full).map_or(0, |meta| meta.len());
    if size > MAX_FILE_BYTES {
        return left_out(format!("{}, too big to include here", byte_size(size)));
    }
    if size > *budget {
        return left_out("not included, since the other files took the room".into());
    }
    let mut bytes = Vec::new();
    let read = fs::File::open(&full)
        .and_then(|file| file.take(MAX_FILE_BYTES).read_to_end(&mut bytes))
        .is_ok();
    let text = match String::from_utf8(bytes) {
        Ok(text) if read && !text.contains('\0') => text,
        _ if !read => return left_out("it couldn't be read".into()),
        _ => return left_out("binary, so its contents aren't included".into()),
    };
    *budget = budget.saturating_sub(size);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    format!("{head}:\n\n<file path=\"{path}\">\n{text}\n</file>")
}

fn folder_text(mention: &Mentioned, listed: &BTreeSet<String>, can_read: bool) -> String {
    let path = &mention.path;
    let head = format!("{} is the folder {path} in the repo", at_word(path));
    let inside = format!("{}/", mention.real());
    let entries: BTreeSet<String> = listed
        .iter()
        .filter_map(|file| file.strip_prefix(inside.as_str()))
        .map(|rest| match rest.split_once('/') {
            Some((folder, _)) => format!("{folder}/"),
            None => rest.to_owned(),
        })
        .collect();
    if entries.is_empty() {
        return format!("{head}, which git ignores or which is empty.");
    }
    let shown: Vec<&str> = entries
        .iter()
        .take(MAX_FOLDER_ENTRIES)
        .map(String::as_str)
        .collect();
    let more = entries.len() - shown.len();
    let more = if more > 0 {
        format!(", and {more} more")
    } else {
        String::new()
    };
    let read = if can_read {
        " Read what you need inside it."
    } else {
        ""
    };
    format!("{head}. It holds {}{more}.{read}", shown.join(", "))
}

fn byte_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= KB * KB {
        format!("{:.1} MB", bytes_f / KB / KB)
    } else {
        format!("{:.0} KB", (bytes_f / KB).ceil())
    }
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::TempRepo;

    use super::*;

    fn repo() -> TempRepo {
        let repo = TempRepo::new().unwrap();
        repo.write(".gitignore", "secret.env\nbuild/\n").unwrap();
        repo.write("README.md", "# Demo\n").unwrap();
        repo.write("src/main.rs", "fn main() {}\n").unwrap();
        repo.write("src/util/mod.rs", "pub fn help() {}\n").unwrap();
        repo.write("gone.txt", "soon gone\n").unwrap();
        repo.commit_all("start").unwrap();
        fs::remove_file(repo.root().join("gone.txt")).unwrap();
        repo.write("notes.txt", "untracked but kept\n").unwrap();
        repo.write("secret.env", "TOKEN=shh\n").unwrap();
        repo.write("build/out.o", "binary-ish\n").unwrap();
        repo
    }

    #[test]
    fn lists_tracked_and_untracked_files_but_not_ignored_or_deleted_ones() {
        let repo = repo();
        let mut files = list(repo.root()).unwrap();
        files.sort();
        assert_eq!(
            files,
            [
                ".gitignore",
                "README.md",
                "notes.txt",
                "src/main.rs",
                "src/util/mod.rs"
            ]
        );
    }

    #[test]
    fn mentions_name_files_and_folders_inside_the_repo_once() {
        let repo = repo();
        let root = repo.root();
        let text = "compare @src/main.rs, @src and @./README.md with @src/main.rs \
                    and @nowhere.rs, @../outside, @.git/config, and @codex";
        let found = mentioned(root, text);
        let paths: Vec<&str> = found.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["src/main.rs", "src/", "README.md"]);
        assert!(found[1].folder && !found[0].folder);
        let absolute = format!("see @{}", root.join("notes.txt").display());
        assert_eq!(mentioned(root, &absolute)[0].path, "notes.txt");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_repo_is_not_a_mention() {
        let repo = repo();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("elsewhere.txt"), "far\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("elsewhere.txt"),
            repo.root().join("link.txt"),
        )
        .unwrap();
        assert!(mentioned(repo.root(), "@link.txt").is_empty());
    }

    /// A link is checked by what it reaches, so `env-example -> .env` never sends the secret.
    #[cfg(unix)]
    #[test]
    fn a_link_goes_by_what_it_reaches() {
        let repo = repo();
        let root = repo.root();
        let link = |target: &Path, name: &str| {
            std::os::unix::fs::symlink(target, root.join(name)).unwrap();
        };
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("far.txt"), "far\n").unwrap();
        link(Path::new("secret.env"), "env-example");
        link(Path::new("README.md"), "readme-link.md");
        link(Path::new("src"), "src-link");
        link(&outside.path().join("far.txt"), "far-link.txt");
        link(Path::new("missing.txt"), "dangling.txt");

        let files = list(root).unwrap();
        for offered in ["readme-link.md", "src-link"] {
            assert!(
                files.iter().any(|file| file == offered),
                "{offered}: {files:?}"
            );
        }
        for left_out in ["env-example", "far-link.txt", "dangling.txt"] {
            assert!(
                !files.iter().any(|file| file == left_out),
                "{left_out}: {files:?}"
            );
        }

        let found = mentioned(root, "see @env-example, @readme-link.md and @src-link");
        let sent = parts(root, &found, true);
        assert_eq!(sent.len(), 3);
        assert!(!sent[0].text.contains("TOKEN"), "{}", sent[0].text);
        assert!(sent[0].text.contains("git ignores it"), "{}", sent[0].text);
        assert!(sent[1].text.contains("# Demo"), "{}", sent[1].text);
        assert!(
            sent[2].text.contains("It holds main.rs, util/"),
            "{}",
            sent[2].text
        );
    }

    #[test]
    fn a_message_led_by_a_file_is_about_it() {
        let repo = repo();
        assert!(leads_with_file(repo.root(), "  @README.md what is this?"));
        assert!(leads_with_file(repo.root(), "@src/ tour it"));
        assert!(!leads_with_file(repo.root(), "@codex look at @README.md"));
        assert!(!leads_with_file(repo.root(), "what is @README.md"));
    }

    #[test]
    fn a_small_file_goes_with_its_contents_and_a_folder_with_its_entries() {
        let repo = repo();
        let root = repo.root();
        let found = mentioned(root, "@README.md and @src/");
        let parts = parts(root, &found, true);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].kind, InputPartKind::File);
        assert_eq!(parts[0].label, "@README.md");
        assert_eq!(parts[0].source.as_deref(), Some("README.md"));
        assert_eq!(
            parts[0].text,
            "@README.md is the file README.md in the repo:\n\n\
             <file path=\"README.md\">\n# Demo\n</file>"
        );
        assert_eq!(
            parts[1].text,
            "@src/ is the folder src/ in the repo. It holds main.rs, util/. \
             Read what you need inside it."
        );
    }

    #[test]
    fn an_ignored_big_or_binary_file_goes_by_its_path_alone() {
        let repo = repo();
        let root = repo.root();
        repo.write("big.txt", &"x".repeat(40 * 1024)).unwrap();
        fs::write(root.join("image.bin"), [0u8, 159, 146, 150]).unwrap();
        let found = mentioned(root, "@secret.env @big.txt @image.bin @build/");
        let texts: Vec<String> = parts(root, &found, true)
            .into_iter()
            .map(|part| part.text)
            .collect();
        assert_eq!(
            texts[0],
            "@secret.env is the file secret.env in the repo (git ignores it, so its contents \
             aren't included here). Read it if you need it."
        );
        assert!(!texts.concat().contains("TOKEN"));
        assert_eq!(
            texts[1],
            "@big.txt is the file big.txt in the repo (40 KB, too big to include here). \
             Read it if you need it."
        );
        assert!(texts[2].contains("binary"), "{}", texts[2]);
        assert_eq!(
            texts[3],
            "@build/ is the folder build/ in the repo, which git ignores or which is empty."
        );
        let for_endpoint = parts(root, &found[1..2], false);
        assert!(!for_endpoint[0].text.contains("Read it"));
    }

    #[test]
    fn the_files_share_one_budget() {
        let repo = repo();
        let root = repo.root();
        for name in ["a", "b", "c", "d"] {
            repo.write(&format!("{name}.txt"), &"y".repeat(30 * 1024))
                .unwrap();
        }
        let found = mentioned(root, "@a.txt @b.txt @c.txt @d.txt");
        let parts = parts(root, &found, true);
        assert!(parts[2].text.contains("<file"));
        assert!(parts[3].text.contains("the other files took the room"));
    }

    #[test]
    fn a_name_with_spaces_is_written_in_quotes() {
        let repo = repo();
        let root = repo.root();
        repo.write("my notes.md", "hello\n").unwrap();
        let found = mentioned(root, "read @\"my notes.md\" please");
        assert_eq!(found[0].path, "my notes.md");
        assert_eq!(parts(root, &found, true)[0].label, "@\"my notes.md\"");
    }
}
