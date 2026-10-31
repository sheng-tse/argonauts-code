//! New dependencies: any package in the plan's `new_dependencies` not on
//! `limits.dependency_allowlist` blocks plan approval until you allowlist it or send the plan
//! back. After approval and before any build task, Argonauts Code adds them on the merge
//! branch with install scripts off, in one commit of Argonauts Code's (`Merge::DependenciesAdded`).

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::task::MergeEvent;

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::merge::{branch, conductor};

/// A package manager that hangs this long on the network is given up on.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// The plan's packages that aren't on the allowlist, by name, whatever version either gives.
pub fn unlisted(new: &[String], allowlist: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for package in new.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let name = package_name(package);
        let listed = allowlist
            .iter()
            .any(|allowed| package_name(allowed).eq_ignore_ascii_case(name));
        if !listed && !out.iter().any(|seen| seen == package) {
            out.push(package.to_owned());
        }
    }
    out
}

/// Adds the packages on the run branch and commits the manifest and lockfile change as
/// Argonauts Code. Returns the commit, or the branch head when they were already there.
pub async fn add(host: &Host, packages: &[String]) -> EngineResult<String> {
    if let Some(bad) = packages.iter().find(|package| !valid_package(package)) {
        return Err(EngineError::Refused(format!(
            "`{bad}` isn't a package name Argonauts Code will pass to a package manager."
        )));
    }
    let worktree = branch::ensure_run_branch(host).await?;
    let installer = installer(&root_files(&worktree)?, packages).map_err(EngineError::Refused)?;
    match &installer.run {
        Run::Command(argv) => run(host, &worktree, argv).await?,
        Run::AppendLines(file) => append_lines(&worktree.join(file), packages)?,
    }
    let changed = porcelain_paths(&host.git().status_of(&worktree, installer.files).await?);
    if changed.is_empty() {
        let head = host.git().head_commit(&worktree).await?;
        return head.ok_or_else(|| EngineError::Refused("the run branch has no commit".into()));
    }
    let subject = format!("Add dependencies: {}", packages.join(", "));
    let files: Vec<&str> = changed.iter().map(String::as_str).collect();
    let sha = conductor::commit(host, &worktree, &files, &subject).await?;
    host.record(
        None,
        None,
        RunEventKind::Merge(MergeEvent::DependenciesAdded {
            packages: packages.to_vec(),
            sha: sha.clone(),
        }),
    )?;
    Ok(sha)
}

/// Why Argonauts Code couldn't add these packages, checked against the repository root before
/// you approve, so you can add them yourself instead.
pub fn problem(root: &Path, packages: &[String]) -> EngineResult<Option<String>> {
    if packages.is_empty() {
        return Ok(None);
    }
    if let Some(bad) = packages.iter().find(|package| !valid_package(package)) {
        return Ok(Some(format!(
            "`{bad}` isn't a package name Argonauts Code will pass to a package manager."
        )));
    }
    Ok(installer(&root_files(root)?, packages).err())
}

/// How to add packages in one repository, and the files that may change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installer {
    pub run: Run,
    pub files: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// An argument vector run in the worktree.
    Command(Vec<String>),
    /// Packages appended one per line, for a plain requirements file.
    AppendLines(&'static str),
}

/// The package manager for the manifest at the repository root, with install scripts off.
/// More than one kind of manifest is ambiguous, so it's left to you.
pub fn installer(root_files: &[String], packages: &[String]) -> Result<Installer, String> {
    let has = |name: &str| root_files.iter().any(|file| file == name);
    let mut found = Vec::new();
    if has("package.json") {
        found.push(if has("pnpm-lock.yaml") {
            command(&["pnpm", "add", "--ignore-scripts"], packages, JS_FILES)
        } else if has("yarn.lock") {
            command(&["yarn", "add", "--ignore-scripts"], packages, JS_FILES)
        } else {
            command(
                &["npm", "install", "--save", "--ignore-scripts"],
                packages,
                JS_FILES,
            )
        });
    }
    if has("Cargo.toml") {
        found.push(command(
            &["cargo", "add"],
            packages,
            &["Cargo.toml", "Cargo.lock"],
        ));
    }
    if has("pyproject.toml") && has("uv.lock") {
        found.push(command(
            &["uv", "add", "--no-sync"],
            packages,
            &["pyproject.toml", "uv.lock"],
        ));
    } else if has("pyproject.toml") && has("poetry.lock") {
        found.push(command(
            &["poetry", "add", "--lock"],
            packages,
            &["pyproject.toml", "poetry.lock"],
        ));
    } else if has("requirements.txt") {
        found.push(Installer {
            run: Run::AppendLines("requirements.txt"),
            files: &["requirements.txt"],
        });
    }
    if has("go.mod") {
        found.push(command(&["go", "get"], packages, &["go.mod", "go.sum"]));
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!(
            "There's no package manifest at the repository root to add {} to.",
            packages.join(", ")
        )),
        _ => Err(format!(
            "The repository has more than one kind of package manifest, so add {} yourself.",
            packages.join(", ")
        )),
    }
}

const JS_FILES: &[&str] = &[
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
];

fn command(program: &[&str], packages: &[String], files: &'static [&'static str]) -> Installer {
    let argv = program
        .iter()
        .map(|arg| (*arg).to_owned())
        .chain(packages.iter().cloned())
        .collect();
    Installer {
        run: Run::Command(argv),
        files,
    }
}

