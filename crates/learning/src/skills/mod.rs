//! Skills: a proven learned workflow turned into step-by-step instructions one agent can follow
//! without the crew, in `.agents/skills/<name>/SKILL.md` and linked into `.claude/skills/<name>`.
//!
//! Skills are instructions every later session follows, so an export or an import is only ever
//! a proposal: a branch that waits for you. The frontmatter has the Agent Skills spec's six
//! fields and nothing else, `metadata` is flat, and the validator checks the spec's rules in
//! Rust. An import is staged outside the repo, cleaned and scanned, and its scripts never run.

mod document;
mod export;
mod import;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{LearningError, Result};
use crate::git::{self, Entry};
use crate::learned_workflows::LoadedLearnedWorkflow;
use crate::scan::{Finding, scan};
use crate::store::{CLAUDE_SKILLS_DIR, SKILLS_DIR, Store};

pub use document::{MAX_COMPATIBILITY, MAX_DESCRIPTION, MAX_NAME};
pub use import::without_credentials;

/// The metadata keys an export writes, all strings.
pub const META_LEARNED_WORKFLOW: &str = "argonauts-workflow";
pub const META_VERSION: &str = "argonauts-workflow-version";
/// The SHA-256 of the exported body, which drift checks compare.
pub const META_SHA256: &str = "argonauts-sha256";

/// A skill's frontmatter: the spec's six fields.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Frontmatter {
    /// Up to 64 lowercase letters, digits, and hyphens, the same as the folder's name.
    pub name: String,
    /// Up to 1,024 characters.
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    /// Flat, string to string.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(
        default,
        rename = "allowed-tools",
        skip_serializing_if = "Option::is_none"
    )]
    pub allowed_tools: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub frontmatter: Frontmatter,
    pub body: String,
}

/// Turns a learned workflow's steps, checklists, and checks into instructions. Refuses when the
/// result would carry a secret, hidden text, or a path under a home folder, or would break the
/// spec.
pub fn export(learned_workflow: &LoadedLearnedWorkflow) -> Result<Skill> {
    export::export(learned_workflow)
}

/// The `SKILL.md` text: the six fields in the spec's order, flat `metadata` with the body's
/// hash, then the body.
pub fn render(skill: &Skill) -> String {
    document::render(skill)
}

/// What breaks the spec in a `SKILL.md`, as `skills-ref` would report it, for a skill in the
/// folder `folder`. Empty when it's valid.
pub fn validate(text: &str, folder: &str) -> Vec<String> {
    document::validate(text, folder)
}

/// A `SKILL.md` read back: its frontmatter brought into the six fields, and its body.
pub fn parse(text: &str, folder: &str) -> Skill {
    let (value, body) = match document::split(text) {
        Some((yaml, body)) => (document::parse_yaml(yaml).unwrap_or_default(), body),
        None => (serde_json::Value::Null, text),
    };
    Skill {
        frontmatter: document::clean(value, folder).frontmatter,
        body: body.to_owned(),
    }
}

/// A branch with a skill on it, waiting for you, and the command that opens it as a pull
/// request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub branch: String,
    pub commit: String,
    pub paths: Vec<String>,
    /// What the pull request says, including an import's scan.
    pub description: String,
    pub pr_command: Vec<String>,
}

/// Exports `learned_workflow` onto `argonauts/skills/<name>`, with the `.claude/skills` link when
/// `skills.claude_link` is on.
pub fn propose_export(
    store: &Store,
    learned_workflow: &LoadedLearnedWorkflow,
    claude_link: bool,
) -> Result<Proposal> {
    let skill = export(learned_workflow)?;
    let name = skill.frontmatter.name.clone();
    let entries = skill_entries(
        &name,
        vec![("SKILL.md".to_owned(), render(&skill))],
        claude_link,
    );
    let branch = format!("argonauts/skills/{name}");
    let version = learned_workflow.learned_workflow.version;
    let title = format!("Export the {name} workflow as a skill");
    let commit = git::commit_on_branch(
        &store.repo_root,
        "HEAD",
        &branch,
        &entries,
        &format!(
            "{title}\n\nVersion {version} of the workflow, as `{SKILLS_DIR}/{name}/SKILL.md`."
        ),
    )?;
    let mut description = format!(
        "Exports version {version} of the `{name}` workflow as the skill \
         `{SKILLS_DIR}/{name}/SKILL.md`"
    );
    if claude_link {
        let _ = write!(
            description,
            ", linked at `{CLAUDE_SKILLS_DIR}/{name}` for Claude Code"
        );
    }
    description.push_str(
        ".\n\nA skill is instructions every later session follows, so it waits for you. It \
         passed the Agent Skills validator and the secret scan.\n\n",
    );
    for note in cli_notes(None) {
        let _ = writeln!(description, "- {note}");
    }
    let paths = entries.iter().map(|(path, _)| path.clone()).collect();
    Ok(Proposal {
        pr_command: pr_command(store, &branch, &title, &description),
        branch,
        commit,
        paths,
        description,
    })
}

