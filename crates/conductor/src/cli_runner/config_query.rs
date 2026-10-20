//! Asking the CLI what its own config holds before a session's first launch, for an adapter
//! that needs it ([`Adapter::config_query`](argonauts_core::Adapter::config_query)). The query
//! runs in the session's folder inside the session's own sandbox, since the CLI may load the
//! repo's plugins to answer, and its answer stands for every later launch of the session.

use std::time::Duration;

use argonauts_core::{CommandSpec, SessionRequest};

use crate::process::{AgentProcess, ProcessEvent, Stream};

use super::context::{Context, Launch};

/// A fresh OpenCode 2 server answered in 0.3 s; this leaves room for a slow first start.
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);

/// What the CLI said to `query`, run in `request`'s session sandbox. `None` when it couldn't
/// run or didn't answer, and the adapter then reads what it can itself.
pub(super) async fn ask(
    ctx: &Context,
    request: SessionRequest,
    query: CommandSpec,
) -> Option<String> {
    let said = match ctx.contain(&ctx.spec, request, query) {
        Ok(Launch { command, .. }) => run(&command, QUERY_TIMEOUT).await,
        Err(why) => Err(why),
    };
    said.map_err(|why| {
        tracing::debug!(
            "{} didn't say what its config holds: {why}",
            ctx.adapter.display_name()
        );
    })
    .ok()
}

/// What `command` printed, once it exits with success. Dropping the process on a timeout ends
/// its whole group, so nothing it started outlives the query.
async fn run(command: &CommandSpec, timeout: Duration) -> Result<String, String> {
    let mut process = AgentProcess::spawn(command).map_err(|err| err.to_string())?;
    let read = async {
        let mut printed = String::new();
        let mut status = None;
        while let Some(event) = process.next_event().await {
            match event {
                ProcessEvent::Line(line) if line.stream == Stream::Stdout => {
                    printed.push_str(&line.text);
                    printed.push('\n');
                }
                ProcessEvent::Line(_) => {}
                ProcessEvent::Exited(exit) => status = Some(exit),
            }
        }
        (printed, status)
    };
    let (printed, status) = tokio::time::timeout(timeout, read)
        .await
        .map_err(|_| format!("no answer in {} s", timeout.as_secs()))?;
    match status {
        Some(Ok(status)) if status.success() => Ok(printed),
        Some(Ok(status)) => Err(format!("it ended with {status}")),
        Some(Err(err)) => Err(err.to_string()),
        None => Err("it ended without a status".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::cli_runner::testing::{FakeDriver, TestAdapter, TestDriver, spec_in};

    /// The fake CLI, which asks itself for its config with `config` as its one argument.
    fn asking(script: &Path) -> TestAdapter {
        TestAdapter::new("true")
            .with_driver(FakeDriver::default())
            .asking_for_its_config(script)
    }

    fn script(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("fake-config");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The session works in `dir/work`, apart from the scripts, which a session never runs
    /// from its own folder.
    fn context(h: &TestDriver, dir: &Path) -> Arc<Context> {
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let spec = spec_in(&work, &dir.join("s"), "hi");
        Arc::new(Context::new(&h.worker, spec, h.worker.home().unwrap()))
    }

    #[tokio::test]
    async fn the_answer_reaches_every_launch_of_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let query = script(
            dir.path(),
            "echo asked >> asked.txt\necho '[{\"type\":\"document\"}]'",
        );
        let h = TestDriver::new(asking(&query));
        let ctx = context(&h, dir.path());
        let said = Some("[{\"type\":\"document\"}]\n".to_owned());
        assert_eq!(ctx.first_launch().await.unwrap().request.cli_config, said);
        for _ in 0..2 {
            let launched = ctx.launch(&ctx.spec).unwrap();
            assert_eq!(launched.request.cli_config, said);
        }
        let asked = std::fs::read_to_string(dir.path().join("work/asked.txt")).unwrap();
        assert_eq!(asked.lines().count(), 1, "asked once per session");
    }

    #[tokio::test]
    async fn a_query_that_fails_or_hangs_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let failing = script(dir.path(), "echo half; exit 3");
        let h = TestDriver::new(asking(&failing));
        let ctx = context(&h, dir.path());
        assert_eq!(ctx.first_launch().await.unwrap().request.cli_config, None);
        assert_eq!(ctx.launch(&ctx.spec).unwrap().request.cli_config, None);

        let hanging = CommandSpec::new("/bin/sh", dir.path()).args(["-c", "sleep 30"]);
        let err = run(&hanging, Duration::from_millis(200)).await.unwrap_err();
        assert!(err.contains("no answer"), "{err}");
    }

    #[tokio::test]
    async fn an_adapter_that_needs_no_config_is_never_asked() {
        let dir = tempfile::tempdir().unwrap();
        let h = TestDriver::new(TestAdapter::new("true").with_driver(FakeDriver::default()));
        let ctx = context(&h, dir.path());
        let first = ctx.first_launch().await.unwrap();
        assert_eq!(first.request.cli_config, None);
        assert!(ctx.cli_config.get().is_none(), "never asked");
    }
}