async fn run(host: &Host, worktree: &Path, argv: &[String]) -> EngineResult<()> {
    let Some((program, args)) = argv.split_first() else {
        return Ok(());
    };
    // Install scripts stay off even where a flag can't say so.
    let child = Command::new(program)
        .args(args)
        .current_dir(worktree)
        .env("npm_config_ignore_scripts", "true")
        .env("YARN_ENABLE_SCRIPTS", "false")
        .env("CI", "1")
        .kill_on_drop(true)
        .output();
    let output = tokio::select! {
        _ = host.interrupted().cancelled() => return Err(EngineError::Interrupted),
        output = tokio::time::timeout(INSTALL_TIMEOUT, child) => output.map_err(|_| {
            EngineError::Refused(format!("`{}` took longer than {} minutes", argv.join(" "), INSTALL_TIMEOUT.as_secs() / 60))
        })??,
    };
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    Err(EngineError::Refused(format!(
        "`{}` failed: {}",
        argv.join(" "),
        tail.join(" / ")
    )))
}

fn append_lines(file: &Path, packages: &[String]) -> EngineResult<()> {
    let mut text = std::fs::read_to_string(file)?;
    let present: Vec<String> = text
        .lines()
        .map(|line| package_name(line.trim()).to_ascii_lowercase())
        .collect();
    for package in packages {
        if present.contains(&package_name(package).to_ascii_lowercase()) {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(package);
        text.push('\n');
    }
    std::fs::write(file, text)?;
    Ok(())
}

fn root_files(worktree: &Path) -> EngineResult<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(worktree)? {
        files.push(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(files)
}

/// The paths in `git status --porcelain` lines, the new name for a rename.
pub fn porcelain_paths(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| line.get(3..))
        .map(|path| {
            path.rsplit(" -> ")
                .next()
                .unwrap_or(path)
                .trim_matches('"')
                .to_owned()
        })
        .collect()
}

/// The name without a version: `ioredis@5`, `@scope/pkg@1`, `requests>=2`, `serde@1.0`.
fn package_name(package: &str) -> &str {
    let package = package.trim();
    let (scope, rest) = match package.strip_prefix('@') {
        Some(rest) => (1, rest),
        None => (0, package),
    };
    let end = rest
        .find(['@', '=', '<', '>', '~', '!', ' ', '[', ';'])
        .map_or(package.len(), |i| i + scope);
    &package[..end]
}

/// Nothing a package manager could read as an option or a second argument.
fn valid_package(package: &str) -> bool {
    !package.is_empty()
        && !package.starts_with('-')
        && !package.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn unlisted_by_name() {
        let allow = strings(&["ioredis", "@types/node", "Requests"]);
        let new = strings(&[
            "ioredis@5",
            "@types/node@20",
            "requests>=2",
            "left-pad",
            "left-pad",
        ]);
        assert_eq!(unlisted(&new, &allow), ["left-pad"]);
        assert!(unlisted(&[], &allow).is_empty());
    }

    #[test]
    fn one_installer_per_repository() {
        let packages = strings(&["ioredis"]);
        let npm = installer(&strings(&["package.json", "src"]), &packages).unwrap();
        assert_eq!(
            npm.run,
            Run::Command(strings(&[
                "npm",
                "install",
                "--save",
                "--ignore-scripts",
                "ioredis"
            ]))
        );
        let pnpm = installer(&strings(&["package.json", "pnpm-lock.yaml"]), &packages).unwrap();
        assert!(matches!(&pnpm.run, Run::Command(argv) if argv[0] == "pnpm"));
        let cargo = installer(&strings(&["Cargo.toml", "Cargo.lock"]), &packages).unwrap();
        assert_eq!(cargo.files, ["Cargo.toml", "Cargo.lock"]);
        let pip = installer(&strings(&["requirements.txt"]), &packages).unwrap();
        assert_eq!(pip.run, Run::AppendLines("requirements.txt"));
        assert!(installer(&strings(&["README.md"]), &packages).is_err());
        assert!(installer(&strings(&["package.json", "Cargo.toml"]), &packages).is_err());
    }

    #[test]
    fn changed_paths_from_status() {
        let lines = strings(&[
            " M package.json",
            "?? package-lock.json",
            "R  a.txt -> b.txt",
        ]);
        assert_eq!(
            porcelain_paths(&lines),
            ["package.json", "package-lock.json", "b.txt"]
        );
    }

    #[test]
    fn package_names() {
        assert_eq!(package_name("ioredis@5.3"), "ioredis");
        assert_eq!(package_name("@scope/pkg@1"), "@scope/pkg");
        assert_eq!(package_name("@scope/pkg"), "@scope/pkg");
        assert_eq!(package_name("requests[socks]>=2"), "requests");
        assert!(!valid_package("--registry=https://example.com"));
        assert!(!valid_package("a b"));
        assert!(valid_package("github.com/redis/go-redis/v9"));
    }

    #[test]
    fn requirements_are_appended_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("requirements.txt");
        std::fs::write(&file, "flask==3.0").unwrap();
        append_lines(&file, &strings(&["redis>=5", "Flask"])).unwrap();
        append_lines(&file, &strings(&["redis>=5"])).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "flask==3.0\nredis>=5\n"
        );
    }
}
