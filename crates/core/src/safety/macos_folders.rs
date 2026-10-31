//! macOS keeping a CLI out of your Desktop, Documents or Downloads folder, read from what the
//! CLI says, and where in System Settings you allow it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A folder macOS asks you about before an app reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Folder {
    Desktop,
    Documents,
    Downloads,
}

impl Folder {
    const ALL: [Folder; 3] = [Folder::Desktop, Folder::Documents, Folder::Downloads];

    pub fn name(self) -> &'static str {
        match self {
            Folder::Desktop => "Desktop",
            Folder::Documents => "Documents",
            Folder::Downloads => "Downloads",
        }
    }

    /// The protected folder `path` is in, under `home`.
    fn holding(path: &Path, home: &Path) -> Option<Folder> {
        Folder::ALL
            .into_iter()
            .find(|folder| path.starts_with(home.join(folder.name())))
    }
}

/// macOS kept `app` out of `folder`: the app the message names, or your terminal, which macOS
/// asks about for a CLI that names none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderBlocked {
    pub folder: Folder,
    pub app: String,
}

/// agy 1.4's line, "failed to read directory /Users/you/Downloads/notes. macOS privacy settings
/// are blocking Antigravity", whose last words name the app macOS asks about.
const FAILED_TO_READ: &str = "failed to read directory";
const BLOCKING: &str = "macos privacy settings are blocking ";

const NOT_PERMITTED: &str = ": operation not permitted";

/// The words after who it is in the line that says a folder is blocked, by which the screen
/// knows the line.
pub const CANT_READ_THIS_FOLDER: &str = " can't read this folder: macOS keeps ";

/// Starts the line under it that says where to let the CLI in.
pub const TO_LET_IT_IN: &str = "To let it in: ";

impl FolderBlocked {
    /// Reads one piece of a CLI's output in a session working in `workdir`. Plain "Operation
    /// not permitted" counts only on the session's own folder or the protected folder itself,
    /// which no sandbox of ours refuses to list, so a write the sandbox turned away never
    /// reads as macOS.
    pub fn read(said: &str, workdir: &Path, home: &Path, terminal: &str) -> Option<Self> {
        if let Some(named) = said.lines().find_map(agy_blocking) {
            let folder = named_paths(said)
                .find_map(|path| Folder::holding(&path, home))
                .or_else(|| Folder::holding(workdir, home))?;
            let app = app_named(named).unwrap_or_else(|| terminal.to_owned());
            return Some(Self { folder, app });
        }
        let folder = Folder::holding(workdir, home)?;
        let protected = home.join(folder.name());
        named_paths(said)
            .filter(|path| *path == workdir || *path == protected)
            .any(|path| {
                let at = format!("{}{NOT_PERMITTED}", path.display());
                find_ignoring_case(said, &at).is_some()
            })
            .then(|| Self {
                folder,
                app: terminal.to_owned(),
            })
    }

    /// "3.8 Flash can't read this folder: macOS keeps agy out of Downloads", for `who` on `cli`.
    pub fn words(&self, who: &str, cli: &str) -> String {
        format!(
            "{who}{CANT_READ_THIS_FOLDER}{cli} out of {}",
            self.folder.name()
        )
    }

    /// Where you let the app in: "System Settings › Privacy & Security › Files and Folders ›
    /// Antigravity › Downloads Folder".
    pub fn where_to_allow(&self) -> String {
        format!(
            "System Settings › Privacy & Security › Files and Folders › {} › {} Folder",
            self.app,
            self.folder.name()
        )
    }
}

/// What follows "blocking " when `line` is agy's whole line about a folder macOS kept it out of.
fn agy_blocking(line: &str) -> Option<&str> {
    let failed = find_ignoring_case(line, FAILED_TO_READ)?;
    let rest = &line[failed + FAILED_TO_READ.len()..];
    let at = find_ignoring_case(rest, BLOCKING)?;
    Some(&rest[at + BLOCKING.len()..])
}

/// The app agy's line names after "blocking": its words up to the first lowercase word or path,
/// so "Antigravity from reading /Users/you/Downloads" names `Antigravity`.
fn app_named(named: &str) -> Option<String> {
    let mut words = Vec::new();
    for word in named.split_whitespace() {
        let word = word.trim_start_matches(['"', '\'', '(']);
        let bare = word.trim_end_matches(['.', ',', ';', ':', '"', '\'', ')']);
        let lowercase = !bare.chars().any(char::is_uppercase);
        if bare.is_empty() || bare.starts_with(['/', '~']) || lowercase {
            break;
        }
        words.push(bare);
        if bare.len() < word.len() {
            break;
        }
    }
    (!words.is_empty()).then(|| words.join(" "))
}

/// Where `needle` first shows in `said`, ignoring ASCII case, with no lowercase copy. A match
/// starts on the needle's first byte, ASCII here, so the offset is a char boundary in `said`.
fn find_ignoring_case(said: &str, needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return Some(0);
    }
    said.as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// The absolute paths `said` names, without the punctuation around them.
