use std::path::Path;
use std::sync::Arc;

use argonauts_core::RunId;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::task::TaskSpec;
use argonauts_core::names::ApprovalId;
use argonauts_core::policy::{
    DecisionPoint, EvidenceFacts, EvidencePacket, EvidenceSubject, HardLimit, HardLimits, Violation,
};
use argonauts_core::session_runner::{Access, RunnerDirectory, SandboxMode};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_testkit::runner::session_spec;
use argonauts_testkit::{FakeDirectory, FakeRunner};
use serde_json::json;

use super::BuiltinLimits;
use super::diff::{self, FileStatus, LineKind};
use super::paths::sample_path;
use super::tamper::{has_skip_marker, is_test_path};
use crate::SettingsReloader;
use crate::approvals::settings::SettingsSource;

const CLEAN: &str = include_str!("../../tests/fixtures/diffs/clean.diff");
const DELETED_TEST: &str = include_str!("../../tests/fixtures/diffs/deleted_test.diff");
const ADDED_ONLY: &str = include_str!("../../tests/fixtures/diffs/added_only.diff");
const LOCKFILE_EDIT: &str = include_str!("../../tests/fixtures/diffs/lockfile_edit.diff");
const NEW_DEPENDENCY: &str = include_str!("../../tests/fixtures/diffs/new_dependency.diff");
const PROTECTED_PATH: &str = include_str!("../../tests/fixtures/diffs/protected_path.diff");
const FORBIDDEN_PATH: &str = include_str!("../../tests/fixtures/diffs/forbidden_path.diff");
const INSTRUCTION_FILES: &str = include_str!("../../tests/fixtures/diffs/instruction_files.diff");
const BINARY_FILES: &str = include_str!("../../tests/fixtures/diffs/binary_files.diff");

/// Settings with the OS sandbox on, so only the limit a test is about shows up. The default
/// is off for now, and off makes every approval ask.
fn sandboxed() -> Settings {
    let mut settings = Settings::default();
    settings.sandbox.mode = SandboxChoice::Os;
    settings
}

fn limits_with(settings: Settings, workers: Arc<dyn RunnerDirectory>) -> BuiltinLimits {
    let reload: SettingsReloader = Arc::new(|| None);
    BuiltinLimits::new(SettingsSource::new(Arc::new(settings), reload), workers)
}

fn limits(settings: Settings) -> BuiltinLimits {
    limits_with(settings, Arc::new(FakeDirectory::new()))
}

fn task(forbidden: &[&str]) -> TaskSpec {
    serde_json::from_value(json!({
        "id": "task-001", "title": "Parser", "task": "Trim fields", "kind": {"kind": "build"},
        "owned_paths": ["src/**"], "read_only_paths": [], "forbidden_paths": forbidden,
        "hidden_paths": [], "depends_on": [], "criteria": ["G1"], "worker": "claude-code/haiku",
        "reviewers": [],
        "limit": {"limit": null, "max_minutes": null, "max_tool_calls": null,
                   "inactivity_min": null},
        "predictions": null,
    }))
    .unwrap()
}

fn point(kind: ApprovalKind) -> DecisionPoint {
    DecisionPoint {
        run: RunId::new("run-01").unwrap(),
        approval: ApprovalId::numbered(1),
        kind,
        learned_workflow: None,
        authors: Vec::new(),
        mode_override: None,
        main_agent: None,
    }
}

fn evidence(kind: ApprovalKind, facts: EvidenceFacts) -> EvidencePacket {
    EvidencePacket {
        kind,
        goal: None,
        facts,
        agent_outputs: Vec::new(),
        documents: Vec::new(),
    }
}

fn subject() -> Option<EvidenceSubject> {
    Some(EvidenceSubject {
        base_commit: "a".repeat(40),
        head_commit: "b".repeat(40),
        tree_sha: "c".repeat(40),
    })
}

/// A merge that passes every limit.
fn good_merge() -> EvidenceFacts {
    EvidenceFacts {
        subject: subject(),
        checks_passed: Some(true),
        acceptance_tests_valid: Some(true),
        tamper_flags: Some(0),
        agent_paths: vec!["src/parser.rs".to_owned(), "tests/parser.rs".to_owned()],
        ..EvidenceFacts::default()
    }
}

fn kinds(violations: &[Violation]) -> Vec<HardLimit> {
    let mut kinds: Vec<HardLimit> = violations.iter().map(|v| v.limit).collect();
    kinds.sort_by_key(|kind| format!("{kind:?}"));
    kinds.dedup();
    kinds
}

fn only(violations: &[Violation], limit: HardLimit) -> Vec<&Violation> {
    violations.iter().filter(|v| v.limit == limit).collect()
}

fn check_point(settings: Settings, kind: ApprovalKind, facts: EvidenceFacts) -> Vec<Violation> {
    limits(settings).check_point(&point(kind), &evidence(kind, facts))
}

fn fake(prefix: &str, body: char, len: usize) -> String {
    format!("{prefix}{}", body.to_string().repeat(len))
}

