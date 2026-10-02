//! `argonauts skills import`, driven through the real binary: an import only ever proposes a
//! branch, and nothing it brings in runs.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use argonauts_testkit::{TempRepo, argonauts_bin};

fn write(root: &Path, path: &str, contents: &str) -> anyhow::Result<()> {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))
}

#[test]
fn skills_import_opens_pull_request_never_runs_scripts() -> anyhow::Result<()> {
    let repo = TempRepo::new()?;
    let marker = repo.scratch().join("script-ran");
    let source = repo.scratch().join("incoming/setup-helper");
    let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
    write(
        &source,
        "SKILL.md",
        &format!(
            "---\nname: setup-helper\ndescription: Sets up the dev environment.\nhooks:\n  \
             post-install: sh scripts/setup.sh\n---\n# Setup\n\nRun `sh scripts/setup.sh` \
             first.\n\n```sh\ntouch '{}'\n```\n",
            marker.display()
        ),
    )?;
    write(&source, "scripts/setup.sh", &script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = source.join("scripts/setup.sh");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    let head = repo.head()?;

    let output = Command::new(argonauts_bin()?)
        .args(["skills", "import"])
        .arg(&source)
        .current_dir(repo.root())
        .envs(repo.envs())
        .env("CI", "true")
        .output()
        .context("running argonauts skills import")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");

    assert!(!marker.exists(), "an imported script ran");
    let branch = "argonauts/skills/import-setup-helper";
    repo.git(&["rev-parse", "--verify", branch])?;
    let listed = repo.git(&["ls-tree", "-r", "--full-tree", branch])?;
    assert!(
        listed.contains("\t.agents/skills/setup-helper/scripts/setup.sh"),
        "{listed}"
    );
    assert!(
        !listed.contains("100755"),
        "scripts land without execute bits: {listed}"
    );
    assert!(listed.contains("\t.claude/skills/setup-helper"), "{listed}");

    // The proposal waits on its branch: your checkout and branch are untouched.
    assert_eq!(repo.head()?, head);
    assert_eq!(repo.git(&["status", "--porcelain"])?, "");
    assert!(!repo.root().join(".agents").exists());
    assert!(
        stdout.contains("gh pr create") || stdout.contains(branch),
        "{stdout}"
    );
    assert!(!marker.exists(), "an imported script ran");
    Ok(())
}
