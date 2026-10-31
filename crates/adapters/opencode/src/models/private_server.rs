//! OpenCode asked something outside a session, such as its model list or its config, on a
//! server of its own, inside the sandbox the caller gives, since OpenCode started in a repo
//! loads the plugins that repo sets up. No call here runs a model.
//!
//! OpenCode 2 answers every command through a background service it keeps for the whole
//! computer, unless told otherwise. That service runs outside Argonauts' sandbox and never sees
//! the config Argonauts sends, so Argonauts never asks it. `--standalone` starts a private
//! server for one command instead: the command's own child, which ends with it. OpenCode 1.18
//! has no service and no such flag, so nothing is added there.
//!
//! A fresh server's first answers can be empty, or list some providers before the rest have
//! loaded: OpenCode 2.0.32 listed its 12 Zen models at once and Ollama's 7 a second later. Each
//! `--standalone` command gets a fresh server that answers once, so a list is asked instead of
//! one server kept for the wait, each second until two answers in a row agree, for up to 15 s.
//! An answer still empty after that keeps the last list, and so does a failed one.

use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::agents::confine::Confine;
use argonauts_core::{CommandSpec, env_remove_except, extract_version};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdout};
use tokio::time::Instant;

use crate::session::command::AUTO_UPDATE_ENV;

/// The flag that puts an OpenCode 2 command on a private server.
pub const STANDALONE_FLAG: &str = "--standalone";

/// The variable a kept server's password goes in, for the server and for each call to it.
const PASSWORD_ENV: &str = "OPENCODE_PASSWORD";

/// How long a kept server is given to end once its input closes, before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// The flags that put one OpenCode command on a private server, by the installed version:
/// `--standalone` from OpenCode 2 on, and none on 1.x, which has no shared service. A version
/// that can't be read gets the flag, since today's OpenCode is 2.x and nothing Argonauts starts
/// may land on the shared service.
pub fn private_server_flags(version: Option<&str>) -> &'static [&'static str] {
    match version.and_then(major_version) {
        Some(major) if major < 2 => &[],
        _ => &[STANDALONE_FLAG],
    }
}

fn major_version(version: &str) -> Option<u64> {
    extract_version(version)?.split('.').next()?.parse().ok()
}

/// One `opencode api` request: an operation from OpenCode's API, such as `model.list`, or a
/// method and a path, such as `GET /api/config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiCall {
    target: Vec<String>,
    params: Vec<(String, String)>,
    body: Option<String>,
}

impl ApiCall {
    /// An operation by its id, as `opencode api model.list`.
    pub fn operation(id: impl Into<String>) -> Self {
        Self {
            target: vec![id.into()],
            params: Vec::new(),
            body: None,
        }
    }

    /// A method and a path, as `opencode api GET /api/config`.
    pub fn request(method: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            target: vec![method.into(), path.into()],
            params: Vec::new(),
            body: None,
        }
    }

    /// A path or query parameter, such as `location[directory]` and the repo to answer for.
    pub fn param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.push((key.into(), value.into()));
        self
    }

    /// A request body, as JSON text.
    pub fn body(mut self, json: impl Into<String>) -> Self {
        self.body = Some(json.into());
        self
    }
}

/// How OpenCode is asked: its program, the folder its server answers for, the flags that give
/// each command a private server, and the sandbox each command runs in.
#[derive(Debug, Clone)]
pub struct PrivateServer {
    program: PathBuf,
    workdir: PathBuf,
    flags: &'static [&'static str],
    env_set: Vec<(String, String)>,
    /// Without one, nothing is run.
    confine: Option<Arc<dyn Confine>>,
}