/// A one-file diff that adds `added` to `path`, built at runtime.
fn adding(path: &str, added: &[&str]) -> String {
    let mut out = format!(
        "diff --git a/{path} b/{path}\nindex 1111111..2222222 100644\n--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,{} @@\n context\n",
        added.len() + 1
    );
    for line in added {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[test]
fn builtin_checks_detect_secrets_tamper_deps_paths() {
    let mut settings = sandboxed();
    settings.limits.dependency_allowlist = vec!["left-pad".to_owned(), "serde".to_owned()];
    let limits = limits(settings);
    let task = task(&["src/api/**"]);

    assert_eq!(limits.check_task_diff(&task, CLEAN), Vec::new());
    assert_eq!(limits.check_task_diff(&task, ""), Vec::new());

    let deleted = limits.check_task_diff(&task, DELETED_TEST);
    let removed = only(&deleted, HardLimit::TestDeleted);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].paths, vec!["tests/login.test.ts"]);

    let focused = limits.check_task_diff(&task, ADDED_ONLY);
    assert_eq!(kinds(&focused), vec![HardLimit::SkipMarker]);
    assert_eq!(focused[0].paths, vec!["src/cart.test.js"]);

    let lock = limits.check_task_diff(&task, LOCKFILE_EDIT);
    assert_eq!(
        kinds(&lock),
        vec![HardLimit::Lockfile, HardLimit::ProtectedPath]
    );
    assert!(lock.iter().all(|v| v.paths == vec!["Cargo.lock"]));

    let deps = limits.check_task_diff(&task, NEW_DEPENDENCY);
    assert_eq!(kinds(&deps), vec![HardLimit::Dependency]);
    assert_eq!(deps[0].paths, vec!["package.json", "Cargo.toml"]);
    assert!(
        deps[0].detail.contains("evil-pkg, sketchy-crate"),
        "{}",
        deps[0].detail
    );
    assert!(!deps[0].detail.contains("left-pad") && !deps[0].detail.contains("express"));

    let protected = limits.check_task_diff(&task, PROTECTED_PATH);
    assert_eq!(kinds(&protected), vec![HardLimit::ProtectedPath]);
    assert_eq!(
        protected[0].paths,
        vec![".argonauts/goal.md", ".github/workflows/ci.yml"]
    );

    let forbidden = limits.check_task_diff(&task, FORBIDDEN_PATH);
    assert_eq!(kinds(&forbidden), vec![HardLimit::ProtectedPath]);
    assert_eq!(forbidden[0].paths, vec!["src/api/frozen.rs"]);
    assert!(forbidden[0].detail.contains("task-001"));
    assert_eq!(
        limits.check_task_diff(&self::task(&[]), FORBIDDEN_PATH),
        Vec::new()
    );

    // Secrets are built here, never written into a fixture.
    let token = fake("ghp_", 'a', 36);
    let assigned = format!("password = \"{}\"", fake("Zq", '7', 20));
    let key = ["-----BEGIN RSA ", "PRIVATE KEY-----"].concat();
    for (line, kind) in [
        (format!("const KEY: &str = \"{token}\";"), "github token"),
        (assigned, "assigned secret"),
        (key, "private key"),
    ] {
        let found = limits.check_task_diff(&task, &adding("src/config.rs", &[&line]));
        assert_eq!(kinds(&found), vec![HardLimit::Secret], "{kind}");
        assert_eq!(found[0].paths, vec!["src/config.rs"]);
        assert!(found[0].detail.contains(kind), "{}", found[0].detail);
        assert!(
            !found[0].detail.contains(&line),
            "the detail must not repeat the secret"
        );
    }
    // A removed secret is a fix, not a leak.
    let removal = format!(
        "diff --git a/.env b/.env\n--- a/.env\n+++ b/.env\n@@ -1 +0,0 @@\n-TOKEN={token}\n"
    );
    assert_eq!(limits.check_task_diff(&task, &removal), Vec::new());
}

#[test]
fn binary_files_the_scan_cannot_read_stop_the_task() {
    let found = limits(sandboxed()).check_task_diff(&task(&[]), BINARY_FILES);
    assert_eq!(kinds(&found), vec![HardLimit::Secret]);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].paths, vec!["keys/client.p12"]);
    assert!(found[0].detail.contains("key file"), "{}", found[0].detail);
    // Media files and deletions pass, and a key store is reported once, by name.
    assert_eq!(
        found[1].paths,
        vec!["certs/server.der", "data/app.sqlite", "dist/config.zip"]
    );
    assert!(
        found[1].detail.contains("can't read"),
        "{}",
        found[1].detail
    );
}

#[test]
fn builtin_checks_fail_closed_on_a_diff_they_cannot_read() {
    let limits = limits(sandboxed());
    let task = task(&[]);
    let broken = [
        // More lines than the header says.
        "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n+c\n",
        // Fewer.
        "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a\n+b\n",
        "diff --git a/x b/../.argonauts/x\n--- a/x\n+++ b/../.argonauts/x\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --cc src/lib.rs\nindex 1,2..3\n",
        "+an added line with no file\n",
        "some prose\n",
    ];
    for diff in broken {
        let violations = limits.check_task_diff(&task, diff);
        assert_eq!(kinds(&violations), vec![HardLimit::ChecksFailed], "{diff}");
        assert!(!violations[0].detail.contains("prose"));
    }
}

