//! Ctrl+G: the draft in `$VISUAL` or `$EDITOR`, run as an argument vector while the screen is
//! handed over.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the edited draft. The caller runs this inside `Ui::with_restored`.
pub fn edit_in_editor(draft: &str) -> Result<String, EditorError> {
    let editor = editor_command(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
    )?;
    edit_with(&editor, draft, &std::env::temp_dir())
}

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("neither $VISUAL nor $EDITOR is set")]
    NoEditor,
    #[error("can't read the editor command `{0}`")]
    BadCommand(String),
    #[error("the editor exited with {0}, so the draft is unchanged")]
    Failed(std::process::ExitStatus),
    #[error("the editor failed: {0}")]
    Io(#[from] std::io::Error),
}

/// `$VISUAL` first, then `$EDITOR`, each split like a shell would split it but never run
/// through one, so `code --wait` works and `vim; rm -rf ~` is just odd arguments.
pub fn editor_command(
    visual: Option<&str>,
    editor: Option<&str>,
) -> Result<Vec<String>, EditorError> {
    let chosen = [visual, editor]
        .into_iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
        .ok_or(EditorError::NoEditor)?;
    let argv =
        shell_words::split(chosen).map_err(|_| EditorError::BadCommand(chosen.to_owned()))?;
    if argv.is_empty() {
        return Err(EditorError::NoEditor);
    }
    Ok(argv)
}

/// Writes the draft to a private file in `dir`, runs `editor` with its path as the last
/// argument, and reads it back. One trailing newline, which editors add, comes off.
pub fn edit_with(editor: &[String], draft: &str, dir: &Path) -> Result<String, EditorError> {
    let (program, args) = editor.split_first().ok_or(EditorError::NoEditor)?;
    let file = DraftFile::create(dir, draft)?;
    let status = Command::new(program).args(args).arg(&file.path).status()?;
    if !status.success() {
        return Err(EditorError::Failed(status));
    }
    let edited = fs::read_to_string(&file.path)?;
    let edited = edited
        .strip_suffix("\r\n")
        .or_else(|| edited.strip_suffix('\n'))
        .unwrap_or(&edited);
    Ok(edited.to_owned())
}

/// Removed when dropped, whatever the editor did.
struct DraftFile {
    path: PathBuf,
}

impl DraftFile {
    fn create(dir: &Path, draft: &str) -> std::io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = dir.join(format!("argonauts-draft-{}-{stamp}.md", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // The draft may hold something private, and the temp dir is shared.
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(draft.as_bytes())?;
        Ok(Self { path })
    }
}

impl Drop for DraftFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn visual_wins_and_splits_without_a_shell() {
        assert_eq!(
            editor_command(Some("code --wait"), Some("vim")).unwrap(),
            ["code", "--wait"]
        );
        assert_eq!(editor_command(Some("  "), Some("vim")).unwrap(), ["vim"]);
        assert_eq!(
            editor_command(None, Some("vim; rm -rf ~")).unwrap(),
            ["vim;", "rm", "-rf", "~"]
        );
        assert!(matches!(
            editor_command(None, None),
            Err(EditorError::NoEditor)
        ));
        assert!(matches!(
            editor_command(Some("vim 'open"), None),
            Err(EditorError::BadCommand(_))
        ));
    }

    #[test]
    fn the_edited_draft_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        // `sed -i.bak` works on both BSD and GNU sed; the path comes last.
        let editor = ["sed", "-i.bak", "s/draft/edited draft/"].map(String::from);
        assert_eq!(
            edit_with(&editor, "the draft\n", dir.path()).unwrap(),
            "the edited draft"
        );
        let failing = ["false".to_owned()];
        assert!(matches!(
            edit_with(&failing, "kept", dir.path()),
            Err(EditorError::Failed(_))
        ));
        let left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".md"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}
