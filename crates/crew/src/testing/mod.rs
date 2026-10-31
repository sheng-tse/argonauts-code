//! Test helpers: a temporary git repository isolated from your git config, and tasks and events
//! to fold.

use std::path::PathBuf;

use argonauts_core::base::names::SessionKey;
use argonauts_core::runs::events::task::{TaskKind, TaskLimit, TaskSpec};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::{RunId, TaskId};

pub(crate) fn git_test_env() -> Vec<(String, String)> {
    vec![
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]
}

/// A temporary repository on branch `main` with one commit containing `README.md`.
pub(crate) struct TestRepo {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) root: PathBuf,
}

impl TestRepo {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = Self { dir, root };
        repo.git(&["init", "--quiet", "-b", "main"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.write("README.md", "# Test\n");
        repo.git(&["add", "README.md"]);
        repo.git(&["commit", "--quiet", "-m", "init"]);
        repo
    }

    pub(crate) fn git(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .envs(git_test_env())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    pub(crate) fn write(&self, rel: &str, contents: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
}

/// A build task with no paths, owned by `worker` when given.
pub(crate) fn task_spec(id: &str, depends_on: &[&str], worker: Option<&str>) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(id).unwrap(),
        title: format!("Build {id}"),
        task: format!("Do the work of {id}."),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: depends_on
            .iter()
            .map(|dep| TaskId::new(*dep).unwrap())
            .collect(),
        criteria: Vec::new(),
        worker: worker.map(|model| model.parse().unwrap()),
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
    }
}

/// An event of run `run-01`, as the sink would record it.
pub(crate) fn run_event(
    seq: u64,
    task: Option<&str>,
    session: Option<&str>,
    kind: RunEventKind,
) -> RunEvent {
    RunEvent {
        seq,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01").unwrap(),
        task_id: task.map(|task| TaskId::new(task).unwrap()),
        session: session.and_then(SessionKey::parse),
        source: "argonauts-code".to_owned(),
        kind,
    }
}