#[test]
fn diffs_parse_renames_binaries_quotes_and_plain_diffs() {
    let renamed = "diff --git a/tests/a_test.go b/pkg/a.go\nsimilarity index 100%\nrename from tests/a_test.go\nrename to pkg/a.go\n";
    let changes = diff::parse(renamed).unwrap();
    assert_eq!(changes[0].status, FileStatus::Renamed);
    assert_eq!(changes[0].path, "pkg/a.go");
    assert_eq!(changes[0].old_path.as_deref(), Some("tests/a_test.go"));
    let violations = limits(sandboxed()).check_task_diff(&task(&[]), renamed);
    assert_eq!(kinds(&violations), vec![HardLimit::TestDeleted]);

    let binary = "diff --git a/img/logo.png b/img/logo.png\nnew file mode 100644\nindex 0000000..1234567\nBinary files /dev/null and b/img/logo.png differ\ndiff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
    let changes = diff::parse(binary).unwrap();
    assert_eq!(changes.len(), 2);
    assert!(changes[0].binary);
    assert_eq!(changes[0].status, FileStatus::Added);
    assert_eq!(changes[1].path, "run.sh");
    assert_eq!(changes[1].status, FileStatus::Modified);

    let quoted = "diff --git \"a/docs/my notes.md\" \"b/docs/my notes.md\"\n--- \"a/docs/my notes.md\"\n+++ \"b/docs/my notes.md\"\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n";
    assert_eq!(diff::parse(quoted).unwrap()[0].path, "docs/my notes.md");
    let octal = "diff --git \"a/caf\\303\\251.md\" \"b/caf\\303\\251.md\"\nnew file mode 100644\n";
    assert_eq!(diff::parse(octal).unwrap()[0].path, "café.md");
    let spaced = "diff --git a/my notes.md b/my notes.md\nnew file mode 100644\n";
    assert_eq!(diff::parse(spaced).unwrap()[0].path, "my notes.md");

    // Lines that look like headers inside a hunk are content, by the hunk's count.
    let plain = "--- a/q.sql\t2026-01-01 00:00:00\n+++ b/q.sql\t2026-01-02 00:00:00\n@@ -1,2 +1,2 @@\n--- old comment\n+++ new comment\n select 1;\n";
    let changes = diff::parse(plain).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "q.sql");
    assert_eq!(
        changes[0].removed().collect::<Vec<_>>(),
        vec!["-- old comment"]
    );
    assert_eq!(
        changes[0].added().collect::<Vec<_>>(),
        vec!["++ new comment"]
    );
    let kinds: Vec<LineKind> = changes[0].hunks[0].iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        vec![LineKind::Removed, LineKind::Added, LineKind::Context]
    );

    let two_plain = format!("{plain}{}", plain.replace("q.sql", "r.sql"));
    let paths: Vec<String> = diff::parse(&two_plain)
        .unwrap()
        .into_iter()
        .map(|c| c.path)
        .collect();
    assert_eq!(paths, vec!["q.sql", "r.sql"]);
}

#[test]
fn protected_globs_read_like_gitignore_and_ignore_case() {
    let mut settings = sandboxed();
    settings
        .limits
        .protected_paths
        .extend(["secrets.env".to_owned(), "/infra".to_owned()]);
    let limits = limits(settings);
    let touched = |path: &str| {
        let diff = adding(path, &["x"]);
        !only(
            &limits.check_task_diff(&task(&[]), &diff),
            HardLimit::ProtectedPath,
        )
        .is_empty()
    };
    assert!(touched(".Argonauts/config.toml"));
    assert!(touched(".agents/skills/x/SKILL.md"));
    assert!(touched("deploy/secrets.env"));
    assert!(touched("infra/main.tf"));
    assert!(!touched("src/infra/main.tf"));
    assert!(touched("web/package-lock.json"));
    assert!(!touched("src/lib.rs"));

    let mut bad = sandboxed();
    bad.limits.protected_paths = vec!["src/[".to_owned()];
    let found = self::limits(bad).check_task_diff(&task(&[]), CLEAN);
    assert_eq!(kinds(&found), vec![HardLimit::ProtectedPath]);
    assert!(found[0].detail.contains("src/["));

    assert_eq!(sample_path(".github/**"), ".github/x");
    assert_eq!(sample_path("**/Cargo.lock"), "Cargo.lock");
    assert_eq!(sample_path("src/{a,b}/[!x]*.rs"), "src/a/xx.rs");
}

