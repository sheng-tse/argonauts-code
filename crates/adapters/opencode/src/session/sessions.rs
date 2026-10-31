//! OpenCode's own sessions for `/resume`, as `opencode session list --format json` gives them,
//! on a private server, so listing never goes through the shared service. Nothing else of
//! OpenCode's store is read. Both commands run the program the adapter was given, never a bare
//! name looked up on `PATH`, and inside the sandbox it was given, since OpenCode started in a
//! repo loads the plugins that repo sets up.
//!
//! OpenCode 2.0.32 lists the top-level sessions of the folder's project (its git repo), newest
//! first, each as `{id, title, updated, created, projectId, directory}` with its times in
//! milliseconds (read from its program text). A session's turns come from
//! `opencode session export <id>`, which prints `{data: {info, messages}}`: your messages as
//! `{type: "user", text}`, and the agent's as `{type: "assistant", content}` with parts of type
//! `text`, `reasoning` and `tool` (the 2.0.26 API's own schema).

use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use argonauts_core::agents::cli_sessions::{CliScan, CliSession, CliTurn, Speaker};
use argonauts_core::agents::confine::Confine;
use argonauts_core::base::names::AgentId;
use argonauts_core::{CommandSpec, env_remove_except};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::models::private_server::private_server_flags;
use crate::session::command::AUTO_UPDATE_ENV;

/// A fresh private server takes a few seconds before it answers.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
struct Listed {
    id: String,
    #[serde(default)]
    title: String,
    /// Milliseconds since 1970.
    updated: i64,
    #[serde(default)]
    directory: Option<PathBuf>,
}

/// How OpenCode's sessions are read: its program, and the sandbox each command runs in.
#[derive(Debug, Clone)]
pub(crate) struct Reader {
    pub(crate) program: PathBuf,
    pub(crate) confine: Arc<dyn Confine>,
}

impl Reader {
    /// What `spec` printed, run inside the sandbox.
    fn output(&self, spec: CommandSpec) -> anyhow::Result<String> {
        let shown = format!("{} {}", spec.program, spec.args.join(" "));
        let confined = wait_for(self.confine.confine(crate::ID, spec))?
            .map_err(|why| anyhow::anyhow!("`{shown}` didn't run: {why}"))?;
        run(&confined, &shown, LIST_TIMEOUT)
    }
}

/// Waits for the sandbox, which may test itself first, on this blocking thread: through the
/// app's runtime when there is one, else on a small one of its own.
fn wait_for<F: Future>(future: F) -> anyhow::Result<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => Ok(runtime.block_on(future)),
        Err(_) => Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(future)),
    }
}

/// The sessions OpenCode lists for `scan.folder`'s project: those that ran in the folder, or in
/// any of the project's folders for a scan of every folder, at most `limit`. None, and nothing
/// run, without a `reader` or while OpenCode keeps no store under `scan.home`.
pub(crate) fn list(
    reader: Option<&Reader>,
    scan: &CliScan,
    limit: usize,
) -> anyhow::Result<Vec<CliSession>> {
    let Some(reader) = reader.filter(|_| has_store(scan)) else {
        return Ok(Vec::new());
    };
    let printed = reader.output(list_command(&reader.program, &scan.folder, limit))?;
    parse(&printed, scan, limit)
}

/// The last `count` turns of `session`, oldest first: your words, and what the agent wrote
/// after them. None without a `reader` or while OpenCode keeps no store under `scan.home`.
pub(crate) fn turns(
    reader: Option<&Reader>,
    scan: &CliScan,
    session: &str,
    count: usize,
) -> anyhow::Result<Vec<CliTurn>> {
    let Some(reader) = reader.filter(|_| has_store(scan)) else {
        return Ok(Vec::new());
    };
    if session.starts_with('-') {
        bail!("{session:?} isn't a session id");
    }
    let printed = reader.output(export_command(&reader.program, &scan.folder, session))?;
    parse_turns(&printed, count)
}

