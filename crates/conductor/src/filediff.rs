//! A file's diff against the worktree's last commit, after an agent changed it, so previews look
//! the same for every agent: `git diff --no-color -U3 -- <path>` with hooks off, at most 40 lines,
//! at most once a second per path. Git's output is read as it comes and only what the preview
//! shows is kept, and git gets a few seconds before it's killed.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use argonauts_core::AgentEvent;
use argonauts_core::events::session::FileDiff;
use argonauts_core::patch_files::patched_files;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::process::read_capped_line;

pub const MAX_HUNK_LINES: usize = 40;
pub const PER_PATH_EVERY: Duration = Duration::from_secs(1);

/// How long one git command may run before it's killed, such as on a huge generated file.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest diff line kept, in bytes. A minified file can make one line of many megabytes.
const MAX_LINE_BYTES: usize = 16 * 1024;
/// Git's error output kept for the error message, in bytes.
const MAX_STDERR_BYTES: usize = 4 * 1024;

/// Diffing reads the agent's worktree, so nothing in it may run a command: no hooks, no
/// fsmonitor, no external diff or text conversion, and paths are never pathspec magic.
const SAFE_GIT: [&str; 6] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "--literal-pathspecs",
    "--no-optional-locks",
];
const DIFF: [&str; 5] = [
    "diff",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "-U3",
];

/// Inherited variables that would point git at another repository or index.
const REPOSITORY_ENV: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

#[derive(Debug)]
pub struct FileDiffs {
    worktree: PathBuf,
    /// The worktree with symlinks resolved, for absolute paths such as `/private/tmp/...`.
    resolved: Option<PathBuf>,
    git_dir: Option<PathBuf>,
    env: Vec<(String, String)>,
    last_run: HashMap<String, Instant>,
    owed: BTreeSet<String>,
    /// The last diff sent for each path, so the same one never goes out twice.
    sent: HashMap<String, FileDiff>,
    edits: EditTracker,
}

impl FileDiffs {
    pub fn new(worktree: PathBuf) -> Self {
        Self {
            resolved: worktree.canonicalize().ok(),
            worktree,
            git_dir: None,
            env: Vec::new(),
            last_run: HashMap::new(),
            owed: BTreeSet::new(),
            sent: HashMap::new(),
            edits: EditTracker::default(),
        }
    }

    /// Uses `git_dir` instead of the `.git` file in the worktree, which the agent could point at
    /// a repository whose config runs commands.
    pub fn with_git_dir(mut self, git_dir: PathBuf) -> Self {
        self.git_dir = Some(git_dir);
        self
    }

    /// Extra environment for git, such as a test's config.
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// The diff for `path`, relative to the worktree, unless one went out for it less than a
    /// second ago; that path then waits for [`FileDiffs::flush`] or its next change. Paths
    /// outside the worktree, and changes that leave the diff as it was, give `None`.
    pub async fn changed(&mut self, path: &str) -> io::Result<Option<FileDiff>> {
        self.changed_at(path, Instant::now()).await
    }

    /// Diffs for the paths `event` says changed, throttled like [`FileDiffs::changed`].
    pub async fn observe(&mut self, event: &AgentEvent) -> io::Result<Vec<FileDiff>> {
        let mut diffs = Vec::new();
        for path in self.edits.observe(event) {
            diffs.extend(self.changed(&path).await?);
        }
        Ok(diffs)
    }

    /// Diffs still owed, so the last change of every path goes out before the session ends.
    pub async fn flush(&mut self) -> io::Result<Vec<FileDiff>> {
        let now = Instant::now();
        let mut diffs = Vec::new();
        for path in std::mem::take(&mut self.owed) {
            diffs.extend(self.run(path, now).await?);
        }
        Ok(diffs)
    }

    /// Owed diffs whose second has passed, for a timer between changes.
    pub async fn due(&mut self) -> io::Result<Vec<FileDiff>> {
        self.due_at(Instant::now()).await
    }

    /// When the next owed diff may go out.
    pub fn next_due(&self) -> Option<Instant> {
        self.owed
            .iter()
            .filter_map(|path| self.last_run.get(path))
            .min()
            .map(|at| *at + PER_PATH_EVERY)
    }