impl PrivateServer {
    /// `version` is what `opencode --version` printed, or `None` when it isn't known.
    pub fn new(
        program: impl Into<PathBuf>,
        version: Option<&str>,
        workdir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            program: program.into(),
            workdir: workdir.into(),
            flags: private_server_flags(version),
            env_set: Vec::new(),
            confine: None,
        }
    }

    /// The sandbox each command [`PrivateServer::run`] starts goes in.
    pub fn confined(mut self, confine: Arc<dyn Confine>) -> Self {
        self.confine = Some(confine);
        self
    }

    /// A variable the server starts with, such as the config Argonauts sends OpenCode.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_set.push((key.into(), value.into()));
        self
    }

    /// The flags each command gets, from [`private_server_flags`].
    pub fn flags(&self) -> &'static [&'static str] {
        self.flags
    }

    /// `opencode api [--standalone] <call>`, for a caller that runs it its own way. The server
    /// goes without the same variables a session goes without, and doesn't update OpenCode
    /// while it answers.
    pub fn api_command(&self, call: &ApiCall) -> CommandSpec {
        self.api_on(call, self.flags.iter().map(|flag| (*flag).to_owned()))
    }

    /// `opencode api <where> <call>`, where `place` says which server answers.
    fn api_on(&self, call: &ApiCall, place: impl IntoIterator<Item = String>) -> CommandSpec {
        let mut cmd = CommandSpec::new(self.program.display().to_string(), &self.workdir)
            .arg("api")
            .args(place)
            .args(call.target.iter().cloned());
        for (key, value) in &call.params {
            cmd = cmd.arg("--param").arg(format!("{key}={value}"));
        }
        if let Some(body) = &call.body {
            cmd = cmd.arg("--data").arg(body);
        }
        self.served(cmd)
    }

    /// `opencode <command> [--standalone]`, such as `opencode models` where `opencode api`
    /// is missing, on a private server as [`PrivateServer::api_command`]'s calls are.
    pub fn command(&self, command: &[&str]) -> CommandSpec {
        let cmd = CommandSpec::new(self.program.display().to_string(), &self.workdir)
            .args(command.iter().copied())
            .args(self.flags.iter().copied());
        self.served(cmd)
    }

    fn served(&self, mut cmd: CommandSpec) -> CommandSpec {
        cmd.env_set
            .push((AUTO_UPDATE_ENV.to_owned(), "1".to_owned()));
        cmd.env_set.extend(self.env_set.iter().cloned());
        cmd.env_remove = env_remove_except(&[]);
        cmd
    }

    /// Runs one call and returns what it printed, or why it failed, as [`PrivateServer::run`]
    /// says.
    pub async fn ask(&self, call: &ApiCall, timeout: Duration) -> Result<String, String> {
        self.run(self.api_command(call), timeout).await
    }

    /// Runs a command [`PrivateServer::command`] made inside the sandbox, and returns what it
    /// printed, or why it failed: there's no sandbox to run it in, it couldn't start, took
    /// longer than `timeout`, exited with an error, or printed text that isn't UTF-8.
    pub async fn run(&self, spec: CommandSpec, timeout: Duration) -> Result<String, String> {
        let shown = format!("{} {}", spec.program, spec.args.join(" "));
        let mut command = self.confined_process(spec, &shown).await?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The private server reads its client's pipe, so it ends when the client is killed.
            .kill_on_drop(true);
        let output = tokio::time::timeout(timeout, command.output())
            .await
            .map_err(|_| format!("`{shown}` took longer than {}s", timeout.as_secs()))?
            .map_err(|e| format!("couldn't run `{shown}`: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let why = stderr.lines().rev().find(|line| !line.trim().is_empty());
            return Err(format!(
                "`{shown}` failed ({}): {}",
                output.status,
                why.unwrap_or("").trim()
            ));
        }
        String::from_utf8(output.stdout)
            .map_err(|_| format!("`{shown}` printed text that isn't UTF-8"))
    }

    /// `spec` inside the sandbox, as a process to start, or why it can't run there.
    async fn confined_process(
        &self,
        spec: CommandSpec,
        shown: &str,
    ) -> Result<tokio::process::Command, String> {
        let Some(confine) = &self.confine else {
            return Err(format!(
                "`{shown}` runs only inside a sandbox, and none was given"
            ));
        };
        let spec = confine
            .confine(crate::ID, spec)
            .await
            .map_err(|why| format!("`{shown}` didn't run: {why}"))?;
        let mut command = tokio::process::Command::new(&spec.program);
        command.args(&spec.args).current_dir(&spec.cwd);
        for name in &spec.env_remove {
            command.env_remove(name);
        }
        command.envs(spec.env_set.iter().cloned());
        Ok(command)
    }

    /// Asks `call` for a list on one server kept for the wait, until it settles, as
    /// [`ask_until_settled`] does. A blank answer is an empty list; any other goes to `parse`.
    pub async fn list<T: PartialEq>(
        &self,
        call: &ApiCall,
        parse: impl Fn(&str) -> Result<Vec<T>, String>,
        wait: ColdStart,
        last: Vec<T>,
    ) -> Listing<T> {
        let kept = match self.keep(wait.up_to).await {
            Ok(kept) => kept,
            Err(why) => return Listing::Failed { kept: last, why },
        };
        let listing = kept.list(call, parse, wait, last).await;
        kept.stop().await;
        listing
    }

    /// One server for a run of calls: `opencode serve --stdio --port 0` inside the sandbox,
    /// which says where it answers on its first line and ends once its input closes, with a
    /// password of its own. On OpenCode 1.x, which has no shared service, each call answers
    /// alone instead.
    pub async fn keep(&self, timeout: Duration) -> Result<KeptServer<'_>, String> {
        if self.flags.is_empty() {
            return Ok(KeptServer {
                server: self,
                running: None,
            });
        }
        let password = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut spec = self.served(
            CommandSpec::new(self.program.display().to_string(), &self.workdir)
                .args(["serve", "--stdio", "--port", "0"]),
        );
        spec.env_set
            .push((PASSWORD_ENV.to_owned(), password.clone()));
        let shown = format!("{} {}", spec.program, spec.args.join(" "));
        let mut command = self.confined_process(spec, &shown).await?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| format!("couldn't run `{shown}`: {e}"))?;
        let Some(stdout) = child.stdout.take() else {
            return Err(format!("`{shown}` gave no output to read"));
        };
        let url = match tokio::time::timeout(timeout, ready_at(stdout)).await {
            Ok(Some(url)) => url,
            Ok(None) => return Err(format!("`{shown}` ended before it said where it answers")),
            Err(_) => {
                return Err(format!(
                    "`{shown}` didn't say where it answers within {}s",
                    timeout.as_secs()
                ));
            }
        };
        Ok(KeptServer {
            server: self,
            running: Some(Running {
                child,
                url,
                password,
            }),
        })
    }
}

