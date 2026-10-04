//! `argonauts sync`, skill drift, and the delegation package.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use argonauts_learning::Store;
use argonauts_learning::package;
use argonauts_learning::recipes::{LoadedRecipe, Match, Recipe};
use argonauts_learning::skills::{export, render, validate};
use argonauts_learning::sync::{self, SyncMode};

fn new_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(repo.join(".argonauts")).unwrap();
    let store = Store::new(dir.path().join("state"), repo);
    (dir, store)
}

fn conventions(store: &Store, text: &str) {
    fs::write(store.conventions(), text).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

const CONVENTIONS: &str = "# Conventions\n\nUse tabs in Go files.\n\n## Claude Code only\n\n\
    Use the Explore subagent before editing.\n\n### Hooks\n\nNone yet.\n\n## Testing\n\n\
    Run the store tests with redis up.\n";

#[test]
fn sync_renders_agents_and_claude_import() {
    let (_dir, store) = new_store();
    conventions(&store, CONVENTIONS);
    let agents_path = store.repo_root.join("AGENTS.md");
    let claude_path = store.repo_root.join("CLAUDE.md");

    let report = sync::sync(&store, SyncMode::Write).unwrap();
    assert_eq!(report.written, [agents_path.clone(), claude_path.clone()]);
    assert_eq!(report.warnings, Vec::<String>::new());

    let agents = read(&agents_path);
    assert!(agents.starts_with("<!-- argonauts sync:"), "{agents}");
    assert!(agents.contains("Use tabs in Go files."));
    assert!(agents.contains("## Testing\n\nRun the store tests with redis up.\n"));
    assert!(!agents.contains("Explore subagent"), "{agents}");
    assert!(!agents.contains("Hooks"), "{agents}");

    let claude = read(&claude_path);
    assert_eq!(claude.lines().next(), Some("@AGENTS.md"));
    assert!(
        claude.contains("## Claude Code only\n\nUse the Explore subagent"),
        "{claude}"
    );
    assert!(claude.contains("### Hooks"), "{claude}");
    assert!(!claude.contains("Use tabs"), "{claude}");

    let (rendered_agents, rendered_claude) = sync::render(CONVENTIONS);
    assert_eq!((rendered_agents, rendered_claude), (agents, claude));

    let again = sync::sync(&store, SyncMode::Write).unwrap();
    assert_eq!(again.written, Vec::<PathBuf>::new());
    assert_eq!(again.unchanged, [agents_path, claude_path]);

    let (_, plain) = sync::render("# Conventions\n\nShort.\n");
    assert_eq!(plain.lines().next(), Some("@AGENTS.md"));
    assert_eq!(plain.lines().count(), 2, "{plain}");
}

#[test]
fn sync_never_overwrites_hand_edits() {
    let (_dir, store) = new_store();
    let agents_path = store.repo_root.join("AGENTS.md");
    let claude_path = store.repo_root.join("CLAUDE.md");

    conventions(&store, CONVENTIONS);
    let checked = sync::sync(&store, SyncMode::Check).unwrap();
    assert_eq!(checked.written, [agents_path.clone(), claude_path.clone()]);
    assert!(!agents_path.exists() && !claude_path.exists());

    sync::sync(&store, SyncMode::Write).unwrap();
    let edited = read(&agents_path).replace("Use tabs", "Use spaces");
    fs::write(&agents_path, &edited).unwrap();
    conventions(
        &store,
        &CONVENTIONS
            .replace("redis up", "redis and postgres up")
            .replace("Explore subagent", "Plan subagent"),
    );

    let report = sync::sync(&store, SyncMode::Write).unwrap();
    assert_eq!(report.edited, std::slice::from_ref(&agents_path));
    assert_eq!(report.written, std::slice::from_ref(&claude_path));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("AGENTS.md was edited by hand"))
    );
    assert_eq!(read(&agents_path), edited);

    let checked = sync::sync(&store, SyncMode::Check).unwrap();
    assert_eq!(checked.edited, std::slice::from_ref(&agents_path));
    assert_eq!(read(&agents_path), edited);

    let forced = sync::sync(&store, SyncMode::Force).unwrap();
    assert_eq!(forced.written, std::slice::from_ref(&agents_path));
    assert!(read(&agents_path).contains("redis and postgres up"));

    // A CLAUDE.md you wrote before sync ever ran is yours too.
    let (_dir, store) = new_store();
    conventions(&store, CONVENTIONS);
    let yours = "# My notes\n\nKeep these.\n";
    fs::write(store.repo_root.join("CLAUDE.md"), yours).unwrap();
    let report = sync::sync(&store, SyncMode::Write).unwrap();
    assert_eq!(report.edited, [store.repo_root.join("CLAUDE.md")]);
    assert_eq!(read(&store.repo_root.join("CLAUDE.md")), yours);
    // Without the import Claude Code never reads AGENTS.md here, so you're told how to fix it.
    let missing = "CLAUDE.md doesn't import AGENTS.md, so Claude Code won't read the \
                   conventions. Add @AGENTS.md as its first line.";
    assert!(
        report.warnings.iter().any(|w| w == missing),
        "{:?}",
        report.warnings
    );
    assert_eq!(
        report.readers.as_deref(),
        Some("Codex and Antigravity CLI read AGENTS.md.")
    );
}