fn has_store(scan: &CliScan) -> bool {
    store(
        &scan.home,
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
    )
    .is_dir()
}

fn export_command(program: &Path, folder: &Path, session: &str) -> CommandSpec {
    let mut cmd = CommandSpec::new(program.display().to_string(), folder)
        .args(["session", "export"])
        .args(private_server_flags(None).iter().copied())
        .arg(session);
    cmd.env_set
        .push((AUTO_UPDATE_ENV.to_owned(), "1".to_owned()));
    cmd.env_remove = env_remove_except(&[]);
    cmd
}

fn parse_turns(printed: &str, count: usize) -> anyhow::Result<Vec<CliTurn>> {
    let exported: serde_json::Value =
        serde_json::from_str(printed).context("OpenCode's export isn't the JSON it prints")?;
    let exported = exported.get("data").unwrap_or(&exported);
    let messages = exported["messages"]
        .as_array()
        .context("OpenCode's export has no messages")?;
    let mut turns: Vec<CliTurn> = Vec::new();
    for message in messages {
        match message["type"].as_str() {
            Some("user") => {
                let text = message["text"].as_str().unwrap_or_default().trim();
                let sent = message["time"]["created"].as_i64().and_then(|ms| {
                    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
                });
                if !text.is_empty() {
                    turns.push(CliTurn::prompt(text).sent_at(sent));
                }
            }
            Some("assistant") => {
                if turns.last().is_none_or(|turn| turn.from != Speaker::Agent) {
                    turns.push(CliTurn::of_the_agent());
                }
                let Some(reply) = turns.last_mut() else {
                    continue;
                };
                for part in message["content"].as_array().into_iter().flatten() {
                    if part["type"] == "text" {
                        reply.wrote(part["text"].as_str().unwrap_or_default());
                    }
                }
            }
            _ => {}
        }
    }
    turns.retain(|turn| turn.from == Speaker::User || !turn.text.is_empty());
    let skip = turns.len().saturating_sub(count);
    Ok(turns.split_off(skip))
}

/// Where OpenCode keeps its sessions: `$XDG_DATA_HOME/opencode`, else
/// `~/.local/share/opencode`.
fn store(home: &Path, xdg_data: Option<PathBuf>) -> PathBuf {
    xdg_data
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join("opencode")
}

/// The version isn't read for a list, so it gets the private server today's OpenCode needs.
fn list_command(program: &Path, folder: &Path, limit: usize) -> CommandSpec {
    let count = limit.clamp(1, u32::MAX as usize).to_string();
    let mut cmd = CommandSpec::new(program.display().to_string(), folder)
        .args(["session", "list"])
        .args(private_server_flags(None).iter().copied())
        .args(["--format", "json", "--max-count", &count]);
    cmd.env_set
        .push((AUTO_UPDATE_ENV.to_owned(), "1".to_owned()));
    cmd.env_remove = env_remove_except(&[]);
    cmd
}