#[test]
fn dependency_entries_are_read_from_every_manifest() {
    let mut settings = sandboxed();
    settings.limits.dependency_allowlist = vec![
        "Requests".to_owned(),
        "golang.org/x/sync".to_owned(),
        "serde_json".to_owned(),
    ];
    let limits = limits(settings);
    let names = |diff: &str| -> Vec<String> {
        let found = limits.check_task_diff(&task(&[]), diff);
        assert!(
            only(&found, HardLimit::ChecksFailed).is_empty(),
            "{found:?}"
        );
        match only(&found, HardLimit::Dependency).first() {
            Some(v) => v
                .detail
                .rsplit(": ")
                .next()
                .unwrap()
                .split(", ")
                .map(str::to_owned)
                .collect(),
            None => Vec::new(),
        }
    };

    let requirements = adding(
        "requirements-dev.txt",
        &[
            "requests==2.32.0",
            "# a comment",
            "pytest-xdist[psutil]>=3",
            "--index-url https://mirror.example",
        ],
    );
    assert_eq!(
        names(&requirements),
        vec!["--index-url https://mirror.example", "pytest-xdist"]
    );
    let bump = "diff --git a/requirements.txt b/requirements.txt\n--- a/requirements.txt\n+++ b/requirements.txt\n@@ -1 +1 @@\n-flask==2.0\n+Flask==3.0\n";
    assert_eq!(names(bump), Vec::<String>::new());

    let go = "diff --git a/go.mod b/go.mod\n--- a/go.mod\n+++ b/go.mod\n@@ -3,5 +3,8 @@\n go 1.22\n \n require (\n+\tgolang.org/x/sync v0.8.0\n+\tgithub.com/evil/mod v1.0.0 // indirect\n \tgithub.com/pkg/errors v0.9.1\n )\n+replace github.com/pkg/errors => github.com/fork/errors v0.9.2\n";
    assert_eq!(
        names(go),
        vec!["github.com/evil/mod", "github.com/fork/errors"]
    );
    // A hunk that starts inside `require (` still counts.
    let go_unplaced = "diff --git a/go.mod b/go.mod\n--- a/go.mod\n+++ b/go.mod\n@@ -9,1 +9,2 @@\n \tgithub.com/pkg/errors v0.9.1\n+\tgithub.com/other/mod v1.2.0\n";
    assert_eq!(names(go_unplaced), vec!["github.com/other/mod"]);

    let pyproject = "diff --git a/pyproject.toml b/pyproject.toml\n--- a/pyproject.toml\n+++ b/pyproject.toml\n@@ -1,6 +1,12 @@\n [project]\n name = \"demo\"\n version = \"0.1.0\"\n dependencies = [\n+    \"requests>=2\",\n+    \"httpx==0.27\",\n     \"click\",\n ]\n+keywords = [\"cli\"]\n+\n+[tool.poetry.dependencies]\n+python = \"^3.11\"\n";
    assert_eq!(names(pyproject), vec!["httpx"]);
    let optional = "diff --git a/pyproject.toml b/pyproject.toml\n--- a/pyproject.toml\n+++ b/pyproject.toml\n@@ -1,1 +1,3 @@\n [project.optional-dependencies]\n+test = [\"pytest>=8\"]\n+docs = [\n";
    assert_eq!(names(optional), vec!["pytest"]);
    // A hunk inside an array whose key it doesn't show counts versioned requirements.
    let unplaced = "diff --git a/pyproject.toml b/pyproject.toml\n--- a/pyproject.toml\n+++ b/pyproject.toml\n@@ -8,1 +8,3 @@\n     \"click\",\n+    \"rich>=13\",\n+    \"tests\",\n";
    assert_eq!(names(unplaced), vec!["rich"]);

    let cargo = "diff --git a/crates/x/Cargo.toml b/crates/x/Cargo.toml\n--- a/crates/x/Cargo.toml\n+++ b/crates/x/Cargo.toml\n@@ -1,3 +1,9 @@\n [package]\n name = \"x\"\n version = \"0.1.0\"\n+\n+[target.'cfg(any(unix, windows))'.dependencies]\n+serde-json = \"1\"\n+libc.workspace = true\n+\n+[dependencies.rand]\n";
    assert_eq!(names(cargo), vec!["libc", "rand"]);
    let cargo_unplaced = "diff --git a/Cargo.toml b/Cargo.toml\n--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -9,1 +9,4 @@\n anyhow = \"1\"\n+tokio = { version = \"1\", features = [\"full\"] }\n+edition = \"2024\"\n+regex.workspace = true\n";
    assert_eq!(names(cargo_unplaced), vec!["regex", "tokio"]);

    let npm_unplaced = "diff --git a/web/package.json b/web/package.json\n--- a/web/package.json\n+++ b/web/package.json\n@@ -12,1 +12,5 @@\n     \"react\": \"^18.2.0\",\n+    \"left-pad\": \"^1.3.0\",\n+    \"lint\": \"eslint .\",\n+    \"version\": \"2.0.0\",\n+    \"@scope/tool\": \"github:someone/tool\",\n";
    assert_eq!(names(npm_unplaced), vec!["@scope/tool", "left-pad"]);
    let overrides = "diff --git a/package.json b/package.json\n--- a/package.json\n+++ b/package.json\n@@ -1,2 +1,5 @@\n {\n   \"name\": \"demo\",\n+  \"overrides\": {\n+    \"minimist\": \"1.2.8\"\n+  }\n";
    assert_eq!(names(overrides), vec!["minimist"]);
}

#[test]
fn skip_markers_are_words_not_substrings() {
    assert!(has_skip_marker("  it.skip(\"slow\", () => {})", false));
    assert!(has_skip_marker(
        "test.describe.only(\"x\", () => {})",
        false
    ));
    assert!(has_skip_marker("    xit(\"later\", () => {})", false));
    assert!(has_skip_marker("#[ignore = \"flaky\"]", false));
    assert!(has_skip_marker(
        "@pytest.mark.skipif(sys.platform == \"win32\")",
        false
    ));
    assert!(has_skip_marker("\tt.Skip(\"needs network\")", false));
    assert!(has_skip_marker("rows.skip(1)", true));
    assert!(has_skip_marker("fit(\"focused\", () => {})", true));

    assert!(!has_skip_marker("let rest = args.iter().skip(1);", false));
    assert!(!has_skip_marker("process.exit(1);", false));
    assert!(!has_skip_marker("model.fit(x, y)", true));
    assert!(!has_skip_marker("#[ignored_field]", false));
    assert!(!has_skip_marker("@IgnoreExtraProperties", false));
    assert!(!has_skip_marker("list.st.Skip(1)", false));

    for path in [
        "tests/api.rs",
        "src/cart.test.js",
        "pkg/store_test.go",
        "test_login.py",
        "src/test/java/ParserTest.java",
        "src/__snapshots__/view.snap",
        "src/limits/tests.rs",
    ] {
        assert!(is_test_path(path), "{path}");
    }
    for path in ["src/latest.rs", "src/contest.py", "docs/testimony.md"] {
        assert!(!is_test_path(path), "{path}");
    }
}