/// Each CLI reads the conventions once: Claude Code through CLAUDE.md's import, the others
/// from AGENTS.md itself.
#[test]
fn sync_says_who_reads_what() {
    let (_dir, store) = new_store();
    conventions(&store, CONVENTIONS);
    let report = sync::sync(&store, SyncMode::Check).unwrap();
    assert_eq!(
        report.readers.as_deref(),
        Some(
            "Claude Code reads CLAUDE.md, which imports AGENTS.md. Codex and Antigravity CLI \
             read AGENTS.md."
        )
    );
    // A CLAUDE.md you wrote that imports AGENTS.md is fine as it is.
    fs::write(store.repo_root.join("CLAUDE.md"), "# Notes\n\n@AGENTS.md\n").unwrap();
    let report = sync::sync(&store, SyncMode::Write).unwrap();
    assert!(
        report
            .readers
            .unwrap()
            .starts_with("Claude Code reads CLAUDE.md")
    );
    assert!(!report.warnings.iter().any(|w| w.contains("doesn't import")));
}

#[test]
fn sync_warns_near_harness_caps() {
    let warnings = |bytes: usize| {
        let (_dir, store) = new_store();
        conventions(&store, &format!("# C\n\n{}\n", "x".repeat(bytes)));
        sync::sync(&store, SyncMode::Check).unwrap().warnings
    };
    assert_eq!(warnings(10_000), Vec::<String>::new());

    let near_antigravity = warnings(22_000);
    assert_eq!(near_antigravity.len(), 1, "{near_antigravity:?}");
    assert!(near_antigravity[0].contains("near Antigravity CLI's 24,000 bytes"));

    let near_codex = warnings(30_500);
    assert_eq!(near_codex.len(), 2, "{near_codex:?}");
    assert!(near_codex[0].contains("near Codex's 32 KiB"));
    assert!(near_codex[1].contains("over Antigravity CLI's 24,000 bytes"));

    let over = warnings(40_000);
    assert!(over.iter().all(|w| w.contains("over")), "{over:?}");
    assert_eq!(over.len(), 2);
}

fn recipe() -> LoadedRecipe {
    LoadedRecipe {
        recipe: Recipe {
            recipe: "api-feature".to_owned(),
            version: 2,
            parent: Some(1),
            description: "Endpoints with tests".to_owned(),
            preset: None,
            mode: None,
            workflow: None,
            matches: Match::default(),
            prompts: BTreeMap::new(),
            settings: BTreeMap::new(),
        },
        path: PathBuf::from(".argonauts/recipes/api-feature/v2.yaml"),
        text: String::new(),
        sha256: "0".repeat(64),
    }
}

#[test]
fn sync_warns_on_skill_drift() {
    let (_dir, store) = new_store();
    let skill_path = store.skills_dir().join("api-feature/SKILL.md");
    fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
    fs::write(&skill_path, render(&export(&recipe()).unwrap())).unwrap();

    let clean = sync::sync(&store, SyncMode::Write).unwrap();
    assert_eq!(clean.skill_drift, Vec::new());

    let edited = read(&skill_path).replace("Build in small steps", "Build in one go");
    fs::write(&skill_path, &edited).unwrap();
    let report = sync::sync(&store, SyncMode::Write).unwrap();
    assert_eq!(report.skill_drift.len(), 1);
    let drift = &report.skill_drift[0];
    assert_eq!(drift.name, "api-feature");
    assert_eq!(drift.path, skill_path);
    assert_ne!(drift.expected, drift.found);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("api-feature skill was edited")),
        "{:?}",
        report.warnings
    );
    assert_eq!(read(&skill_path), edited);
}

#[test]
fn delegation_package_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let out = dir.path().join("package");
    let binary = dir.path().join("bin/argonauts");

    let written = package::write(&out, &repo, &binary).unwrap();
    let mut relative: Vec<String> = written
        .iter()
        .map(|path| {
            path.strip_prefix(&out)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    relative.sort();
    assert_eq!(
        relative,
        [
            ".claude-plugin/plugin.json",
            ".mcp.json",
            "mcp.json",
            "plugin.json",
            "skills/delegate-to-argonauts/SKILL.md",
        ]
    );

    let json = |path: &str| -> Value { serde_json::from_str(&read(&out.join(path))).unwrap() };
    let manifest = json("plugin.json");
    assert!(
        manifest["$schema"]
            .as_str()
            .unwrap()
            .starts_with("https://")
    );
    assert_eq!(manifest["name"], "argonauts");
    assert_eq!(json(".claude-plugin/plugin.json")["name"], "argonauts");

    for path in ["mcp.json", ".mcp.json"] {
        let servers = json(path);
        let entry = &servers["mcpServers"]["argonauts"];
        let command = entry["command"].as_str().unwrap();
        assert!(Path::new(command).is_absolute());
        assert_eq!(command, binary.to_str().unwrap());
        let args: Vec<&str> = entry["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();
        assert_eq!(args, ["mcp", "serve", "--repo", repo.to_str().unwrap()]);
        assert!(entry.get("env").is_none(), "no credentials: {entry}");
        assert!(entry.get("headers").is_none(), "no credentials: {entry}");
    }

    let skill = read(&out.join("skills/delegate-to-argonauts/SKILL.md"));
    assert_eq!(
        validate(&skill, "delegate-to-argonauts"),
        Vec::<String>::new()
    );
    for expected in ["argonauts_request_decision", "Never approve", "link"] {
        assert!(skill.contains(expected), "{expected}");
    }

    let not_a_repo = package::write(&out, &dir.path().join("missing"), &binary);
    assert!(not_a_repo.is_err());
}
