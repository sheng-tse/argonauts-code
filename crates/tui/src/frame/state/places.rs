//! Where paths are shown from: the conversation's folder, your home, a run's worktree.

use super::AppState;

/// Where a call's paths are shown from, as Claude Code shows them: relative to the folder the
/// work happens in, `~/…` under your home, and whole only outside both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Places {
    pub folder: Option<String>,
    pub home: Option<String>,
}

impl Places {
    /// Your home alone, for a call whose folder isn't known here. A run's worktree still
    /// shows its paths from inside it.
    pub fn here() -> Self {
        Self {
            folder: None,
            home: dirs::home_dir().map(|home| home.display().to_string()),
        }
    }

    /// The folder the conversation works in, as the startup header names it with `~` for
    /// your home.
    pub fn of_conversation(state: &AppState) -> Self {
        let mut places = Self::here();
        places.folder = state.welcome.as_ref().map(|welcome| {
            match (welcome.repo.strip_prefix('~'), &places.home) {
                (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                    format!("{home}{rest}")
                }
                _ => welcome.repo.clone(),
            }
        });
        places
    }

    pub fn show(&self, path: &str) -> String {
        let path = path.strip_prefix("file://").unwrap_or(path);
        if let Some(inner) = in_worktree(path) {
            return inner.to_owned();
        }
        if let Some(folder) = self.folder.as_deref() {
            match under(path, folder) {
                Some("") => return ".".to_owned(),
                Some(rest) => return rest.to_owned(),
                None => {}
            }
        }
        if let Some(home) = self.home.as_deref() {
            match under(path, home) {
                // The home folder itself keeps its full path, as Claude Code's header shows it.
                Some("") => return path.trim_end_matches('/').to_owned(),
                Some(rest) => return format!("~/{rest}"),
                None => {}
            }
        }
        path.to_owned()
    }
}

/// What follows `folder` in `path`, empty for the folder itself.
fn under<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    let folder = folder.trim_end_matches('/');
    if folder.is_empty() {
        return None;
    }
    let rest = path.strip_prefix(folder)?;
    if rest.is_empty() || rest == "/" {
        return Some("");
    }
    rest.strip_prefix('/')
}

/// Worktrees sit at `<home>/worktrees/<repo>/<run>/<task>/`, and the block's header already
/// names the task, so a path inside one starts after it.
fn in_worktree(path: &str) -> Option<&str> {
    let at = path.find("/worktrees/")?;
    let mut parts = path[at + "/worktrees/".len()..].splitn(4, '/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(_), Some(_), Some(_), Some(inner)) if !inner.is_empty() => Some(inner),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_home_folder_shows_whole_and_a_folder_under_it_from_home() {
        let places = Places {
            folder: Some("/Users/you/code/tasks-api".into()),
            home: Some("/Users/you".into()),
        };
        assert_eq!(places.show("/Users/you"), "/Users/you");
        assert_eq!(places.show("/Users/you/"), "/Users/you");
        assert_eq!(places.show("/Users/you/notes/todo.md"), "~/notes/todo.md");
        assert_eq!(places.show("/Users/you/code/tasks-api/src"), "src");

        // A folder whose name only starts with the home's is outside it.
        let places = Places {
            folder: None,
            home: Some("/Users/you/work".into()),
        };
        assert_eq!(
            places.show("/Users/you/workshop/a.md"),
            "/Users/you/workshop/a.md"
        );
    }
}