#[test]
fn test_config_and_weakened_tests_are_caught() {
    let limits = limits(sandboxed());
    let check = |diff: &str| limits.check_task_diff(&task(&[]), diff);

    for diff in [
        adding("jest.config.ts", &["  bail: true,"]),
        adding("web/vitest.config.mjs", &["  passWithNoTests: true,"]),
        adding("pytest.ini", &["addopts = -k 'not slow'"]),
        adding(".cargo/config.toml", &["rustflags = []"]),
        "diff --git a/Cargo.toml b/Cargo.toml\n--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -1,1 +1,3 @@\n [workspace]\n+[profile.test]\n+opt-level = 3\n".to_owned(),
        "diff --git a/Cargo.toml b/Cargo.toml\n--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -1,1 +1,2 @@\n [lib]\n+test = false\n".to_owned(),
        "diff --git a/Cargo.toml b/Cargo.toml\n--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -20,1 +20,2 @@\n name = \"api\"\n+harness = false\n".to_owned(),
        "diff --git a/pyproject.toml b/pyproject.toml\n--- a/pyproject.toml\n+++ b/pyproject.toml\n@@ -1,2 +1,2 @@\n [tool.pytest.ini_options]\n-addopts = \"-q\"\n+addopts = \"-q -k 'not slow'\"\n".to_owned(),
        "diff --git a/package.json b/package.json\n--- a/package.json\n+++ b/package.json\n@@ -3,3 +3,3 @@\n   \"scripts\": {\n-    \"test\": \"vitest run\",\n+    \"test\": \"exit 0\",\n     \"build\": \"tsc\"\n".to_owned(),
    ] {
        let found = check(&diff);
        assert_eq!(kinds(&found), vec![HardLimit::TestConfigChanged], "{diff}");
    }
    let unrelated = "diff --git a/Cargo.toml b/Cargo.toml\n--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -1,2 +1,2 @@\n [package]\n-description = \"old\"\n+description = \"new\"\n";
    assert_eq!(check(unrelated), Vec::new());

    // A dropped assertion is suspected weakening: flagged, not blocked.
    let weaker = "diff --git a/tests/api.rs b/tests/api.rs\n--- a/tests/api.rs\n+++ b/tests/api.rs\n@@ -1,4 +1,3 @@\n #[test]\n fn lists() {\n-    assert_eq!(list().len(), 2);\n     list();\n";
    let found = check(weaker);
    assert_eq!(kinds(&found), vec![HardLimit::TamperFlag]);
    assert_eq!(found[0].paths, vec!["tests/api.rs"]);
    assert!(found[0].detail.contains("1 fewer assertions"));

    // A test moved between files keeps its count.
    let moved = "diff --git a/tests/a.rs b/tests/a.rs\n--- a/tests/a.rs\n+++ b/tests/a.rs\n@@ -1,3 +0,0 @@\n-#[test]\n-fn one() {\n-    assert!(true);\ndiff --git a/tests/b.rs b/tests/b.rs\n--- a/tests/b.rs\n+++ b/tests/b.rs\n@@ -1 +1,4 @@\n use x;\n+#[test]\n+fn one() {\n+    assert!(true);\n";
    assert_eq!(check(moved), Vec::new());
}

/// A plugin or hook an agent writes into your repo would run in your own Claude Code, outside
/// any sandbox, once merged.
#[test]
fn claude_code_config_in_a_diff_blocks_auto_merge() {
    let limits = limits(sandboxed());
    let paths = [
        ".claude/settings.json",
        ".claude/hooks/x.sh",
        "tools/p/.claude-plugin/plugin.json",
        ".mcp.json",
        // 2.1.289 loads these as instructions, and runs what a skill or a plugin holds.
        "CLAUDE.local.md",
        ".claude/rules/always.md",
        ".claude/agents/x.md",
        ".claude/commands/x.md",
        ".claude/skills/r/scripts/publish.sh",
        // The plugin's hooks file and the module it registers, beside its `.claude-plugin/`.
        "tools/p/hooks/hooks.json",
        "tools/p/register.js",
    ];
    let diff: String = paths
        .iter()
        .map(|path| {
            format!(
                "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n\
                 @@ -0,0 +1 @@\n+x\n"
            )
        })
        .collect();
    let found = limits.check_task_diff(&task(&[]), &diff);
    assert_eq!(kinds(&found), vec![HardLimit::InstructionFile]);
    let mut expected = paths.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(found[0].paths, expected);
    for path in paths {
        assert!(found[0].detail.contains(path), "{}", found[0].detail);
    }
    let unrelated = "diff --git a/src/claude/settings.json b/src/claude/settings.json\nnew file mode 100644\n--- /dev/null\n+++ b/src/claude/settings.json\n@@ -0,0 +1 @@\n+x\n";
    assert!(limits.check_task_diff(&task(&[]), unrelated).is_empty());
}

fn new_files(paths: &[&str]) -> String {
    paths
        .iter()
        .map(|path| {
            format!(
                "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n\
                 @@ -0,0 +1 @@\n+x\n"
            )
        })
        .collect()
}

