//! Staging a skill from a folder or a git URL. Nothing it brings in ever runs: a clone has
//! hooks, submodules, and LFS smudging off, links are never followed, files are written without
//! execute bits, and scripts are only listed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use time::OffsetDateTime;

use argonauts_core::base::clock;
use argonauts_core::base::hash::sha256_hex;

use super::document::{self, Cleaned};
use super::{ImportReport, Skill};
use crate::export::scan::{Finding, scan};
use crate::store::Store;
use crate::store::error::{LearningError, Result};
use crate::store::git;

/// A skill is a few instructions and helpers, so these only stop a runaway source.
const MAX_FILES: usize = 1_000;
const MAX_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DEPTH: usize = 12;
const SEARCH_DEPTH: usize = 4;

/// Files git reads as configuration when they sit in a tree, which a skill never needs.
const GIT_FILES: [&str; 2] = [".gitattributes", ".gitmodules"];

const SCRIPT_EXTENSIONS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "ksh",
    "py",
    "pyw",
    "js",
    "mjs",
    "cjs",
    "ts",
    "mts",
    "cts",
    "jsx",
    "tsx",
    "rb",
    "pl",
    "pm",
    "php",
    "ps1",
    "psm1",
    "bat",
    "cmd",
    "vbs",
    "lua",
    "tcl",
    "r",
    "jl",
    "swift",
    "go",
    "rs",
    "java",
    "kt",
    "c",
    "cc",
    "cpp",
    "h",
    "hpp",
    "cs",
    "exe",
    "dll",
    "so",
    "dylib",
    "wasm",
    "jar",
    "bin",
    "app",
    "command",
    "applescript",
    "scpt",
];

pub fn import(store: &Store, source: &str) -> Result<ImportReport> {
    let imports = store.imports_dir();
    fs::create_dir_all(&imports)?;
    let at = clock::now_utc();
    match origin(source)? {
        Origin::Folder(path) => stage(&imports, source, &path, at),
        Origin::Git(url) => {
            let clone = imports.join(format!(
                ".clone-{}-{}",
                std::process::id(),
                at.unix_timestamp_nanos()
            ));
            let result = fetch(&imports, &url, &clone)
                .and_then(|()| stage(&imports, &without_credentials(source), &clone, at));
            let _ = fs::remove_dir_all(&clone);
            result
        }
    }
}

enum Origin {
    Folder(PathBuf),
    Git(String),
}

static SCP_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9._-]+@[A-Za-z0-9.-]+:[^\s]+$")
        .unwrap_or_else(|err| panic!("scp url pattern: {err}"))
});

static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Za-z][A-Za-z0-9+.-]*://)[^/@\s]+@")
        .unwrap_or_else(|err| panic!("url credentials pattern: {err}"))
});