fn parse(printed: &str, scan: &CliScan, limit: usize) -> anyhow::Result<Vec<CliSession>> {
    if printed.trim().is_empty() {
        return Ok(Vec::new());
    }
    let listed: Vec<Listed> = serde_json::from_str(printed)
        .context("OpenCode's session list isn't the JSON it prints")?;
    let sessions = listed
        .into_iter()
        .filter_map(|listed| {
            let folder = listed.directory.unwrap_or_else(|| scan.folder.clone());
            if !scan.every_folder && folder != scan.folder {
                return None;
            }
            let updated =
                OffsetDateTime::from_unix_timestamp_nanos(i128::from(listed.updated) * 1_000_000)
                    .ok()?;
            Some(CliSession {
                agent_id: AgentId("opencode".to_owned()),
                id: listed.id,
                title: one_line(&listed.title),
                prompt: None,
                folder,
                updated,
                model: None,
                branch: None,
                last_message: None,
                turns: 0,
            })
        })
        .take(limit)
        .collect();
    Ok(sessions)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What `spec` printed once it exits with success, waiting at most `timeout`. Its output is
/// read as it comes, so a long list can't fill the pipe and stall it. `shown` names it in what
/// goes wrong, as it reads before the sandbox wraps it.
fn run(spec: &CommandSpec, shown: &str, timeout: Duration) -> anyhow::Result<String> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    command.envs(spec.env_set.iter().cloned());
    let mut child = command
        .spawn()
        .with_context(|| format!("couldn't run `{shown}`"))?;
    let mut stdout = child.stdout.take().context("no output to read")?;
    let reader = std::thread::spawn(move || {
        let mut printed = String::new();
        stdout.read_to_string(&mut printed).map(|_| printed)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            // The private server reads its client's pipe, so it ends with the client.
            let _ = child.kill();
            let _ = child.wait();
            bail!("`{shown}` took longer than {} s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let printed = reader
        .join()
        .map_err(|_| anyhow::anyhow!("reading `{shown}` failed"))?
        .with_context(|| format!("`{shown}` printed text that isn't UTF-8"))?;
    if !status.success() {
        bail!("`{shown}` failed ({status})");
    }
    Ok(printed)
}

#[cfg(test)]
mod tests {

    use argonauts_core::agents::confine::Unconfined;
    use argonauts_core::base::future::BoxFuture;

    use super::*;
    use crate::session::test_script::write_script;

    const OPENCODE: &str = "/opt/opencode/bin/opencode";

    /// As OpenCode 2.0.32 prints it: pretty JSON, newest first.
    const LISTED: &str = r#"[
  {
    "id": "ses_eae352190ffeOmLG7bRmRZphzX",
    "title": "Fix the flaky\n test",
    "updated": 1792364400000,
    "created": 1792364000000,
    "projectId": "4b0ea68d7af9a6031a7ffda7ad66e0cb83315750",
    "directory": "/work/repo"
  },
  {
    "id": "ses_eae33ed1dffeBeVXCqn5atkhlz",
    "title": "Add a README",
    "updated": 1792360800000,
    "created": 1792360000000,
    "projectId": "4b0ea68d7af9a6031a7ffda7ad66e0cb83315750",
    "directory": "/work/task-001"
  }
]
"#;

    #[test]
    fn the_folders_sessions_newest_first() {
        let scan = CliScan::new("/work/repo", "/home/me");
        let sessions = parse(LISTED, &scan, 10).unwrap();
        assert_eq!(sessions.len(), 1);
        let first = &sessions[0];
        assert_eq!(first.agent_id.0, "opencode");
        assert_eq!(first.id, "ses_eae352190ffeOmLG7bRmRZphzX");
        assert_eq!(first.title, "Fix the flaky test");
        assert_eq!(first.folder, PathBuf::from("/work/repo"));
        assert_eq!(first.updated.unix_timestamp(), 1_792_364_400);

        let everywhere = parse(LISTED, &scan.clone().everywhere(), 10).unwrap();
        assert_eq!(everywhere.len(), 2);
        assert_eq!(everywhere[1].folder, PathBuf::from("/work/task-001"));
        assert_eq!(parse(LISTED, &scan.everywhere(), 1).unwrap().len(), 1);
        assert!(
            parse("\n", &CliScan::new("/w", "/h"), 5)
                .unwrap()
                .is_empty()
        );
        assert!(parse("Error: no project", &CliScan::new("/w", "/h"), 5).is_err());
    }

    /// Shaped as the 2.0.26 API's schema gives an export: two exchanges, the second answered in
    /// two steps, one of which ran a tool.
    const EXPORTED: &str = r#"{"data": {"info": {"id": "ses_1"}, "messages": [
        {"id": "msg_1", "type": "user", "time": {"created": 1}, "text": "What's the code word?"},
        {"id": "msg_2", "type": "assistant", "time": {"created": 2},
         "content": [{"type": "reasoning", "text": "They asked"}, {"type": "text", "text": "PELICAN"}]},
        {"id": "msg_3", "type": "model-switched", "time": {"created": 3}, "model": {}},
        {"id": "msg_4", "type": "user", "time": {"created": 4}, "text": "Read the file."},
        {"id": "msg_5", "type": "assistant", "time": {"created": 5},
         "content": [{"type": "tool", "name": "read"}]},
        {"id": "msg_6", "type": "assistant", "time": {"created": 6},
         "content": [{"type": "text", "text": "It says hi."}]}
    ]}}"#;

    #[test]
    fn turns_come_from_the_export_oldest_first() {
        let turns = parse_turns(EXPORTED, 10).unwrap();
        let said: Vec<(Speaker, &str)> = turns
            .iter()
            .map(|turn| (turn.from, turn.text.as_str()))
            .collect();
        assert_eq!(
            said,
            [
                (Speaker::User, "What's the code word?"),
                (Speaker::Agent, "PELICAN"),
                (Speaker::User, "Read the file."),
                (Speaker::Agent, "It says hi."),
            ]
        );
        assert_eq!(parse_turns(EXPORTED, 2).unwrap()[0].text, "Read the file.");
        let sent = turns[2].sent.map(OffsetDateTime::unix_timestamp_nanos);
        assert_eq!(sent, Some(4_000_000), "a prompt keeps when it was sent");
        assert!(parse_turns("not json", 2).is_err());
        let cmd = export_command(Path::new(OPENCODE), Path::new("/work/repo"), "ses_1");
        assert_eq!(cmd.args, ["session", "export", "--standalone", "ses_1"]);
    }

    #[test]
    fn it_lists_on_a_private_server_with_no_keys() {
        let cmd = list_command(Path::new(OPENCODE), Path::new("/work/repo"), 25);
        assert_eq!(cmd.program, OPENCODE);
        assert_eq!(cmd.cwd, PathBuf::from("/work/repo"));
        assert_eq!(
            cmd.args,
            [
                "session",
                "list",
                "--standalone",
                "--format",
                "json",
                "--max-count",
                "25"
            ]
        );
        assert_eq!(cmd.env_remove, env_remove_except(&[]));
        let every = list_command(Path::new(OPENCODE), Path::new("/w"), usize::MAX);
        assert_eq!(every.args.last().map(String::as_str), Some("4294967295"));
    }

    #[test]
    fn no_store_means_nothing_runs() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            store(home.path(), None),
            home.path().join(".local/share/opencode")
        );
        assert_eq!(
            store(home.path(), Some(PathBuf::from("/xdg"))),
            PathBuf::from("/xdg/opencode")
        );
        assert_eq!(
            store(home.path(), Some(PathBuf::from("relative"))),
            home.path().join(".local/share/opencode")
        );
        let scan = CliScan::new(home.path(), home.path());
        if std::env::var_os("XDG_DATA_HOME").is_none() {
            let reader = Reader {
                program: PathBuf::from(OPENCODE),
                confine: Arc::new(Unconfined),
            };
            assert!(list(Some(&reader), &scan, 5).unwrap().is_empty());
        }
    }

    #[test]
    fn no_program_means_nothing_runs() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".local/share/opencode")).unwrap();
        let scan = CliScan::new(home.path(), home.path());
        assert!(list(None, &scan, 5).unwrap().is_empty());
        assert!(turns(None, &scan, "ses_1", 4).unwrap().is_empty());
    }

    #[test]
    fn a_list_is_read_whole_and_a_slow_one_given_up() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("opencode");
        write_script(&program, "#!/bin/sh\ncat \"$0.json\"\n");
        std::fs::write(dir.path().join("opencode.json"), LISTED).unwrap();
        let spec = list_command(&program, dir.path(), 5);
        assert_eq!(
            run(&spec, "opencode session list", LIST_TIMEOUT).unwrap(),
            LISTED
        );

        let slow = CommandSpec::new("/bin/sleep", dir.path()).arg("5");
        let err = run(&slow, "sleep 5", Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("`sleep 5` took longer"), "{err}");
        let failing = CommandSpec::new("/bin/sh", dir.path()).args(["-c", "exit 2"]);
        assert!(run(&failing, "sh", LIST_TIMEOUT).is_err());
    }

    /// A sandbox that starts each command through `env SANDBOXED=1`, as a wrapper does, and
    /// keeps which CLI asked for it.
    #[derive(Debug, Default)]
    struct Marking {
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl Confine for Marking {
        fn confine<'a>(
            &'a self,
            cli: &'a str,
            command: CommandSpec,
        ) -> BoxFuture<'a, Result<CommandSpec, String>> {
            self.asked.lock().unwrap().push(cli.to_owned());
            let mut wrapped = CommandSpec::new("/usr/bin/env", &command.cwd)
                .arg("SANDBOXED=1")
                .arg(&command.program)
                .args(command.args);
            wrapped.env_set = command.env_set;
            wrapped.env_remove = command.env_remove;
            Box::pin(async move { Ok(wrapped) })
        }
    }

    /// A sandbox that can't start here.
    #[derive(Debug)]
    struct Broken;

    impl Confine for Broken {
        fn confine<'a>(
            &'a self,
            _cli: &'a str,
            _command: CommandSpec,
        ) -> BoxFuture<'a, Result<CommandSpec, String>> {
            Box::pin(async { Err("no working sandbox: sandbox-exec is missing".to_owned()) })
        }
    }

    /// A store in a home of its own, and an `opencode` that notes each call and lists the
    /// sessions only when it runs inside [`Marking`]'s sandbox.
    fn sandboxed_opencode(dir: &Path) -> (PathBuf, CliScan) {
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".local/share/opencode")).unwrap();
        let program = dir.join("opencode");
        let script = "#!/bin/sh\necho \"$*\" >> \"$0.calls\"\n\
                      if [ \"$SANDBOXED\" = 1 ]; then cat \"$0.json\"; fi\n";
        write_script(&program, script);
        std::fs::write(dir.join("opencode.json"), LISTED).unwrap();
        (program, CliScan::new(dir, home).everywhere())
    }

    #[test]
    fn it_lists_inside_the_sandbox_it_was_given_or_not_at_all() {
        if std::env::var_os("XDG_DATA_HOME").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (program, scan) = sandboxed_opencode(dir.path());
        let marking = Arc::new(Marking::default());
        let reader = Reader {
            program: program.clone(),
            confine: marking.clone(),
        };
        assert_eq!(list(Some(&reader), &scan, 5).unwrap().len(), 2);
        assert_eq!(*marking.asked.lock().unwrap(), ["opencode"]);

        let broken = Reader {
            program,
            confine: Arc::new(Broken),
        };
        let err = list(Some(&broken), &scan, 5).unwrap_err().to_string();
        assert!(err.contains("didn't run: no working sandbox"), "{err}");
        assert!(turns(Some(&broken), &scan, "ses_1", 4).is_err());
        let calls = std::fs::read_to_string(dir.path().join("opencode.calls")).unwrap();
        assert_eq!(calls.lines().count(), 1, "{calls}");
    }

    /// As the app lists them: on a blocking thread of its runtime.
    #[tokio::test]
    async fn the_sandbox_is_waited_for_on_the_runtime_s_blocking_thread() {
        if std::env::var_os("XDG_DATA_HOME").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (program, scan) = sandboxed_opencode(dir.path());
        let reader = Reader {
            program,
            confine: Arc::new(Marking::default()),
        };
        let listed = tokio::task::spawn_blocking(move || list(Some(&reader), &scan, 5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(listed.len(), 2);
    }
}