    async fn changed_at(&mut self, path: &str, now: Instant) -> io::Result<Option<FileDiff>> {
        let Some(path) = self.relative(path) else {
            return Ok(None);
        };
        if self.throttled(&path, now) {
            self.owed.insert(path);
            return Ok(None);
        }
        self.owed.remove(&path);
        self.run(path, now).await
    }

    async fn due_at(&mut self, now: Instant) -> io::Result<Vec<FileDiff>> {
        let ready: Vec<String> = self
            .owed
            .iter()
            .filter(|path| !self.throttled(path, now))
            .cloned()
            .collect();
        let mut diffs = Vec::new();
        for path in ready {
            self.owed.remove(&path);
            diffs.extend(self.run(path, now).await?);
        }
        Ok(diffs)
    }

    fn throttled(&self, path: &str, now: Instant) -> bool {
        self.last_run
            .get(path)
            .is_some_and(|at| now.saturating_duration_since(*at) < PER_PATH_EVERY)
    }

    async fn run(&mut self, path: String, now: Instant) -> io::Result<Option<FileDiff>> {
        self.last_run.insert(path.clone(), now);
        let diff = self.diff(&path).await?;
        let repeated = self.sent.get(&path) == Some(&diff);
        // A path never shown that turns out unchanged has nothing to preview. One shown before
        // gets its empty diff, so the preview clears.
        let nothing_new = diff.hunk.is_empty() && !self.sent.contains_key(&path);
        if repeated || nothing_new {
            return Ok(None);
        }
        self.sent.insert(path, diff.clone());
        Ok(Some(diff))
    }

    async fn diff(&self, path: &str) -> io::Result<FileDiff> {
        let tracked = self
            .streamed_diff(&[&DIFF[..], &["HEAD", "--", path]].concat(), &[0])
            .await?;
        if tracked.saw_output || !self.worktree.join(path).exists() {
            return Ok(tracked.finish(path));
        }
        let listed = self.git(&["ls-files", "--", path]).await?;
        if !listed.status.success() {
            return Err(git_failed(&listed.stderr));
        }
        if !listed.stdout.is_empty() {
            return Ok(DiffParser::default().finish(path));
        }
        // A new file git doesn't know yet, shown as added in full. `--no-index` exits with 1
        // when the files are different.
        let added = self
            .streamed_diff(
                &[&DIFF[..], &["--no-index", "--", "/dev/null", path]].concat(),
                &[0, 1],
            )
            .await?;
        Ok(added.finish(path))
    }

    /// What the worktree holds now, as one number that changes whenever a tracked file's
    /// content changes, or the files git doesn't track, their sizes or their times do. Reading
    /// it runs git twice and changes nothing.
    pub async fn fingerprint(&self) -> io::Result<u64> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        use std::os::unix::ffi::OsStrExt;

        let status = self
            .git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])
            .await?;
        if !status.status.success() {
            return Err(git_failed(&status.stderr));
        }
        let tracked = self.git(&[&DIFF[..], &["HEAD"]].concat()).await?;
        if !tracked.status.success() {
            return Err(git_failed(&tracked.stderr));
        }
        let mut hasher = DefaultHasher::new();
        status.stdout.hash(&mut hasher);
        tracked.stdout.hash(&mut hasher);
        for entry in status.stdout.split(|byte| *byte == 0) {
            let Some(path) = entry.strip_prefix(b"?? ") else {
                continue;
            };
            let path = self.worktree.join(std::ffi::OsStr::from_bytes(path));
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                meta.len().hash(&mut hasher);
                meta.modified().ok().hash(&mut hasher);
            }
        }
        Ok(hasher.finish())
    }

    /// A small command's whole output.
    async fn git(&self, args: &[&str]) -> io::Result<Output> {
        within_timeout(self.command(args).output()).await?
    }

    /// A diff, parsed line by line as git writes it. `ok` are the exit codes that aren't
    /// failures.
    async fn streamed_diff(&self, args: &[&str], ok: &[i32]) -> io::Result<DiffParser> {
        let mut child = self.command(args).spawn()?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(io::Error::other("git started without its pipes"));
        };
        // Dropping the child on a timeout kills git.
        let (parsed, errors, status) = within_timeout(async move {
            let (parsed, errors) = tokio::join!(parse_stream(stdout), read_start(stderr));
            let status: ExitStatus = child.wait().await?;
            io::Result::Ok((parsed?, errors?, status))
        })
        .await??;
        if status.code().is_some_and(|code| ok.contains(&code)) {
            Ok(parsed)
        } else {
            Err(git_failed(&errors))
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.worktree);
        if let Some(git_dir) = &self.git_dir {
            command
                .arg(flag_with_path("--git-dir=", git_dir))
                .arg(flag_with_path("--work-tree=", &self.worktree));
        }
        command
            .args(SAFE_GIT)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .kill_on_drop(true);
        for name in REPOSITORY_ENV {
            command.env_remove(name);
        }
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        command
    }

    /// `path` relative to the worktree, or `None` when it points outside it. Agents give
    /// relative paths, absolute ones, and agy's `file://` URIs.
    fn relative(&self, path: &str) -> Option<String> {
        let path = Path::new(path.strip_prefix("file://").unwrap_or(path));
        let relative = if path.is_absolute() {
            [Some(&self.worktree), self.resolved.as_ref()]
                .into_iter()
                .flatten()
                .find_map(|root| path.strip_prefix(root).ok())?
        } else {
            path
        };
        let mut parts = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_str()?),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        (!parts.is_empty()).then(|| parts.join("/"))
    }
}