/// What a kept server prints once it's ready.
#[derive(Deserialize)]
struct Ready {
    url: String,
}

/// The address a kept server says it answers at, read from its output, which is then read to
/// its end so the server never blocks on a full pipe. `None` when it ends without one.
async fn ready_at(stdout: ChildStdout) -> Option<String> {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Ok(ready) = serde_json::from_str::<Ready>(line.trim()) {
            tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
            return Some(ready.url);
        }
    }
    None
}

/// A private server kept for a run of calls, from [`PrivateServer::keep`]. Each call reaches it
/// with `--server` and its password. Dropping it kills the server; [`KeptServer::stop`] lets
/// it end on its own first.
#[derive(Debug)]
pub struct KeptServer<'a> {
    server: &'a PrivateServer,
    /// `None` on OpenCode 1.x, where each call answers alone.
    running: Option<Running>,
}

#[derive(Debug)]
struct Running {
    child: Child,
    url: String,
    password: String,
}

impl KeptServer<'_> {
    /// Runs one `opencode api` call on this server, as [`PrivateServer::ask`] does.
    pub async fn ask(&self, call: &ApiCall, timeout: Duration) -> Result<String, String> {
        let Some(running) = &self.running else {
            return self.server.ask(call, timeout).await;
        };
        let mut spec = self
            .server
            .api_on(call, ["--server".to_owned(), running.url.clone()]);
        spec.env_set
            .push((PASSWORD_ENV.to_owned(), running.password.clone()));
        self.server.run(spec, timeout).await
    }

    /// `opencode <command>` on this server, such as `opencode models`.
    pub async fn command(&self, command: &[&str], timeout: Duration) -> Result<String, String> {
        let Some(running) = &self.running else {
            return self.server.run(self.server.command(command), timeout).await;
        };
        let mut spec = self.server.served(
            CommandSpec::new(
                self.server.program.display().to_string(),
                &self.server.workdir,
            )
            .args(command.iter().copied())
            .args(["--server", running.url.as_str()]),
        );
        spec.env_set
            .push((PASSWORD_ENV.to_owned(), running.password.clone()));
        self.server.run(spec, timeout).await
    }

    /// Asks `call` for a list until this server's answer settles, as [`ask_until_settled`]
    /// does. A blank answer is an empty list; any other goes to `parse`.
    pub async fn list<T: PartialEq>(
        &self,
        call: &ApiCall,
        parse: impl Fn(&str) -> Result<Vec<T>, String>,
        wait: ColdStart,
        last: Vec<T>,
    ) -> Listing<T> {
        let parse = &parse;
        let ask = move || async move {
            let printed = self.ask(call, wait.up_to).await?;
            if printed.trim().is_empty() {
                return Ok(Vec::new());
            }
            parse(&printed)
        };
        ask_until_settled(wait, ask, last).await
    }

    /// Closes the server's input so it ends on its own, and kills it if it hasn't after a few
    /// seconds.
    pub async fn stop(self) {
        let Some(mut running) = self.running else {
            return;
        };
        drop(running.child.stdin.take());
        if tokio::time::timeout(STOP_GRACE, running.child.wait())
            .await
            .is_err()
        {
            let _ = running.child.kill().await;
        }
    }
}