/// How other agents come to read an exported skill. Argonauts Code never edits their config, so
/// these are for you to act on. With `repo`, the Hermes line names the command to run.
pub fn cli_notes(repo: Option<&Path>) -> Vec<String> {
    let hermes = match repo {
        Some(repo) => format!(
            "Hermes reads it once you run `hermes skills trust {}`.",
            repo.display()
        ),
        None => "Hermes reads it once you run `hermes skills trust` on this repository.".to_owned(),
    };
    vec![
        hermes,
        "OpenClaw sees it only when this repository is its agent's workspace.".to_owned(),
        "Argonauts Code never edits either tool's config.".to_owned(),
    ]
}

/// The skill's files under `.agents/skills/<name>/`, and its link in `.claude/skills/`.
fn skill_entries(
    name: &str,
    files: Vec<(String, String)>,
    claude_link: bool,
) -> Vec<(String, Entry)> {
    let mut entries: Vec<(String, Entry)> = files
        .into_iter()
        .map(|(path, text)| (format!("{SKILLS_DIR}/{name}/{path}"), Entry::File(text)))
        .collect();
    if claude_link {
        entries.push((
            format!("{CLAUDE_SKILLS_DIR}/{name}"),
            Entry::Link(format!("../../{SKILLS_DIR}/{name}")),
        ));
    }
    entries
}

fn pr_command(store: &Store, branch: &str, title: &str, body: &str) -> Vec<String> {
    let mut command: Vec<String> = ["gh", "pr", "create", "--head", branch]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if let Ok(base) = git::run(
        &store.repo_root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    ) && !base.is_empty()
    {
        command.extend(["--base".to_owned(), base]);
    }
    command.extend([
        "--title".to_owned(),
        title.to_owned(),
        "--body".to_owned(),
        body.to_owned(),
    ]);
    command
}

/// What an import found.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportReport {
    pub name: String,
    /// The folder or URL it came from, without any credentials in the URL.
    pub source: String,
    pub at: OffsetDateTime,
    /// Where it's staged, under the state folder's `imports/`.
    pub staged: PathBuf,
    /// Every file with its SHA-256, as staged.
    pub files: Vec<(String, String)>,
    /// Scripts and binaries, listed as code. None of them ever runs.
    pub code: Vec<String>,
    /// What cleaning changed: frontmatter fields dropped or flattened into `metadata`, and
    /// links or special files left out.
    pub cleaned: Vec<String>,
    pub findings: Vec<Finding>,
    /// Why it can't be proposed at all, such as hidden Unicode.
    pub rejected: Vec<String>,
}

/// Stages a skill from a folder or a git URL, cleans its frontmatter, and scans it.
pub fn import(store: &Store, source: &str) -> Result<ImportReport> {
    import::import(store, source)
}

/// Proposes a staged import as `argonauts/skills/import-<name>`, with the scan in its
/// description. Binaries stay out of the branch; scripts go in as plain files, never run.
pub fn propose_import(store: &Store, report: &ImportReport, claude_link: bool) -> Result<Proposal> {
    let name = &report.name;
    if !report.rejected.is_empty() {
        return Err(LearningError::Refused(format!(
            "The {name} import can't be proposed: {}.",
            report.rejected.join("; ")
        )));
    }
    let mut files = Vec::new();
    let mut left_out = Vec::new();
    let mut per_file = Vec::new();
    for (path, hash) in &report.files {
        let bytes = fs::read(report.staged.join(path))?;
        if argonauts_core::hash::sha256_hex(&bytes) != *hash {
            return Err(LearningError::Refused(format!(
                "The staged copy of `{path}` changed after the scan. Import {name} again."
            )));
        }
        match import::as_text(&bytes) {
            Some(text) => {
                for finding in scan(text) {
                    per_file.push(format!("`{path}`: {finding}"));
                }
                files.push((path.clone(), text.to_owned()));
            }
            None => left_out.push(path.clone()),
        }
    }
    let entries = skill_entries(name, files, claude_link);
    let branch = format!("argonauts/skills/import-{name}");
    let title = format!("Import the {name} skill");
    let source = shown_source(&report.source);
    let commit = git::commit_on_branch(
        &store.repo_root,
        "HEAD",
        &branch,
        &entries,
        &format!("{title}\n\nFrom {source}. Nothing in it has run."),
    )?;
    let description = import_description(report, &source, &per_file, &left_out, claude_link);
    let paths = entries.iter().map(|(path, _)| path.clone()).collect();
    Ok(Proposal {
        pr_command: pr_command(store, &branch, &title, &description),
        branch,
        commit,
        paths,
        description,
    })
}

