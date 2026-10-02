//! Test helpers: a temporary git repository isolated from your git config, and cards and events
//! to fold.

use std::path::PathBuf;

use argonauts_core::events::card::{CardBudget, CardKind, CardSpec};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::SessionKey;
use argonauts_core::{CardId, RunId};

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

/// A build card with no paths, owned by `owner` when given.
pub(crate) fn card_spec(id: &str, depends_on: &[&str], owner: Option<&str>) -> CardSpec {
    CardSpec {
        id: CardId::new(id).unwrap(),
        title: format!("Build {id}"),
        task: format!("Do the work of {id}."),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: depends_on
            .iter()
            .map(|dep| CardId::new(*dep).unwrap())
            .collect(),
        criteria: Vec::new(),
        owner: owner.map(|worker| worker.parse().unwrap()),
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
    }
}

/// An event of run `run-01`, as the sink would record it.
pub(crate) fn run_event(
    seq: u64,
    card: Option<&str>,
    session: Option<&str>,
    kind: RunEventKind,
) -> RunEvent {
    RunEvent {
        seq,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01").unwrap(),
        card_id: card.map(|card| CardId::new(card).unwrap()),
        session: session.and_then(SessionKey::parse),
        source: "conductor".to_owned(),
        kind,
    }
}