/// How long a fresh server is given to list something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColdStart {
    /// From the start of one ask to the start of the next.
    pub every: Duration,
    /// From the first ask: no ask starts after it.
    pub up_to: Duration,
}

impl ColdStart {
    /// Each second for up to 15 s. A fresh OpenCode server listed nothing for about 5 s.
    pub const DEFAULT: Self = Self {
        every: Duration::from_secs(1),
        up_to: Duration::from_secs(15),
    };
}

impl Default for ColdStart {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What asking for a list ended with. Each outcome carries the list to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing<T> {
    /// The server listed something.
    Listed(Vec<T>),
    /// Every answer was empty for the whole wait, so the last list stands.
    StillEmpty { kept: Vec<T>, asks: u32 },
    /// An ask failed, as on an OpenCode with no `opencode api`, so the last list stands.
    Failed { kept: Vec<T>, why: String },
}

impl<T> Listing<T> {
    /// The list to show: the new one, or the last one kept.
    pub fn into_list(self) -> Vec<T> {
        match self {
            Self::Listed(list) => list,
            Self::StillEmpty { kept, .. } | Self::Failed { kept, .. } => kept,
        }
    }

    /// Whether the server answered with a list of its own.
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Listed(_))
    }
}

/// Asks until two answers in a row list the same thing, `wait.every` apart for up to
/// `wait.up_to`, since a fresh server's first answers can be empty or list only the providers
/// that loaded first. When the time is up, the last list it gave counts. A failed ask stops at
/// once, since asking again doesn't mend a missing command or an answer that can't be read;
/// then, as when every answer was empty, `last` stands.
pub async fn ask_until_settled<T, F, Fut>(wait: ColdStart, mut ask: F, last: Vec<T>) -> Listing<T>
where
    T: PartialEq,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Vec<T>, String>>,
{
    let first = Instant::now();
    let mut asks = 0;
    let mut latest: Vec<T> = Vec::new();
    loop {
        let asked = Instant::now();
        asks += 1;
        match ask().await {
            Ok(list) if !list.is_empty() && list == latest => return Listing::Listed(list),
            Ok(list) if !list.is_empty() => latest = list,
            Ok(_) => {}
            Err(why) => return Listing::Failed { kept: last, why },
        }
        let next = asked + wait.every;
        if next.duration_since(first) > wait.up_to {
            return if latest.is_empty() {
                Listing::StillEmpty { kept: last, asks }
            } else {
                Listing::Listed(latest)
            };
        }
        tokio::time::sleep_until(next).await;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;

    use argonauts_core::agents::confine::Unconfined;
    use argonauts_core::base::future::BoxFuture;

    use super::*;
    use crate::session::test_script::write_script;

    fn unconfined() -> Arc<dyn Confine> {
        Arc::new(Unconfined)
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

    /// Gives up after 20 ms, for the tests where nothing is ever listed.
    const QUICK: ColdStart = ColdStart {
        every: Duration::from_millis(5),
        up_to: Duration::from_millis(20),
    };

    /// Waits long enough for a few asks even on a busy machine.
    const PATIENT: ColdStart = ColdStart {
        every: Duration::from_millis(5),
        up_to: Duration::from_secs(5),
    };

    fn lines(printed: &str) -> Result<Vec<String>, String> {
        Ok(printed.lines().map(str::to_owned).collect())
    }

    /// Where the fake servers below say they answer.
    const FAKE_URL: &str = "http://127.0.0.1:9";

    /// A fake `opencode` whose kept server answers empty `empty_answers` times, then lists two
    /// models. It writes the arguments and update setting of each call to `calls.txt`, whether
    /// each call to the server came with the server's own password, and when the server ended.
    fn fake_opencode(dir: &Path, empty_answers: u32) -> PathBuf {
        let program = dir.join("opencode");
        let script = format!(
            "#!/bin/sh\n\
             echo \"$* update=$OPENCODE_DISABLE_AUTOUPDATE\" >> calls.txt\n\
             case \"$*\" in\n\
             'serve --stdio --port 0')\n\
               printf '%s' \"$OPENCODE_PASSWORD\" > password\n\
               echo 'starting'; echo '{{\"url\":\"{FAKE_URL}\"}}'\n\
               cat > /dev/null; echo 'ended' >> calls.txt ;;\n\
             'api --server {FAKE_URL} '*)\n\
               [ \"$OPENCODE_PASSWORD\" = \"$(cat password)\" ] || {{ echo 'wrong password' >&2; exit 1; }}\n\
               n=$(grep -c model.list calls.txt)\n\
               if [ \"$n\" -gt {empty_answers} ]; then printf 'ollama/llama3.2\\nopencode/beam-free\\n'; fi ;;\n\
             *) exit 2 ;;\n\
             esac\n"
        );
        write_script(&program, &script);
        program
    }

    #[test]
    fn opencode_2_gets_a_private_server_and_1_18_none() {
        assert_eq!(private_server_flags(Some("2.0.32")), ["--standalone"]);
        assert_eq!(
            private_server_flags(Some("opencode v2.0.26")),
            ["--standalone"]
        );
        assert!(private_server_flags(Some("1.18.34")).is_empty());
        // Unknown versions never risk the shared service.
        assert_eq!(private_server_flags(None), ["--standalone"]);
        assert_eq!(private_server_flags(Some("dev build")), ["--standalone"]);
    }

    #[test]
    fn a_call_names_its_operation_after_the_flags() {
        let server = PrivateServer::new("opencode", Some("2.0.32"), "/work/repo");
        let list = ApiCall::operation("model.list").param("location[directory]", "/work/repo");
        let cmd = server.api_command(&list);
        assert_eq!(cmd.program, "opencode");
        assert_eq!(cmd.cwd, Path::new("/work/repo"));
        assert_eq!(
            cmd.args,
            [
                "api",
                "--standalone",
                "model.list",
                "--param",
                "location[directory]=/work/repo"
            ]
        );
        assert!(cmd.stdin.is_none());
        assert!(
            cmd.env_set
                .contains(&(AUTO_UPDATE_ENV.to_owned(), "1".to_owned()))
        );
        assert_eq!(cmd.env_remove, env_remove_except(&[]));

        let config = ApiCall::request("GET", "/api/config");
        let old = PrivateServer::new("opencode", Some("1.18.34"), "/work/repo");
        assert_eq!(old.api_command(&config).args, ["api", "GET", "/api/config"]);
        let posted = ApiCall::operation("debug.location.evict").body("{}");
        assert_eq!(
            server.api_command(&posted).args,
            [
                "api",
                "--standalone",
                "debug.location.evict",
                "--data",
                "{}"
            ]
        );
    }

    #[test]
    fn the_server_gets_the_variables_it_is_given() {
        let server =
            PrivateServer::new("opencode", None, "/work/repo").env("OPENCODE_CONFIG_CONTENT", "{}");
        let cmd = server.api_command(&ApiCall::operation("model.list"));
        assert!(
            cmd.env_set
                .contains(&("OPENCODE_CONFIG_CONTENT".to_owned(), "{}".to_owned()))
        );
    }

    /// The answers a fake ask gives, one per ask, the last one again once they run out.
    fn answering(answers: Vec<Vec<&'static str>>) -> (Cell<usize>, Vec<Vec<&'static str>>) {
        (Cell::new(0), answers)
    }

    #[tokio::test]
    async fn an_empty_answer_asks_again_until_two_agree() {
        let (asks, answers) = answering(vec![vec![], vec![], vec!["qwen"]]);
        let listing = ask_until_settled(
            PATIENT,
            || {
                let answer = answers[asks.get().min(answers.len() - 1)].clone();
                asks.set(asks.get() + 1);
                async move { Ok(answer) }
            },
            vec!["kept"],
        )
        .await;
        assert_eq!(listing, Listing::Listed(vec!["qwen"]));
        assert!(listing.is_fresh());
        assert_eq!(asks.get(), 4);
    }

    /// OpenCode 2.0.32 listed its Zen models at once and Ollama's a second later.
    #[tokio::test]
    async fn a_list_that_grows_is_asked_until_it_stops() {
        let (asks, answers) = answering(vec![vec!["zen"], vec!["zen", "ollama"]]);
        let listing = ask_until_settled(
            PATIENT,
            || {
                let answer = answers[asks.get().min(answers.len() - 1)].clone();
                asks.set(asks.get() + 1);
                async move { Ok(answer) }
            },
            Vec::new(),
        )
        .await;
        assert_eq!(listing, Listing::Listed(vec!["zen", "ollama"]));
        assert_eq!(asks.get(), 3);
    }

    #[tokio::test]
    async fn a_list_still_changing_when_time_is_up_counts_as_it_last_stood() {
        let asks = Cell::new(0);
        let listing = ask_until_settled(
            QUICK,
            || {
                asks.set(asks.get() + 1);
                let answer = vec![asks.get()];
                async move { Ok(answer) }
            },
            vec![0],
        )
        .await;
        assert_eq!(listing, Listing::Listed(vec![asks.get()]));
    }

    #[tokio::test]
    async fn answers_still_empty_keep_the_last_list() {
        let asks = Cell::new(0u32);
        let listing = ask_until_settled(
            QUICK,
            || {
                asks.set(asks.get() + 1);
                async { Ok(Vec::<&str>::new()) }
            },
            vec!["kept"],
        )
        .await;
        let Listing::StillEmpty {
            kept,
            asks: counted,
        } = listing
        else {
            panic!("expected the last list kept: {listing:?}");
        };
        assert_eq!(kept, ["kept"]);
        assert_eq!(counted, asks.get());
        // Asked at 0, 5, 10, 15 and 20 ms, or fewer when the clock runs late, never after.
        assert!((2..=5).contains(&counted), "{counted} asks");
    }

    #[tokio::test]
    async fn a_failed_ask_stops_at_once_and_keeps_the_last_list() {
        let asks = Cell::new(0);
        let listing = ask_until_settled(
            ColdStart::DEFAULT,
            || {
                asks.set(asks.get() + 1);
                async { Err::<Vec<&str>, _>("no such command".to_owned()) }
            },
            vec!["kept"],
        )
        .await;
        assert_eq!(asks.get(), 1);
        assert_eq!(listing.into_list(), ["kept"]);
    }

    #[tokio::test]
    async fn one_kept_server_is_asked_through_opencode_until_its_list_settles() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_opencode(dir.path(), 2);
        let server =
            PrivateServer::new(&program, Some("2.0.32"), dir.path()).confined(unconfined());
        let listing = server
            .list(
                &ApiCall::operation("model.list"),
                lines,
                PATIENT,
                Vec::new(),
            )
            .await;
        assert_eq!(
            listing,
            Listing::Listed(vec![
                "ollama/llama3.2".to_owned(),
                "opencode/beam-free".to_owned()
            ])
        );
        let calls = std::fs::read_to_string(dir.path().join("calls.txt")).unwrap();
        let asked = format!("api --server {FAKE_URL} model.list update=1");
        let mut expected = vec!["serve --stdio --port 0 update=1"];
        expected.extend([asked.as_str(); 4]);
        expected.push("ended");
        assert_eq!(calls.lines().collect::<Vec<_>>(), expected);
    }

    #[tokio::test]
    async fn opencode_1_18_answers_each_call_alone() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("opencode");
        write_script(&program, "#!/bin/sh\necho \"$*\" >> calls.txt\necho '[]'\n");
        let server =
            PrivateServer::new(&program, Some("1.18.34"), dir.path()).confined(unconfined());
        let kept = server.keep(Duration::from_secs(5)).await.unwrap();
        kept.ask(&ApiCall::operation("model.list"), Duration::from_secs(5))
            .await
            .unwrap();
        kept.stop().await;
        let calls = std::fs::read_to_string(dir.path().join("calls.txt")).unwrap();
        assert_eq!(calls, "api model.list\n");
    }

    #[tokio::test]
    async fn a_server_that_never_says_where_it_answers_is_given_up() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("opencode");
        write_script(&program, "#!/bin/sh\necho 'no address here'\n");
        let server =
            PrivateServer::new(&program, Some("2.0.32"), dir.path()).confined(unconfined());
        let why = server.keep(Duration::from_secs(5)).await.unwrap_err();
        assert!(
            why.ends_with("ended before it said where it answers"),
            "{why}"
        );
    }

    #[tokio::test]
    async fn a_server_that_never_lists_keeps_the_last_list() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_opencode(dir.path(), u32::MAX);
        let server =
            PrivateServer::new(&program, Some("2.0.32"), dir.path()).confined(unconfined());
        let last = vec!["ollama/qwen3-coder:30b-64k".to_owned()];
        let listing = server
            .list(
                &ApiCall::operation("model.list"),
                lines,
                QUICK,
                last.clone(),
            )
            .await;
        assert!(!listing.is_fresh(), "{listing:?}");
        assert_eq!(listing.into_list(), last);
    }

    #[tokio::test]
    async fn nothing_runs_outside_a_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_opencode(dir.path(), 0);
        let call = ApiCall::operation("model.list");
        let bare = PrivateServer::new(&program, Some("2.0.32"), dir.path());
        let why = bare.ask(&call, Duration::from_secs(5)).await.unwrap_err();
        assert!(
            why.ends_with("runs only inside a sandbox, and none was given"),
            "{why}"
        );
        let why = bare.keep(Duration::from_secs(5)).await.unwrap_err();
        assert!(
            why.ends_with("runs only inside a sandbox, and none was given"),
            "{why}"
        );
        let broken = bare.confined(Arc::new(Broken));
        let why = broken.ask(&call, Duration::from_secs(5)).await.unwrap_err();
        assert!(
            why.ends_with("didn't run: no working sandbox: sandbox-exec is missing"),
            "{why}"
        );
        assert!(!dir.path().join("calls.txt").exists());
    }

    #[tokio::test]
    async fn a_failing_opencode_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("opencode");
        write_script(
            &program,
            "#!/bin/sh\necho 'unknown command api' >&2\nexit 2\n",
        );
        let server =
            PrivateServer::new(&program, Some("1.18.34"), dir.path()).confined(unconfined());
        let why = server
            .ask(&ApiCall::operation("model.list"), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(why.ends_with("unknown command api"), "{why}");
        assert!(why.contains("api model.list"), "{why}");
    }
}
