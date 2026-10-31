//! A worker's writes outside the project, and what the never-list kept from it. Each leaves one
//! quiet note in the run once the CLI says the file changed, a command that changed it ended,
//! or the call was refused, as a direct turn's leaves a line in the conversation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use argonauts_core::AgentEvent;
use argonauts_core::base::names::SessionKey;
use argonauts_core::safety::outside::{BeforeWrites, KeptOut, Never, both};

use crate::engine::host::Host;
use crate::engine::shared::lock;

#[derive(Debug, Default)]
pub struct OutsideWrites {
    /// Each running session's own folder, where a relative path points.
    folders: HashMap<SessionKey, PathBuf>,
    /// Each running session's never-list, and its calls that named a path on it.
    kept_out: HashMap<SessionKey, (Never, KeptOut)>,
    /// How each file a running session's write calls named looked as each started.
    before_writes: HashMap<SessionKey, BeforeWrites>,
}

impl OutsideWrites {
    pub fn began(&mut self, key: &SessionKey, folder: &Path, never: Never) {
        self.folders.insert(key.clone(), folder.to_path_buf());
        self.kept_out
            .insert(key.clone(), (never, KeptOut::default()));
        self.before_writes
            .insert(key.clone(), BeforeWrites::default());
    }

    /// The note for `event`: a file the session changed outside its own folder and every
    /// folder of the project in `project`, by a file tool or a command, or a call the
    /// never-list refused.
    pub fn follow(
        &mut self,
        key: &SessionKey,
        event: &AgentEvent,
        project: &[&Path],
        home: Option<&Path>,
    ) -> Option<String> {
        let own = self.folders.get(key).map(PathBuf::as_path);
        let inside: Vec<&Path> = own.into_iter().chain(project.iter().copied()).collect();
        if matches!(event, AgentEvent::FileChanged { .. }) {
            return match self.before_writes.get_mut(key) {
                Some(before) => before.changed_as_reported(&inside, event, home),
                None => BeforeWrites::default().changed_as_reported(&inside, event, home),
            };
        }
        let folder = own?;
        let wrote = self.before_writes.get_mut(key).and_then(|before| {
            before.note(folder, event, home);
            before.changed_by_command(&inside, event, home)
        });
        let (never, kept_out) = self.kept_out.get_mut(key)?;
        both(kept_out.follow(never, folder, event, home), wrote)
    }

    /// The folder a running session works in.
    pub fn folder(&self, key: &SessionKey) -> Option<&Path> {
        self.folders.get(key).map(PathBuf::as_path)
    }

    /// Forgets a session that ended.
    pub fn ended(&mut self, key: &SessionKey) {
        self.folders.remove(key);
        self.kept_out.remove(key);
        self.before_writes.remove(key);
    }
}

impl Host {
    /// The line a worker's write outside the project leaves once it's done. Its own folder,
    /// your checkout, the run's worktrees, and Argonauts Code's state are the project's.
    pub(super) fn outside_note(
        &self,
        key: &SessionKey,
        event: &argonauts_core::AgentEvent,
    ) -> Option<String> {
        let repo = &self.shared().repo;
        let state = repo.run_dir.ancestors().nth(3);
        let project: Vec<&Path> = [
            Some(repo.root.as_path()),
            Some(repo.worktrees.as_path()),
            state,
        ]
        .into_iter()
        .flatten()
        .collect();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        lock(&self.shared().outside_writes).follow(key, event, &project, home.as_deref())
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

    /// A write outside leaves its note once the file is there, and one the CLI only said it made
    /// leaves none.
    #[test]
    fn a_worker_write_outside_the_project_and_its_worktree_is_said_once_done() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        let notes = you.join("Downloads/notes.md");
        std::fs::create_dir_all(notes.parent().unwrap()).unwrap();
        std::fs::write(&notes, "made up").unwrap();
        let key = SessionKey::parse("s-001").unwrap();
        let tree = you.join(".argonauts/worktrees/app/run-1/task-a");
        let app = you.join("app");
        let project = [app.as_path()];
        let home = Some(you.as_path());
        let mut writes = OutsideWrites::default();
        writes.began(&key, &tree, Never::default());
        let mut follow = |event: AgentEvent| writes.follow(&key, &event, &project, home);
        assert_eq!(
            follow(changed(&notes.display().to_string())),
            Some("wrote ~/Downloads/notes.md, outside the project".to_owned())
        );
        assert_eq!(
            follow(changed(
                &you.join("Downloads/refused.md").display().to_string()
            )),
            None,
            "not on the disk"
        );
        assert_eq!(follow(changed("src/lib.rs")), None, "its worktree");
        assert_eq!(
            follow(changed(&app.join("README.md").display().to_string())),
            None,
            "the project"
        );
        let started = AgentEvent::ToolStarted {
            id: "w1".into(),
            name: "Write".into(),
            input: serde_json::json!({ "file_path": notes }),
            parent: None,
        };
        assert_eq!(follow(started), None, "a write still under way");
    }