/// A URL as is, but a local folder by its name only, so no home path reaches a pull request.
fn shown_source(source: &str) -> String {
    let path = Path::new(source);
    if path.is_absolute() || source.starts_with('.') || path.exists() {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.to_owned());
        format!("a local folder, `{name}`")
    } else {
        format!("`{source}`")
    }
}

fn import_description(
    report: &ImportReport,
    source: &str,
    per_file: &[String],
    left_out: &[String],
    claude_link: bool,
) -> String {
    let name = &report.name;
    let mut text = format!("Imports the `{name}` skill into `{SKILLS_DIR}/{name}/`");
    if claude_link {
        let _ = write!(text, ", linked at `{CLAUDE_SKILLS_DIR}/{name}`");
    }
    let date = report.at.format(&Rfc3339).unwrap_or_default();
    let _ = write!(
        text,
        ". Nothing in it has run.\n\nSource: {source}\nImported: {date}\n\n## Scan\n\n"
    );
    if per_file.is_empty() {
        text.push_str("Nothing flagged.\n");
    } else {
        for line in per_file {
            let _ = writeln!(text, "- {line}");
        }
    }
    if !report.code.is_empty() {
        text.push_str("\n## Code, never run\n\n");
        for path in &report.code {
            if left_out.contains(path) {
                let _ = writeln!(text, "- `{path}` (binary, left out of this branch)");
            } else {
                let _ = writeln!(text, "- `{path}`");
            }
        }
    }
    if !report.cleaned.is_empty() {
        text.push_str("\n## Cleaned\n\n");
        for change in &report.cleaned {
            let _ = writeln!(text, "- {change}");
        }
    }
    text.push_str("\n## Files\n\n");
    for (path, hash) in &report.files {
        let _ = writeln!(text, "- `{path}` sha256 `{hash}`");
    }
    text
}

/// An exported skill whose body no longer matches the hash it was exported with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub name: String,
    pub path: PathBuf,
    pub expected: String,
    pub found: String,
}

/// Exported skills edited since export. They're never overwritten, only reported.
pub fn drift(store: &Store) -> Result<Vec<Drift>> {
    let mut drifted = Vec::new();
    for (name, path, text) in skill_files(store)? {
        let Some((yaml, body)) = document::split(&text) else {
            continue;
        };
        let Ok(value) = document::parse_yaml(yaml) else {
            continue;
        };
        let Some(expected) = value
            .get("metadata")
            .and_then(|metadata| metadata.get(META_SHA256))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let found = document::body_hash(body);
        if found != expected {
            drifted.push(Drift {
                name,
                path,
                expected: expected.to_owned(),
                found,
            });
        }
    }
    Ok(drifted)
}

/// Every skill in `.agents/skills`, for `/skills`: name, description, the learned workflow it came
/// from.
pub fn list(store: &Store) -> Result<Vec<Skill>> {
    let mut skills: Vec<Skill> = skill_files(store)?
        .into_iter()
        .map(|(name, _, text)| parse(&text, &name))
        .collect();
    skills.sort_by(|a, b| a.frontmatter.name.cmp(&b.frontmatter.name));
    Ok(skills)
}

/// Each `.agents/skills/<name>/SKILL.md` that's a regular UTF-8 file, by folder name.
fn skill_files(store: &Store) -> Result<Vec<(String, PathBuf, String)>> {
    let dir = store.skills_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("SKILL.md");
        let regular = fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_file());
        if !regular {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        files.push((entry.file_name().to_string_lossy().into_owned(), path, text));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}