fn named_paths(said: &str) -> impl Iterator<Item = PathBuf> + '_ {
    said.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ')'))
        .map(|word| word.trim_end_matches([':', ',', '.', ';']))
        .filter(|word| word.starts_with('/'))
        .map(PathBuf::from)
}

/// The app macOS names for the terminal you run in, from `TERM_PROGRAM`.
pub fn terminal_app(term_program: Option<&str>) -> String {
    let program = term_program.unwrap_or_default().trim();
    match program.to_ascii_lowercase().as_str() {
        "apple_terminal" => "Terminal".to_owned(),
        "iterm.app" => "iTerm".to_owned(),
        "ghostty" => "Ghostty".to_owned(),
        "wezterm" => "WezTerm".to_owned(),
        "vscode" => "Visual Studio Code".to_owned(),
        "warpterminal" => "Warp".to_owned(),
        "" => "your terminal".to_owned(),
        _ => program.trim_end_matches(".app").to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/you";

    fn read(said: &str, workdir: &str) -> Option<FolderBlocked> {
        FolderBlocked::read(said, Path::new(workdir), Path::new(HOME), "Ghostty")
    }

    #[test]
    fn agy_names_the_app_and_the_session_names_the_folder() {
        let blocked = read(
            "failed to read directory. macOS privacy settings are blocking Antigravity",
            "/Users/you/Downloads/app",
        )
        .unwrap();
        assert_eq!(blocked.folder, Folder::Downloads);
        assert_eq!(blocked.app, "Antigravity");
        assert_eq!(
            blocked.where_to_allow(),
            "System Settings › Privacy & Security › Files and Folders › Antigravity › \
             Downloads Folder"
        );
    }

    #[test]
    fn words_before_the_message_that_change_length_in_lower_case_are_read_past() {
        let blocked = read(
            "İstanbul: failed to read directory. macOS privacy settings are blocking Antigravity",
            "/Users/you/Downloads/app",
        )
        .unwrap();
        assert_eq!(blocked.app, "Antigravity");
        assert_eq!(
            blocked.words("3.8 Flash", "agy"),
            "3.8 Flash can't read this folder: macOS keeps agy out of Downloads"
        );
    }

    #[test]
    fn a_path_the_message_names_wins_over_the_session_folder() {
        let blocked = read(
            "failed to read directory /Users/you/Desktop/notes. macOS privacy settings are \
             blocking Antigravity.",
            "/Users/you/Downloads/app",
        )
        .unwrap();
        assert_eq!(blocked.folder, Folder::Desktop);
    }

    #[test]
    fn operation_not_permitted_on_the_folder_names_your_terminal() {
        let blocked = read(
            "ls: /Users/you/Documents/app: Operation not permitted",
            "/Users/you/Documents/app",
        )
        .unwrap();
        assert_eq!(blocked.folder, Folder::Documents);
        assert_eq!(blocked.app, "Ghostty");
    }

    #[test]
    fn a_refused_write_inside_or_a_folder_elsewhere_is_not_macos() {
        // The sandbox turns writes away in the same words.
        assert_eq!(
            read(
                "cp: /Users/you/Downloads/app/out.txt: Operation not permitted",
                "/Users/you/Downloads/app"
            ),
            None
        );
        assert_eq!(
            read(
                "ls: /Users/you/code/app: Operation not permitted",
                "/Users/you/code/app"
            ),
            None
        );
        assert_eq!(
            read(
                "failed to read directory. macOS privacy settings are blocking Antigravity",
                "/Users/you/code/app"
            ),
            None
        );
    }

    #[test]
    fn the_app_is_named_up_to_the_first_lowercase_word_or_path() {
        let app = |said: &str| read(said, "/Users/you/Downloads/app").map(|blocked| blocked.app);
        let line = |rest: &str| {
            format!("failed to read directory. macOS privacy settings are blocking {rest}")
        };
        assert_eq!(
            app(&line("Antigravity from reading /Users/you/Downloads")),
            Some("Antigravity".to_owned())
        );
        assert_eq!(
            app(&line("Visual Studio Code, so ask again")),
            Some("Visual Studio Code".to_owned())
        );
        assert_eq!(app(&line("iTerm. Allow it")), Some("iTerm".to_owned()));
        assert_eq!(
            app(&line("/Users/you/Downloads")),
            Some("Ghostty".to_owned())
        );
        assert_eq!(app(&line("")), Some("Ghostty".to_owned()));
    }

    /// A file that quotes agy's words, read by a call that went through, says nothing of
    /// macOS: only the whole line counts.
    #[test]
    fn the_words_alone_without_agys_line_are_not_macos() {
        assert_eq!(
            read(
                "const BLOCKING: &str = \"macos privacy settings are blocking \";",
                "/Users/you/Downloads/app"
            ),
            None
        );
    }

    #[test]
    fn the_terminal_is_named_as_macos_lists_it() {
        assert_eq!(terminal_app(Some("Apple_Terminal")), "Terminal");
        assert_eq!(terminal_app(Some("iTerm.app")), "iTerm");
        assert_eq!(terminal_app(Some("ghostty")), "Ghostty");
        assert_eq!(terminal_app(None), "your terminal");
    }
}