    /// A worker's write reported to a file that was there before its session leaves its note
    /// only once the file's size or time changed.
    #[test]
    fn a_worker_write_to_a_file_already_there_is_said_once_it_changed() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        let notes = you.join("Downloads/notes.md");
        std::fs::create_dir_all(notes.parent().unwrap()).unwrap();
        std::fs::write(&notes, "made up").unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let file = std::fs::File::options().write(true).open(&notes).unwrap();
        file.set_modified(hour_ago).unwrap();
        let key = SessionKey::parse("s-003").unwrap();
        let tree = you.join(".argonauts/worktrees/app/run-1/task-a");
        let home = Some(you.as_path());
        let mut writes = OutsideWrites::default();
        writes.began(&key, &tree, Never::default());
        let started = AgentEvent::ToolStarted {
            id: "w1".into(),
            name: "Edit".into(),
            input: serde_json::json!({ "file_path": notes }),
            parent: None,
        };
        let edited = AgentEvent::FileChanged {
            path: notes.display().to_string(),
            kind: "update".into(),
        };
        assert_eq!(writes.follow(&key, &started, &[], home), None);
        assert_eq!(writes.follow(&key, &edited, &[], home), None, "as it was");
        std::fs::write(&notes, "made up, and more").unwrap();
        assert_eq!(
            writes.follow(&key, &edited, &[], home),
            Some("wrote ~/Downloads/notes.md, outside the project".to_owned())
        );
    }

    /// A worker's command leaves a note for what it changed outside the project once it
    /// ends, and none for what it changed in the project.
    #[test]
    fn a_worker_command_that_wrote_outside_the_project_is_said_once_it_ends() {
        let temp = tempfile::tempdir().unwrap();
        let you = temp.path().canonicalize().unwrap();
        let key = SessionKey::parse("s-004").unwrap();
        let tree = you.join(".argonauts/worktrees/app/run-1/task-a");
        let app = you.join("app");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(you.join("Downloads")).unwrap();
        let project = [app.as_path()];
        let home = Some(you.as_path());
        let mut writes = OutsideWrites::default();
        writes.began(&key, &tree, Never::default());
        let started = AgentEvent::ToolStarted {
            id: "c1".into(),
            name: "exec_command".into(),
            input: serde_json::json!({
                "command": ["bash", "-lc", "cp notes.md ~/Downloads/notes.md ~/app/notes.md"]
            }),
            parent: None,
        };
        assert_eq!(writes.follow(&key, &started, &project, home), None);
        std::fs::write(you.join("Downloads/notes.md"), "made up").unwrap();
        std::fs::write(app.join("notes.md"), "made up").unwrap();
        let finished = AgentEvent::ToolFinished {
            id: "c1".into(),
            ok: true,
            output: Some(String::new()),
            parent: None,
        };
        assert_eq!(
            writes.follow(&key, &finished, &project, home),
            Some("wrote ~/Downloads/notes.md, outside the project".to_owned())
        );
    }

    /// A worker's read the never-list refused leaves one note after the CLI's own words, and
    /// a session that ended is forgotten.
    #[test]
    fn a_worker_read_the_never_list_refused_is_kept_out() {
        let key = SessionKey::parse("s-002").unwrap();
        let tree = Path::new("/home/you/.argonauts/worktrees/app/run-1/task-a");
        let home = Path::new("/home/you");
        let never = Never::resolve(&[".env".to_owned()], home);
        let mut writes = OutsideWrites::default();
        writes.began(&key, tree, never);
        let mut follow = |event: AgentEvent| writes.follow(&key, &event, &[], Some(home));
        let started = AgentEvent::ToolStarted {
            id: "r1".into(),
            name: "read".into(),
            input: serde_json::json!({ "filePath": "/home/you/app/.env" }),
            parent: None,
        };
        assert_eq!(follow(started), None);
        let refused = AgentEvent::ToolFinished {
            id: "r1".into(),
            ok: false,
            output: Some("Unknown: FileSystem.stat".into()),
            parent: None,
        };
        assert_eq!(
            follow(refused.clone()),
            Some("kept out: ~/app/.env is on the never-list".to_owned())
        );
        writes.ended(&key);
        assert_eq!(writes.follow(&key, &refused, &[], Some(home)), None);
    }
}