/// A plugin already in the repository shows by its `.claude-plugin/` folder in the task's tree,
/// so a change to its hooks module or its MCP server code alone waits for you too. What Claude
/// Code keeps under `.claude/` for itself doesn't.
#[test]
fn a_change_inside_a_plugin_the_tree_holds_blocks_auto_merge() {
    let limits = limits(sandboxed());
    let tree = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tree.path().join("tools/q/.claude-plugin")).unwrap();
    let diff = new_files(&[
        "tools/q/hooks/register.js",
        "tools/q/server/index.js",
        "tools/other/index.js",
        ".claude/worktrees/w/notes.txt",
    ]);
    let found = limits.check_task_tree_diff(&task(&[]), &diff, tree.path());
    assert_eq!(kinds(&found), vec![HardLimit::InstructionFile]);
    assert_eq!(
        found[0].paths,
        vec!["tools/q/hooks/register.js", "tools/q/server/index.js"]
    );
    // Without the tree, nothing shows the plugin.
    assert!(limits.check_task_diff(&task(&[]), &diff).is_empty());

    // At the merge, the agents' own paths show the root.
    let mut facts = good_merge();
    facts.agent_paths.extend([
        "tools/p/.claude-plugin/plugin.json".to_owned(),
        "tools/p/hooks/register.js".to_owned(),
    ]);
    let found = limits.check_point(
        &point(ApprovalKind::Merge),
        &evidence(ApprovalKind::Merge, facts),
    );
    assert_eq!(kinds(&found), vec![HardLimit::InstructionFile]);
    assert_eq!(
        found[0].paths,
        vec![
            "tools/p/.claude-plugin/plugin.json",
            "tools/p/hooks/register.js"
        ]
    );
}

#[test]
fn instruction_files_in_diff_block_auto_merge() {
    let limits = limits(sandboxed());
    let found = limits.check_task_diff(&task(&[]), INSTRUCTION_FILES);
    // A flag for the merge, not a failure that stops the task.
    assert_eq!(kinds(&found), vec![HardLimit::InstructionFile]);
    assert_eq!(found[0].paths, vec!["CLAUDE.md", "skills/deploy/SKILL.md"]);
    assert!(found[0].detail.contains("skills/deploy/SKILL.md"));

    let deleted = "diff --git a/docs/agents.md b/docs/agents.md\ndeleted file mode 100644\n--- a/docs/agents.md\n+++ /dev/null\n@@ -1 +0,0 @@\n-Be careful.\n";
    let found = limits.check_task_diff(&task(&[]), deleted);
    assert_eq!(kinds(&found), vec![HardLimit::InstructionFile]);

    let mut facts = good_merge();
    facts.agent_paths.push("sub/GEMINI.md".to_owned());
    let found = limits.check_point(
        &point(ApprovalKind::Merge),
        &evidence(ApprovalKind::Merge, facts.clone()),
    );
    assert_eq!(kinds(&found), vec![HardLimit::InstructionFile]);
    assert_eq!(found[0].paths, vec!["sub/GEMINI.md"]);
    assert!(found[0].detail.contains("sub/GEMINI.md"));

    // Instruction files only Argonauts Code committed, and non-merge points, aren't flagged.
    facts.agent_paths.pop();
    facts.conductor_paths.push("CLAUDE.md".to_owned());
    assert_eq!(
        check_point(sandboxed(), ApprovalKind::Merge, facts),
        Vec::new()
    );
}

#[test]
fn a_commit_only_argonauts_code_made_is_not_a_violation() {
    let mut facts = good_merge();
    facts.conductor_paths = vec![
        ".argonauts/goal.md".to_owned(),
        ".argonauts/decisions.md".to_owned(),
        "Cargo.toml".to_owned(),
        "Cargo.lock".to_owned(),
        "AGENTS.md".to_owned(),
    ];
    assert_eq!(
        check_point(sandboxed(), ApprovalKind::Merge, facts.clone()),
        Vec::new()
    );
    // The same paths from an agent are violations.
    facts.agent_paths.extend(facts.conductor_paths.clone());
    let found = check_point(sandboxed(), ApprovalKind::Merge, facts);
    assert_eq!(
        kinds(&found),
        vec![
            HardLimit::InstructionFile,
            HardLimit::Lockfile,
            HardLimit::ProtectedPath
        ]
    );
    let protected = only(&found, HardLimit::ProtectedPath);
    assert_eq!(
        protected[0].paths,
        vec![
            ".argonauts/decisions.md",
            ".argonauts/goal.md",
            "Cargo.lock"
        ]
    );
}

#[test]
fn plan_or_merge_without_subject_fails() {
    for kind in [ApprovalKind::Plan, ApprovalKind::Merge] {
        let mut facts = good_merge();
        facts.subject = None;
        let found = check_point(sandboxed(), kind, facts);
        assert_eq!(kinds(&found), vec![HardLimit::NoSubject], "{kind:?}");
        let empty = only(&found, HardLimit::NoSubject);
        assert_eq!(empty.len(), 1, "{kind:?}");
        assert_eq!(empty[0].detail, "the evidence names no commits");
    }
    // Other points name no commits and don't need to.
    for kind in [ApprovalKind::Understand, ApprovalKind::Stuck] {
        assert_eq!(
            check_point(sandboxed(), kind, EvidenceFacts::default()),
            Vec::new(),
            "{kind:?}"
        );
    }
    // Evidence built for another point covers nothing here.
    let found = limits(sandboxed()).check_point(
        &point(ApprovalKind::Merge),
        &evidence(ApprovalKind::Understand, good_merge()),
    );
    assert_eq!(kinds(&found), vec![HardLimit::HeadMoved]);
}

