//! A per-session home, made from a template Argonauts owns, so the agent's own memory, skills,
//! and logins never leak between sessions or from your own home.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::SpecFile;
use argonauts_core::layout::user_files::HARNESSES;

/// The template's files, placed under `home`. The worker writes them before the spawn.
pub(crate) fn home_files(
    argonauts_home: &Path,
    template: &str,
    home: &Path,
) -> anyhow::Result<Vec<SpecFile>> {
    let mut parts = Path::new(template).components();
    let plain = matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    );
    if !plain {
        bail!("the home template {template:?} must be a folder name in ~/.argonauts/{HARNESSES}");
    }
    let root = argonauts_home.join(HARNESSES).join(template);
    if !root.is_dir() {
        bail!("the home template {} is missing", root.display());
    }
    let mut files = Vec::new();
    collect(&root, &root, home, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn collect(
    root: &Path,
    folder: &Path,
    home: &Path,
    files: &mut Vec<SpecFile>,
) -> anyhow::Result<()> {
    let entries = fs::read_dir(folder).with_context(|| format!("reading {}", folder.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", folder.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .with_context(|| format!("reading {}", path.display()))?;
        // A link could pull in any file of yours, so templates hold plain files only.
        if kind.is_symlink() {
            bail!(
                "the home template holds a link, {}, which isn't copied",
                path.display()
            );
        }
        if kind.is_dir() {
            collect(root, &path, home, files)?;
            continue;
        }
        let contents = fs::read_to_string(&path).with_context(|| {
            format!(
                "reading {} (home templates hold text files only)",
                path.display()
            )
        })?;
        let relative: PathBuf = path.strip_prefix(root)?.to_owned();
        files.push(SpecFile {
            path: home.join(relative),
            contents,
        });
    }
    Ok(())
}
