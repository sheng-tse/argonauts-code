//! A worker's writes outside the project. Each leaves one quiet note in the run once the CLI
//! says the file changed, as a direct turn's leaves a line in the conversation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use argonauts_core::AgentEvent;
use argonauts_core::names::SessionKey;
use argonauts_core::outside::changed_outside;

#[derive(Debug, Default)]
pub struct OutsideWrites {
    /// Each running session's own folder, where a relative path points.
    folders: HashMap<SessionKey, PathBuf>,
}

impl OutsideWrites {
    pub fn began(&mut self, key: &SessionKey, folder: &Path) {
        self.folders.insert(key.clone(), folder.to_path_buf());
    }

    /// The note for `event`, when it's a file the session changed outside its own folder and
    /// every folder of the project in `project`.
    pub fn follow(
        &self,
        key: &SessionKey,
        event: &AgentEvent,
        project: &[&Path],
        home: Option<&Path>,
    ) -> Option<String> {
        if !matches!(event, AgentEvent::FileChanged { .. }) {
            return None;
        }
        let own = self.folders.get(key).map(PathBuf::as_path);
        let inside: Vec<&Path> = own.into_iter().chain(project.iter().copied()).collect();
        changed_outside(&inside, event, home)
    }

    /// Forgets a session that ended.
    pub fn ended(&mut self, key: &SessionKey) {
        self.folders.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(path: &str) -> AgentEvent {
        AgentEvent::FileChanged {
            path: path.into(),
            kind: "add".into(),
        }
    }

    #[test]
    fn a_worker_write_outside_the_project_and_its_worktree_is_said_once_done() {
        let key = SessionKey::parse("s-001").unwrap();
        let tree = Path::new("/home/you/.argonauts/worktrees/app/run-1/task-a");
        let project = [Path::new("/home/you/app")];
        let home = Some(Path::new("/home/you"));
        let mut writes = OutsideWrites::default();
        writes.began(&key, tree);
        let follow = |event: AgentEvent| writes.follow(&key, &event, &project, home);
        assert_eq!(
            follow(changed("/home/you/Downloads/notes.md")),
            Some("wrote ~/Downloads/notes.md, outside the project".to_owned())
        );
        assert_eq!(follow(changed("src/lib.rs")), None, "its worktree");
        assert_eq!(
            follow(changed("/home/you/app/README.md")),
            None,
            "the project"
        );
        let started = AgentEvent::ToolStarted {
            id: "w1".into(),
            name: "Write".into(),
            input: serde_json::json!({ "file_path": "/home/you/Downloads/notes.md" }),
            parent: None,
        };
        assert_eq!(follow(started), None, "a write still under way");
    }
}