fn flag_with_path(flag: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(flag);
    arg.push(path);
    arg
}

fn git_failed(stderr: &[u8]) -> io::Error {
    io::Error::other(format!(
        "git diff failed: {}",
        String::from_utf8_lossy(stderr).trim()
    ))
}

async fn within_timeout<T>(work: impl Future<Output = T>) -> io::Result<T> {
    tokio::time::timeout(GIT_TIMEOUT, work).await.map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("git took more than {} seconds", GIT_TIMEOUT.as_secs()),
        )
    })
}

async fn parse_stream(stdout: impl AsyncRead + Unpin) -> io::Result<DiffParser> {
    let mut reader = BufReader::new(stdout);
    let mut parser = DiffParser::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        if read_capped_line(&mut reader, &mut line, MAX_LINE_BYTES)
            .await?
            .consumed
            == 0
        {
            return Ok(parser);
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        parser.push(&String::from_utf8_lossy(&line));
    }
}

/// The first `MAX_STDERR_BYTES` of a stream, reading the rest so the writer never blocks.
async fn read_start(stream: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut stream = stream;
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(kept);
        }
        let room = MAX_STDERR_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(room)]);
    }
}

/// Counts every added and removed line, and keeps the hunks from the first `@@` on, cut to
/// [`MAX_HUNK_LINES`].
#[derive(Debug, Default)]
struct DiffParser {
    added: u32,
    removed: u32,
    hunk: Vec<String>,
    hunk_lines: usize,
    in_hunks: bool,
    saw_output: bool,
}

impl DiffParser {
    fn push(&mut self, line: &str) {
        self.saw_output = true;
        if line.starts_with("@@") || line.starts_with("Binary files ") {
            self.in_hunks = true;
        }
        if !self.in_hunks {
            return;
        }
        match line.as_bytes().first() {
            Some(b'+') => self.added = self.added.saturating_add(1),
            Some(b'-') => self.removed = self.removed.saturating_add(1),
            _ => {}
        }
        self.hunk_lines += 1;
        if self.hunk.len() < MAX_HUNK_LINES {
            self.hunk.push(line.to_owned());
        }
    }

    fn finish(self, path: &str) -> FileDiff {
        FileDiff {
            path: path.to_owned(),
            added: self.added,
            removed: self.removed,
            hunk: self.hunk.join("\n"),
            truncated: self.hunk_lines > MAX_HUNK_LINES,
        }
    }
}

/// Paths an event says changed: `FileChanged`, or an edit tool finishing (Claude Code's `Edit`
/// and `Write`, Codex's `file_change`, agy's edit tools). A finish doesn't name its tool, so
/// only `FileChanged` is read here; [`EditTracker`] pairs edit tools with their finish.
pub fn changed_paths(event: &AgentEvent) -> Vec<String> {
    match event {
        AgentEvent::FileChanged { path, .. } => vec![path.clone()],
        _ => Vec::new(),
    }
}