#[test]
fn sandbox_off_makes_every_approval_ask() {
    let mut settings = Settings::default();
    settings.sandbox.mode = SandboxChoice::Tool;
    let every = [
        ApprovalKind::Understand,
        ApprovalKind::Plan,
        ApprovalKind::Merge,
        ApprovalKind::Fallback,
        ApprovalKind::AgentDown,
        ApprovalKind::Stuck,
        ApprovalKind::Limit,
        ApprovalKind::ChangeRequest,
        ApprovalKind::GoalChange,
        ApprovalKind::WorkflowPromotion,
        ApprovalKind::TakeOverReturn,
        ApprovalKind::Question,
        ApprovalKind::OriginalReturned,
    ];
    for kind in every {
        let found = check_point(settings.clone(), kind, good_merge());
        assert_eq!(kinds(&found), vec![HardLimit::NoSandbox], "{kind:?}");
    }

    // Its sessions start without a sandbox, and the violation says why.
    let directory = FakeDirectory::new();
    directory.add(FakeRunner::cli("claude-code/haiku", "anthropic"));
    let limits = limits_with(settings, Arc::new(directory));
    let mut spec = session_spec("claude-code/haiku", Access::ReadOnly, Path::new("work"));
    spec.sandbox.mode = SandboxMode::None;
    let found = limits.check_session(&spec);
    assert_eq!(kinds(&found), vec![HardLimit::NoSandbox]);
    assert!(found[0].detail.contains("sandbox.mode to tool"));
}

#[test]
fn sandbox_turned_on_mid_run_still_asks() {
    let mut off = Settings::default();
    off.sandbox.mode = SandboxChoice::Tool;
    let reload: SettingsReloader = Arc::new(|| Some(sandboxed()));
    let source = SettingsSource::new(Arc::new(off), reload);
    // New sessions get the sandbox the files now ask for.
    assert_eq!(source.current().sandbox.mode, SandboxChoice::Os);

    // The sessions that already ran had none, so every approval of the run still asks.
    let limits = BuiltinLimits::new(source, Arc::new(FakeDirectory::new()));
    for kind in [
        ApprovalKind::Understand,
        ApprovalKind::Plan,
        ApprovalKind::Merge,
    ] {
        let found = limits.check_point(&point(kind), &evidence(kind, good_merge()));
        assert_eq!(kinds(&found), vec![HardLimit::NoSandbox], "{kind:?}");
        assert!(found[0].detail.contains("started with sandbox.mode tool"));
    }

    // Turning it off mid-run waits for the next run.
    let mut off = Settings::default();
    off.sandbox.mode = SandboxChoice::Tool;
    let reload: SettingsReloader = Arc::new(move || Some(off.clone()));
    let source = SettingsSource::new(Arc::new(sandboxed()), reload);
    let limits = BuiltinLimits::new(source, Arc::new(FakeDirectory::new()));
    let merge = ApprovalKind::Merge;
    assert_eq!(
        limits.check_point(&point(merge), &evidence(merge, good_merge())),
        Vec::new()
    );
}

#[test]
fn check_session_needs_sandbox_and_goal_hash() {
    let directory = FakeDirectory::new();
    directory.add(FakeRunner::cli("claude-code/haiku", "anthropic"));
    let limits = limits_with(sandboxed(), Arc::new(directory));
    let spec = |access: Access| session_spec("claude-code/haiku", access, Path::new("work"));

    for access in [Access::Build, Access::Experiment] {
        let found = limits.check_session(&spec(access));
        assert_eq!(kinds(&found), vec![HardLimit::NoGoalHash], "{access:?}");

        let mut blank = spec(access);
        blank.goal_sha256 = Some("  ".to_owned());
        assert_eq!(
            kinds(&limits.check_session(&blank)),
            vec![HardLimit::NoGoalHash]
        );

        let mut hashed = spec(access);
        hashed.goal_sha256 = Some("f".repeat(64));
        assert_eq!(limits.check_session(&hashed), Vec::new());

        let mut bare = spec(access);
        bare.sandbox.mode = SandboxMode::None;
        assert_eq!(
            kinds(&limits.check_session(&bare)),
            vec![HardLimit::NoGoalHash, HardLimit::NoSandbox]
        );
    }
    for access in [Access::ReadOnly, Access::NoTools] {
        assert_eq!(limits.check_session(&spec(access)), Vec::new());
    }
    let mut contained = spec(Access::Build);
    contained.goal_sha256 = Some("f".repeat(64));
    contained.sandbox.mode = SandboxMode::Container;
    assert_eq!(limits.check_session(&contained), Vec::new());
}