fn origin(source: &str) -> Result<Origin> {
    let path = Path::new(source);
    if path.is_dir() {
        return Ok(Origin::Folder(path.to_path_buf()));
    }
    let lower = source.to_ascii_lowercase();
    // Only transports that fetch. `ext::` and the like run commands, so they never get here.
    let fetches = ["https://", "http://", "ssh://", "git://", "file://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        || SCP_URL.is_match(source);
    if fetches {
        Ok(Origin::Git(source.to_owned()))
    } else {
        Err(LearningError::Refused(format!(
            "`{source}` is neither a folder nor a git URL."
        )))
    }
}

/// The URL without a user or token in it, for anything that records it.
pub fn without_credentials(source: &str) -> String {
    URL_CREDENTIALS.replace(source, "$1").into_owned()
}

fn fetch(imports: &Path, url: &str, dest: &Path) -> Result<()> {
    let dest = dest.to_string_lossy();
    git::run_with(
        imports,
        &[
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "submodule.recurse=false",
            "clone",
            "--quiet",
            "--depth",
            "1",
            "--single-branch",
            "--no-tags",
            "--no-recurse-submodules",
            "--template=",
            "--",
            url,
            &dest,
        ],
        &[("GIT_LFS_SKIP_SMUDGE", "1"), ("GIT_TERMINAL_PROMPT", "0")],
        None,
    )?;
    Ok(())
}

/// One file of the skill as found.
struct Found {
    path: String,
    bytes: Vec<u8>,
}

fn stage(imports: &Path, source: &str, root: &Path, at: OffsetDateTime) -> Result<ImportReport> {
    let folder = skill_folder(root, source)?;
    let mut changes = Vec::new();
    let mut found = Vec::new();
    let mut total = 0;
    walk(&folder, "", 0, &mut found, &mut changes, &mut total)?;

    let mut rejected = Vec::new();
    let original = found
        .iter()
        .find(|file| file.path == "SKILL.md")
        .map(|file| String::from_utf8(file.bytes.clone()));
    let original = match original {
        Some(Ok(text)) => text,
        Some(Err(_)) => {
            rejected.push("`SKILL.md` isn't UTF-8 text".to_owned());
            String::new()
        }
        None => {
            return Err(LearningError::Refused(format!(
                "There's no SKILL.md in {source}."
            )));
        }
    };
    let (yaml, body) = match document::split(&original) {
        Some((yaml, body)) => (Some(yaml), body),
        None => {
            changes.push("added the frontmatter it didn't have".to_owned());
            (None, original.as_str())
        }
    };
    let value = match yaml.map(document::parse_yaml) {
        Some(Ok(value)) => value,
        Some(Err(err)) => {
            rejected.push(format!("its frontmatter isn't valid YAML: {err}"));
            serde_json::Value::Null
        }
        None => serde_json::Value::Null,
    };
    let folder_name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Cleaned {
        mut frontmatter,
        changes: cleaned,
    } = document::clean(value, &folder_name);
    changes.extend(cleaned);
    if frontmatter.name.is_empty() {
        frontmatter.name = "imported-skill".to_owned();
        changes.push("named it `imported-skill`, since nothing else gave a usable name".to_owned());
    }
    if frontmatter.description.is_empty() {
        rejected.push("it has no description, which the spec requires".to_owned());
    }
    let name = frontmatter.name.clone();
    let skill_md = document::render(&Skill {
        frontmatter,
        body: body.to_owned(),
    });
    rejected.extend(
        document::validate(&skill_md, &name)
            .into_iter()
            .filter(|problem| !problem.contains("description")),
    );

    let staged = imports.join(&name);
    if staged.exists() {
        fs::remove_dir_all(&staged)?;
    }
    fs::create_dir_all(&staged)?;

    let mut files = Vec::new();
    let mut code = Vec::new();
    let mut findings: Vec<Finding> = Vec::new();
    for file in &found {
        let bytes = if file.path == "SKILL.md" {
            skill_md.as_bytes()
        } else {
            &file.bytes
        };
        let target = staged.join(&file.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, bytes)?;
        files.push((file.path.clone(), sha256_hex(bytes)));

        let text = as_text(&file.bytes);
        if text.is_none() || is_script(&file.path, text) {
            code.push(file.path.clone());
        }
        let mut scanned = text.map(scan).unwrap_or_default();
        if file.path == "SKILL.md" {
            scanned.extend(scan(&skill_md));
        }
        for finding in scanned {
            if finding.refuses() {
                let problem = format!("`{}` holds {finding}", file.path);
                if !rejected.contains(&problem) {
                    rejected.push(problem);
                }
            }
            if !findings.contains(&finding) {
                findings.push(finding);
            }
        }
    }

    Ok(ImportReport {
        name,
        source: source.to_owned(),
        at,
        staged,
        files,
        code,
        cleaned: changes,
        findings,
        rejected,
    })
}

/// The folder holding `SKILL.md`: the root, or the only one a few levels down.
fn skill_folder(root: &Path, source: &str) -> Result<PathBuf> {
    if is_regular_file(&root.join("SKILL.md")) {
        return Ok(root.to_path_buf());
    }
    let mut folders = Vec::new();
    search(root, 0, &mut folders)?;
    match folders.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(LearningError::Refused(format!(
            "There's no SKILL.md in {source}."
        ))),
        many => Err(LearningError::Refused(format!(
            "{source} holds {} skills. Import one folder at a time.",
            many.len()
        ))),
    }
}

fn search(dir: &Path, depth: usize, folders: &mut Vec<PathBuf>) -> Result<()> {
    if depth > SEARCH_DEPTH {
        return Ok(());
    }
    for entry in sorted_entries(dir)? {
        let kind = entry.file_type()?;
        if kind.is_dir() && entry.file_name() != ".git" {
            let path = entry.path();
            if is_regular_file(&path.join("SKILL.md")) {
                folders.push(path);
            } else {
                search(&path, depth + 1, folders)?;
            }
        }
    }
    Ok(())
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

fn sorted_entries(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

/// Every regular file under `dir`. Links are never followed, so a skill can't point the
/// import at a file outside it, such as a key in your home folder.
fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    found: &mut Vec<Found>,
    changes: &mut Vec<String>,
    total: &mut u64,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(LearningError::Refused(format!(
            "The skill nests folders more than {MAX_DEPTH} deep."
        )));
    }
    for entry in sorted_entries(dir)? {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            changes.push(format!(
                "left out `{prefix}{}`, whose name isn't UTF-8",
                file_name.to_string_lossy()
            ));
            continue;
        };
        if name == ".git" {
            continue;
        }
        let path = format!("{prefix}{name}");
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            changes.push(format!(
                "left out the link `{path}`, which it never follows"
            ));
        } else if kind.is_dir() {
            walk(
                &entry.path(),
                &format!("{path}/"),
                depth + 1,
                found,
                changes,
                total,
            )?;
        } else if !kind.is_file() {
            changes.push(format!("left out `{path}`, which isn't a regular file"));
        } else if GIT_FILES.contains(&name) {
            changes.push(format!(
                "left out `{path}`, which git would read as settings"
            ));
        } else {
            *total += entry.metadata()?.len();
            if found.len() >= MAX_FILES || *total > MAX_BYTES {
                return Err(LearningError::Refused(format!(
                    "The skill has more than {MAX_FILES} files or {} MiB.",
                    MAX_BYTES / (1024 * 1024)
                )));
            }
            found.push(Found {
                path,
                bytes: fs::read(entry.path())?,
            });
        }
    }
    Ok(())
}

/// Text when it's UTF-8 without NUL bytes, which binaries nearly always hold.
pub fn as_text(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|text| !text.contains('\0'))
}

fn is_script(path: &str, text: Option<&str>) -> bool {
    let extension = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    extension.is_some_and(|ext| SCRIPT_EXTENSIONS.contains(&ext.as_str()))
        || text.is_some_and(|text| text.starts_with("#!"))
}