/// Remembers the edit tools that started, and names their paths when they finish well.
#[derive(Debug, Default)]
pub struct EditTracker {
    open: HashMap<String, Vec<String>>,
}

impl EditTracker {
    pub fn observe(&mut self, event: &AgentEvent) -> Vec<String> {
        match event {
            AgentEvent::ToolStarted {
                id, name, input, ..
            } => {
                let paths = edit_paths(name, input);
                if !paths.is_empty() {
                    self.open.insert(id.clone(), paths);
                }
                Vec::new()
            }
            AgentEvent::ToolFinished { id, ok, .. } => {
                let paths = self.open.remove(id).unwrap_or_default();
                if *ok { paths } else { Vec::new() }
            }
            other => changed_paths(other),
        }
    }
}

/// Whether a call writes a file, by the agent's tool name and the path its input names.
pub fn writes_files(tool: &str, input: &Value) -> bool {
    !edit_paths(tool, input).is_empty()
}

/// The paths an edit tool's input names, by the agent's tool name.
fn edit_paths(tool: &str, input: &Value) -> Vec<String> {
    let at = |keys: &[&str]| -> Vec<String> {
        keys.iter()
            .find_map(|key| input.get(*key).and_then(Value::as_str))
            .map(|path| vec![path.to_owned()])
            .unwrap_or_default()
    };
    match tool {
        // Claude Code.
        "Edit" | "Write" | "MultiEdit" => at(&["file_path"]),
        "NotebookEdit" => at(&["notebook_path"]),
        // Codex.
        "file_change" => input
            .get("changes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|change| change.get("path").and_then(Value::as_str))
            .map(str::to_owned)
            .collect(),
        // agy 1.2.13.
        "write_to_file" | "replace_file_content" | "multi_replace_file_content" | "sed_file" => {
            at(&["TargetFile", "File", "AbsolutePath"])
        }
        // OpenCode.
        "write" | "edit" | "multiedit" => at(&["filePath"]),
        // OpenCode 2.0.32 names its apply_patch tool `patch`.
        "patch" | "apply_patch" => input
            .get("patchText")
            .and_then(Value::as_str)
            .map(patched_files)
            .unwrap_or_default()
            .into_iter()
            .map(|(path, _)| path)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};
    use serde_json::json;

    fn numbered(count: usize, tag: &str) -> String {
        (1..=count).map(|n| format!("line {n}{tag}\n")).collect()
    }

    fn file_diffs(repo: &TestRepo) -> FileDiffs {
        FileDiffs::new(repo.root.clone()).with_env(git_test_env())
    }

    #[tokio::test]
    async fn file_diff_capped_and_throttled() {
        let repo = TestRepo::new();
        repo.write("big.txt", &numbered(100, ""));
        repo.git(&["add", "big.txt"]);
        repo.git(&["commit", "--quiet", "-m", "big"]);
        let mut file_diffs = file_diffs(&repo);
        let start = Instant::now();

        repo.write("big.txt", &numbered(30, " changed"));
        let diff = file_diffs
            .changed_at("big.txt", start)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(diff.path, "big.txt");
        assert_eq!((diff.added, diff.removed), (30, 100));
        assert!(diff.truncated);
        assert_eq!(diff.hunk.lines().count(), MAX_HUNK_LINES);
        assert!(diff.hunk.starts_with("@@ "));

        // Within the second, the path waits.
        repo.write("big.txt", &numbered(100, ""));
        repo.write("big.txt", &format!("{}extra\n", numbered(100, "")));
        let soon = start + Duration::from_millis(300);
        assert_eq!(file_diffs.changed_at("big.txt", soon).await.unwrap(), None);
        assert_eq!(file_diffs.next_due(), Some(start + PER_PATH_EVERY));
        assert!(file_diffs.due_at(soon).await.unwrap().is_empty());
        let owed = file_diffs.flush().await.unwrap();
        assert_eq!(owed.len(), 1);
        assert_eq!((owed[0].added, owed[0].removed), (1, 0));
        assert!(!owed[0].truncated);
        assert!(file_diffs.flush().await.unwrap().is_empty());

        // The same diff never goes out twice, and undoing the edit clears the preview.
        let later = Instant::now() + PER_PATH_EVERY;
        assert_eq!(file_diffs.changed_at("big.txt", later).await.unwrap(), None);
        repo.write("big.txt", &numbered(100, ""));
        let cleared = file_diffs
            .changed_at("big.txt", later + PER_PATH_EVERY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((cleared.added, cleared.removed), (0, 0));
        assert!(cleared.hunk.is_empty());
    }

    #[tokio::test]
    async fn owed_diffs_go_out_once_their_second_passes() {
        let repo = TestRepo::new();
        let mut file_diffs = file_diffs(&repo);
        let start = Instant::now();
        repo.write("README.md", "# Test\nmore\n");
        assert!(
            file_diffs
                .changed_at("README.md", start)
                .await
                .unwrap()
                .is_some()
        );
        repo.write("README.md", "# Test\nmore\nand more\n");
        assert_eq!(
            file_diffs.changed_at("./README.md", start).await.unwrap(),
            None
        );
        let due = file_diffs.due_at(start + PER_PATH_EVERY).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].added, 2);
        assert_eq!(file_diffs.next_due(), None);
    }

    #[tokio::test]
    async fn new_files_absolute_paths_and_outside_paths() {
        let repo = TestRepo::new();
        let mut file_diffs = file_diffs(&repo);
        repo.write("src/new.rs", "fn main() {}\n");
        let absolute = repo.root.join("src/new.rs");
        let diff = file_diffs
            .changed(&format!("file://{}", absolute.display()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(diff.path, "src/new.rs");
        assert_eq!((diff.added, diff.removed), (1, 0));
        assert!(diff.hunk.contains("+fn main() {}"));

        for outside in ["../escape.txt", "/etc/hosts", "", ":(glob)*"] {
            assert_eq!(
                file_diffs.changed(outside).await.unwrap(),
                None,
                "{outside}"
            );
        }
        // Unchanged and deleted files.
        assert_eq!(file_diffs.changed("README.md").await.unwrap(), None);
        std::fs::remove_file(repo.root.join("README.md")).unwrap();
        assert_eq!(file_diffs.changed("README.md").await.unwrap(), None);
        let deleted = file_diffs.flush().await.unwrap();
        assert_eq!((deleted[0].added, deleted[0].removed), (0, 1));
    }

    #[test]
    fn edit_tools_count_once_they_finish() {
        let started = |id: &str, name: &str, input: Value| AgentEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
            input,
            parent: None,
        };
        let finished = |id: &str, ok: bool| AgentEvent::ToolFinished {
            id: id.into(),
            ok,
            output: None,
            parent: None,
        };
        let mut tracker = EditTracker::default();
        let starts = [
            started("1", "Edit", json!({"file_path": "/w/a.rs"})),
            started("2", "MultiEdit", json!({"file_path": "b.rs"})),
            started("3", "NotebookEdit", json!({"notebook_path": "n.ipynb"})),
            started(
                "4",
                "file_change",
                json!({"changes": [{"path": "/w/c.txt", "kind": "add"}, {"path": "/w/d.txt", "kind": "delete"}]}),
            ),
            started(
                "5",
                "write_to_file",
                json!({"TargetFile": "file:///w/e.md"}),
            ),
            started("6", "Bash", json!({"command": "ls"})),
            started("7", "Write", json!({"file_path": "f.rs"})),
        ];
        for event in &starts {
            assert!(tracker.observe(event).is_empty());
        }
        assert_eq!(tracker.observe(&finished("1", true)), ["/w/a.rs"]);
        assert_eq!(tracker.observe(&finished("2", true)), ["b.rs"]);
        assert_eq!(tracker.observe(&finished("3", true)), ["n.ipynb"]);
        assert_eq!(
            tracker.observe(&finished("4", true)),
            ["/w/c.txt", "/w/d.txt"]
        );
        assert_eq!(tracker.observe(&finished("5", true)), ["file:///w/e.md"]);
        assert!(tracker.observe(&finished("6", true)).is_empty());
        assert!(tracker.observe(&finished("7", false)).is_empty());
        assert!(tracker.observe(&finished("1", true)).is_empty());

        let changed = AgentEvent::FileChanged {
            path: "g.rs".into(),
            kind: "update".into(),
        };
        assert_eq!(changed_paths(&changed), ["g.rs"]);
        assert_eq!(tracker.observe(&changed), ["g.rs"]);
    }
}