#[test]
fn check_session_refuses_unsandboxed_cli() {
    let directory = FakeDirectory::new();
    directory.add(FakeRunner::cli("claude-code/haiku", "anthropic"));
    directory.add(FakeRunner::endpoint("ollama/llama3.2", "meta"));
    let limits = limits_with(sandboxed(), Arc::new(directory));
    let unsandboxed = |worker: &str, access: Access| {
        let mut spec = session_spec(worker, access, Path::new("work"));
        spec.sandbox.mode = SandboxMode::None;
        limits.check_session(&spec)
    };

    let cli = unsandboxed("claude-code/haiku", Access::ReadOnly);
    assert_eq!(kinds(&cli), vec![HardLimit::NoSandbox]);
    assert!(cli[0].detail.contains("claude-code/haiku"));
    assert_eq!(
        kinds(&unsandboxed("claude-code/haiku", Access::NoTools)),
        vec![HardLimit::NoSandbox]
    );
    assert_eq!(unsandboxed("ollama/llama3.2", Access::NoTools), Vec::new());
    // A worker the directory doesn't know is held to a CLI's rules.
    assert_eq!(
        kinds(&unsandboxed("mystery/model", Access::ReadOnly)),
        vec![HardLimit::NoSandbox]
    );
}

#[test]
fn decision_points_check_every_limit_the_evidence_shows() {
    let mut settings = sandboxed();
    settings.limits.dependency_allowlist = vec!["serde".to_owned(), "lodash@4.17.21".to_owned()];
    settings.limits.spend_per_day = Some(40.0);
    settings.limits.auto_merges_per_day = Some(3);
    let merge = |facts: EvidenceFacts| check_point(settings.clone(), ApprovalKind::Merge, facts);

    assert_eq!(merge(good_merge()), Vec::new());

    let cases: Vec<(EvidenceFacts, HardLimit)> = vec![
        (
            EvidenceFacts {
                checks_passed: None,
                ..good_merge()
            },
            HardLimit::ChecksFailed,
        ),
        (
            EvidenceFacts {
                checks_passed: Some(false),
                ..good_merge()
            },
            HardLimit::ChecksFailed,
        ),
        (
            EvidenceFacts {
                protected_paths: vec!["infra/x".to_owned()],
                ..good_merge()
            },
            HardLimit::ProtectedPath,
        ),
        (
            EvidenceFacts {
                new_dependencies: vec!["lodash@5.0.0".to_owned()],
                ..good_merge()
            },
            HardLimit::Dependency,
        ),
        (
            EvidenceFacts {
                agent_paths: vec!["web/yarn.lock".to_owned()],
                ..good_merge()
            },
            HardLimit::Lockfile,
        ),
        (
            EvidenceFacts {
                spent_today_usd: 40.0,
                ..good_merge()
            },
            HardLimit::Ceiling,
        ),
        (
            EvidenceFacts {
                spent_today_usd: f64::NAN,
                ..good_merge()
            },
            HardLimit::Ceiling,
        ),
        (
            EvidenceFacts {
                send_backs: 2,
                ..good_merge()
            },
            HardLimit::SentBackTwice,
        ),
        (
            EvidenceFacts {
                auto_merges_today: 3,
                ..good_merge()
            },
            HardLimit::AutoMergeCap,
        ),
        (
            EvidenceFacts {
                acceptance_tests_valid: Some(false),
                ..good_merge()
            },
            HardLimit::TestPassesOnStubs,
        ),
        (
            EvidenceFacts {
                untested_criteria: vec!["G2".to_owned()],
                ..good_merge()
            },
            HardLimit::CriterionUntested,
        ),
        (
            EvidenceFacts {
                tamper_flags: Some(1),
                ..good_merge()
            },
            HardLimit::TamperFlag,
        ),
    ];
    for (facts, limit) in cases {
        let found = merge(facts);
        let kinds = kinds(&found);
        assert!(kinds.contains(&limit), "{limit:?}: {found:?}");
    }

    // Unset, nothing caps the day's automatic merges.
    let mut uncapped = settings.clone();
    uncapped.limits.auto_merges_per_day = None;
    let found = check_point(
        uncapped,
        ApprovalKind::Merge,
        EvidenceFacts {
            auto_merges_today: 500,
            ..good_merge()
        },
    );
    assert_eq!(found, Vec::new());

    let allowed = EvidenceFacts {
        new_dependencies: vec![
            "serde".to_owned(),
            "serde@1.0".to_owned(),
            "lodash@4.17.21".to_owned(),
        ],
        spent_today_usd: 39.99,
        send_backs: 1,
        auto_merges_today: 2,
        ..good_merge()
    };
    assert_eq!(merge(allowed), Vec::new());

    // A plan checks the paths its tasks own, globs included, but not broad ones.
    let plan = |owned: &[&str]| {
        let facts = EvidenceFacts {
            subject: subject(),
            agent_paths: owned.iter().map(|p| (*p).to_owned()).collect(),
            auto_merges_today: 99,
            ..EvidenceFacts::default()
        };
        kinds(&check_point(settings.clone(), ApprovalKind::Plan, facts))
    };
    assert_eq!(plan(&["src/**", "**", "tests/*.rs"]), Vec::new());
    assert_eq!(plan(&[".github/**"]), vec![HardLimit::ProtectedPath]);
    assert_eq!(
        plan(&["**/Cargo.lock"]),
        vec![HardLimit::Lockfile, HardLimit::ProtectedPath]
    );
    // No daily ceiling set means none to hit, and none is set unless you set one.
    let unlimited = sandboxed();
    assert_eq!(unlimited.limits.spend_per_day, None);
    let spent = EvidenceFacts {
        spent_today_usd: 1e6,
        ..good_merge()
    };
    assert_eq!(
        check_point(unlimited, ApprovalKind::Merge, spent),
        Vec::new()
    );
}
