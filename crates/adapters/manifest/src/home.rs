//! A per-session home made from a template, so an agent's own memory, skills, and logins
//! never leak between sessions.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use argonauts_core::registry::cli::HomeTemplate;
use argonauts_core::session::SpecFile;

/// Templates hold config, not data, so anything bigger is a mistake worth stopping on.
const MAX_FILES: usize = 256;
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Written when the template is empty, since Argonauts Code only creates a file's parents and
/// the home must still exist.
const KEEP_FILE: &str = ".keep";

/// The template's files from `templates_root/<template>/`, to be written under `home`.
/// Template names with path separators or `..` are refused, and so are symlinks inside the
/// template, which could reach into a repo or copy a secret.
pub(crate) fn home_files(
    template: &HomeTemplate,
    templates_root: &Path,
    home: &Path,
) -> anyhow::Result<Vec<SpecFile>> {
    check_template_name(&template.template)?;
    let source = templates_root.join(&template.template);
    let meta = fs::symlink_metadata(&source)
        .with_context(|| format!("home template {}", source.display()))?;
    if !meta.is_dir() {
        bail!("home template {} isn't a folder", source.display());
    }
    let mut found = Vec::new();
    collect(&source, Path::new(""), &mut found)?;
    let mut total = 0;
    let mut files = Vec::with_capacity(found.len().max(1));
    for (relative, path) in found {
        if files.len() == MAX_FILES {
            bail!(
                "home template {} has more than {MAX_FILES} files",
                source.display()
            );
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("home template file {}", path.display()))?;
        total += contents.len();
        if total > MAX_BYTES {
            bail!(
                "home template {} is larger than {} MiB",
                source.display(),
                MAX_BYTES / (1024 * 1024)
            );
        }
        files.push(SpecFile {
            path: home.join(relative),
            contents,
        });
    }
    if files.is_empty() {
        files.push(SpecFile {
            path: home.join(KEEP_FILE),
            contents: String::new(),
        });
    }
    Ok(files)
}

/// One plain folder name, so the template can't be looked up outside the templates root.
pub(crate) fn check_template_name(name: &str) -> anyhow::Result<()> {
    let mut components = Path::new(name).components();
    let plain = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    if !plain || name.contains(['/', '\\', '\0']) || name.contains("..") {
        bail!("home template {name:?} must be one folder name, without / or ..");
    }
    Ok(())
}

/// Regular files under `dir`, as (path inside the template, path on disk), sorted so the
/// spec is the same on every run.
fn collect(dir: &Path, prefix: &Path, found: &mut Vec<(PathBuf, PathBuf)>) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("home template {}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("home template {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = prefix.join(entry.file_name());
        let kind = entry
            .file_type()
            .with_context(|| format!("home template file {}", path.display()))?;
        if kind.is_dir() {
            collect(&path, &relative, found)?;
        } else if kind.is_file() {
            found.push((relative, path));
        } else {
            bail!(
                "home template {} holds {}, which isn't a plain file or folder",
                dir.display(),
                relative.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::registry::cli::HomeKeep;

    fn template(name: &str) -> HomeTemplate {
        HomeTemplate {
            env: "AGENT_HOME".into(),
            template: name.into(),
            keep: HomeKeep::Session,
        }
    }

    #[test]
    fn copies_nested_files_in_order() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("agent");
        fs::create_dir_all(source.join("skills")).unwrap();
        fs::write(source.join("config.yaml"), "a: 1\n").unwrap();
        fs::write(source.join("skills/none.md"), "none\n").unwrap();
        let files = home_files(&template("agent"), root.path(), Path::new("/s/home")).unwrap();
        assert_eq!(
            files,
            [
                SpecFile {
                    path: "/s/home/config.yaml".into(),
                    contents: "a: 1\n".into()
                },
                SpecFile {
                    path: "/s/home/skills/none.md".into(),
                    contents: "none\n".into()
                },
            ]
        );
    }

    #[test]
    fn an_empty_template_still_makes_the_home() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("bare")).unwrap();
        let files = home_files(&template("bare"), root.path(), Path::new("/s/home")).unwrap();
        assert_eq!(files[0].path, Path::new("/s/home/.keep"));
    }

    #[test]
    fn refuses_names_that_leave_the_root() {
        for name in ["", ".", "..", "../x", "a/b", "a\\b", "/abs", "x..y"] {
            assert!(check_template_name(name).is_err(), "{name:?}");
        }
        assert!(check_template_name("hermes").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_missing_template_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let err = home_files(&template("gone"), root.path(), Path::new("/h")).unwrap_err();
        assert!(err.to_string().contains("gone"), "{err}");

        let source = root.path().join("linked");
        fs::create_dir(&source).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", source.join("hosts")).unwrap();
        let err = home_files(&template("linked"), root.path(), Path::new("/h")).unwrap_err();
        assert!(err.to_string().contains("plain file"), "{err}");

        std::os::unix::fs::symlink(&source, root.path().join("alias")).unwrap();
        let err = home_files(&template("alias"), root.path(), Path::new("/h")).unwrap_err();
        assert!(err.to_string().contains("isn't a folder"), "{err}");
    }
}
