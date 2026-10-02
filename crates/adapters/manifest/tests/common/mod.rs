//! Runs a manifest adapter's command the way the harness worker does, without a conductor:
//! files written, an argument vector, the environment adjusted, the prompt on stdin, and every
//! line through the adapter's parser.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use argonauts_core::{AgentEvent, CommandSpec};

pub fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

pub fn write_files(spec: &CommandSpec) {
    for file in &spec.files {
        if let Some(parent) = file.path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&file.path, &file.contents).unwrap();
    }
}

pub struct Run {
    pub events: Vec<AgentEvent>,
    pub exit_code: Option<i32>,
}

pub fn run(adapter: &dyn argonauts_core::Adapter, spec: &CommandSpec) -> Run {
    write_files(spec);
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    for (name, value) in &spec.env_set {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    if let Some(prompt) = &spec.stdin {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(prompt.as_bytes()).unwrap();
    }
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let stderr = BufReader::new(child.stderr.take().unwrap());
    let mut parser = adapter.parser();
    let mut events = Vec::new();
    for line in stdout.lines() {
        events.extend(parser.parse_stdout_line(&line.unwrap()));
    }
    for line in stderr.lines() {
        events.extend(parser.parse_stderr_line(&line.unwrap()));
    }
    let exit_code = child.wait().unwrap().code();
    events.extend(parser.finish(exit_code));
    Run { events, exit_code }
}

pub fn scratch(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
