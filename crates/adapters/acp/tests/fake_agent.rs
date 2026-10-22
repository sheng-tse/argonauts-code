//! The ACP driver against the testkit's fake agent, over real pipes, launched from the adapter's
//! own command: the conformance rows for the report routes, the permission policy, the model
//! read-back, steering, stop after the tool, resume after a restart, config isolation, and
//! failures. No model and no network.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use argonauts_adapter_acp::acp_adapter;
use argonauts_core::adapter::Transport;
use argonauts_core::driver::{
    DriverControl, DriverEnd, DriverIo, DriverStart, DriverStop, McpServerSpec, PermissionScope,
};
use argonauts_core::events::session::Delivery;
use argonauts_core::registry::CliManifest;
use argonauts_core::registry::acp::{AcpProfile, ModelRoute, ReportRoute, ResumeRoute, SteerRoute};
use argonauts_core::registry::cli::{ErrorPattern, HomeKeep, HomeTemplate};
use argonauts_core::session_runner::Access;
use argonauts_core::{AgentEvent, ErrorCategory, SessionRequest, SessionRole};
use argonauts_testkit::acp::WRONG_PICK_EXIT;
use argonauts_testkit::scenario::LogLine;
use argonauts_testkit::{FakeClis, TempRepo};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

const PATIENCE: Duration = Duration::from_secs(20);

const SCENARIO: &str = r#"
[[acp]]
argv0 = "hermes"
args_contain = ["report-tool"]
[[acp.turn]]
[[acp.turn.step]]
update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = "Done." } }
[[acp.turn.step]]
write = { path = "report.json", content = '{"status": "done"}' }

[[acp]]
argv0 = "hermes"
args_contain = ["report-final"]
[[acp.turn]]
[[acp.turn.step]]
update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = "All done." } }
[[acp.turn]]
[[acp.turn.step]]
update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = '{"status": "blocked"}' } }

[[acp]]
argv0 = "hermes"
args_contain = ["permissions"]
[[acp.turn]]
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "p1", title = "Edit src/lib.rs", kind = "edit", locations = [{ path = "src/lib.rs" }] }, expect = "allow-once" }
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "p2", title = "Edit .argonauts/runs.json", kind = "edit", locations = [{ path = ".argonauts/runs.json" }] }, expect = "reject-once" }
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "p3", title = "Edit /etc/hosts", kind = "edit", locations = [{ path = "/etc/hosts" }] }, expect = "reject-once" }
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "p4", title = "curl", kind = "execute", rawInput = { command = "curl https://example.com" } }, expect = "reject-once" }
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "p5", title = "Switch mode", kind = "switch_mode", rawInput = { modeId = "bypassPermissions" } }, expect = "reject-once" }
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "p6", title = "Something" }, expect = "reject-once" }
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "p7", title = "cargo test", kind = "execute", rawInput = { command = "cargo test" } }, expect = "allow-once" }

[[acp]]
argv0 = "hermes"
args_contain = ["model"]
[acp.reply."session/new".result]
configOptions = [{ id = "model", name = "Model", category = "model", type = "select", currentValue = "gpt-oss-20b", options = [] }]
[acp.reply."session/set_config_option".result]
configOptions = [{ id = "model", name = "Model", category = "model", type = "select", currentValue = "gpt-oss-20b", options = [] }]

[[acp]]
argv0 = "hermes"
args_contain = ["steer"]
[[acp.turn]]
[[acp.turn.step]]
update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = "Working." } }
[[acp.turn.step]]
sleep_ms = 500
[[acp.turn]]
[[acp.turn.step]]
update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = " Steered." } }

[[acp]]
argv0 = "hermes"
args_contain = ["stop-after-tool"]
[[acp.turn]]
[[acp.turn.step]]
update = { sessionUpdate = "tool_call", toolCallId = "t1", title = "cargo build", kind = "execute", status = "in_progress" }
[[acp.turn.step]]
sleep_ms = 300
[[acp.turn.step]]
update = { sessionUpdate = "tool_call_update", toolCallId = "t1", status = "completed" }
[[acp.turn.step]]
hang = true

[[acp]]
argv0 = "hermes"
args_contain = ["resume"]
session_id = "hermes-7"
[acp.reply.initialize.result]
protocolVersion = 1
agentCapabilities = { sessionCapabilities = { resume = {} } }

[[acp]]
argv0 = "hermes"
args_contain = ["isolation"]

[[acp]]
argv0 = "hermes"
args_contain = ["auth"]
[acp.reply."session/new"]
error = { code = -32000, message = "Authentication required" }

[[acp]]
argv0 = "hermes"
args_contain = ["crash"]
[[acp.turn]]
[[acp.turn.step]]
stderr = "hermes: credentials or agent init failed"
[[acp.turn.step]]
exit = 1

[[acp]]
argv0 = "hermes"
args_contain = ["disconnect"]
[[acp.turn]]
[[acp.turn.step]]
exit = 3
"#;

struct Bench {
    repo: TempRepo,
    clis: FakeClis,
}

struct Live {
    events: mpsc::UnboundedReceiver<AgentEvent>,
    control: mpsc::Sender<DriverControl>,
    task: JoinHandle<DriverEnd>,
    child: tokio::process::Child,
}

struct Ended {
    end: DriverEnd,
    events: Vec<AgentEvent>,
    exit: Option<i32>,
}

impl Bench {
    fn manifest(&self, row: &str, acp: AcpProfile, home: Option<HomeTemplate>) -> CliManifest {
        CliManifest {
            id: "hermes".into(),
            display_name: "Hermes Agent".into(),
            binary: self.clis.bin_dir().join("hermes").display().to_string(),
            read_only_args: vec!["acp".into(), row.into()],
            build_args: vec!["acp".into(), row.into()],
            provider: "from-model".into(),
            transport: Transport::Acp,
            env_set: BTreeMap::from([("HERMES_ACP_SKIP_CONFIGURED_MCP".into(), "1".into())]),
            errors: vec![ErrorPattern {
                pattern: "credentials or agent init failed".into(),
                category: ErrorCategory::Auth,
                retryable: false,
            }],
            home,
            acp: Some(acp),
            ..CliManifest::default()
        }
    }

    fn start(&self) -> DriverStart {
        let root = self.repo.root().to_owned();
        DriverStart {
            cwd: root.clone(),
            prompt: "Fix the bug.".into(),
            model: None,
            effort: None,
            access: Access::Build,
            resume: None,
            mcp_servers: vec![McpServerSpec {
                name: "argonauts".into(),
                command: "/usr/local/bin/argonauts".into(),
                args: vec!["mcp".into(), "session".into()],
            }],
            scope: PermissionScope {
                roots: vec![root.clone()],
                writable: vec![root],
                read_only: Vec::new(),
                deny: Vec::new(),
                forbidden: vec![".argonauts/**".into()],
                network: false,
            },
            json_schema: None,
            report_file: None,
            asking: None,
        }
    }

    /// Launches the fake from the adapter's command and hands its pipes to the driver, as the
    /// CLI runner does.
    fn launch(&self, row: &str, manifest: CliManifest, start: DriverStart) -> Live {
        let adapter = acp_adapter(manifest);
        let request = SessionRequest {
            workdir: self.repo.root().to_owned(),
            role: SessionRole::Build,
            scratch_dir: Some(self.scratch(row)),
            ..SessionRequest::default()
        };
        let spec = adapter.command(&request).unwrap();
        for file in &spec.files {
            std::fs::create_dir_all(file.path.parent().unwrap()).unwrap();
            std::fs::write(&file.path, &file.contents).unwrap();
        }
        let mut child = tokio::process::Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(self.clis.envs())
            .envs(spec.env_set.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (stderr_sender, stderr_lines) = mpsc::channel(64);
        let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
        tokio::spawn(async move {
            while let Ok(Some(line)) = stderr.next_line().await {
                if stderr_sender.send(line).await.is_err() {
                    break;
                }
            }
        });
        let (events_sender, events) = mpsc::unbounded_channel();
        let (frames, _) = mpsc::unbounded_channel();
        let (control, control_receiver) = mpsc::channel(8);
        let io = DriverIo {
            stdin: Box::new(child.stdin.take().unwrap()),
            stdout: Box::new(child.stdout.take().unwrap()),
            stderr_lines,
            events: events_sender,
            control: control_receiver,
            frames,
        };
        let task = tokio::spawn(adapter.session_driver().unwrap().run(io, start));
        Live {
            events,
            control,
            task,
            child,
        }
    }

    async fn run(&self, row: &str, acp: AcpProfile, start: DriverStart) -> Ended {
        self.launch(row, self.manifest(row, acp, None), start)
            .finish()
            .await
    }

    fn scratch(&self, row: &str) -> PathBuf {
        self.repo.scratch().join("sessions").join(row)
    }

    fn client_frames(&self, method: &str) -> Vec<Value> {
        self.clis
            .log()
            .unwrap()
            .into_iter()
            .filter_map(|line| match line {
                LogLine::AcpFrame {
                    from_agent: false,
                    frame,
                    ..
                } if frame["method"] == method => Some(frame),
                _ => None,
            })
            .collect()
    }
}

impl Live {
    async fn wait_for(&mut self, wanted: impl Fn(&AgentEvent) -> bool) {
        loop {
            let event = tokio::time::timeout(PATIENCE, self.events.recv())
                .await
                .expect("the driver emitted nothing")
                .expect("the driver ended");
            if wanted(&event) {
                return;
            }
        }
    }

    async fn deliver(&self, text: &str, now: bool) -> oneshot::Receiver<Delivery> {
        let (done, delivered) = oneshot::channel();
        self.control
            .send(DriverControl::Deliver {
                text: text.into(),
                now,
                done,
            })
            .await
            .unwrap();
        delivered
    }

    async fn finish(self) -> Ended {
        let Live {
            mut events,
            control,
            task,
            mut child,
        } = self;
        let end = tokio::time::timeout(PATIENCE, task)
            .await
            .expect("the driver didn't end")
            .unwrap();
        drop(control);
        let status = tokio::time::timeout(PATIENCE, child.wait())
            .await
            .expect("the fake agent didn't exit once its stdin closed")
            .unwrap();
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(event);
        }
        Ended {
            end,
            events: seen,
            exit: status.code(),
        }
    }
}

fn permission_errors(events: &[AgentEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Error {
                category: ErrorCategory::Permission,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

fn structured(events: &[AgentEvent]) -> Option<&Value> {
    events.iter().find_map(|event| match event {
        AgentEvent::Result { structured, .. } => structured.as_ref(),
        _ => None,
    })
}

#[tokio::test]
async fn acp_driver_against_fake_agent() {
    let argonauts_home = tempfile::tempdir().unwrap();
    let template = argonauts_home.path().join("clis/hermes");
    std::fs::create_dir_all(&template).unwrap();
    std::fs::write(
        template.join("config.yaml"),
        "memory:\n  memory_enabled: false\n",
    )
    .unwrap();
    // SAFETY: this test binary holds this one test, so no other thread reads the environment.
    unsafe { std::env::set_var("ARGONAUTS_HOME", argonauts_home.path()) };

    let repo = TempRepo::new().unwrap();
    let scenario = repo.scratch().join("acp-scenario.toml");
    std::fs::write(&scenario, SCENARIO).unwrap();
    let clis = FakeClis::install(repo.scratch(), &scenario).unwrap();
    let bench = Bench { repo, clis };
    let schema = json!({ "type": "object", "required": ["status"] });

    // The report through the report tool, which writes the report file.
    let mut start = bench.start();
    start.json_schema = Some(schema.clone());
    start.report_file = Some(bench.repo.root().join("report.json"));
    let ended = bench.run("report-tool", AcpProfile::default(), start).await;
    assert_eq!(ended.end.stop, DriverStop::EndTurn);
    assert_eq!(
        structured(&ended.events),
        Some(&json!({ "status": "done" }))
    );
    assert_eq!(ended.exit, Some(0));

    // The report through a final prompt that asks for the JSON object alone.
    let mut start = bench.start();
    start.json_schema = Some(schema.clone());
    let final_prompt = AcpProfile {
        report: ReportRoute::FinalPrompt,
        ..AcpProfile::default()
    };
    let ended = bench.run("report-final", final_prompt, start).await;
    assert_eq!(ended.end.stop, DriverStop::EndTurn);
    assert_eq!(
        structured(&ended.events),
        Some(&json!({ "status": "blocked" }))
    );
    assert_eq!(bench.client_frames("session/prompt").len(), 3);

    // The permission policy: allow once inside the scope, reject once everything else. The
    // fake checks each pick and exits non-zero on a wrong one.
    let ended = bench
        .run("permissions", AcpProfile::default(), bench.start())
        .await;
    assert_eq!(ended.end.stop, DriverStop::EndTurn);
    assert_ne!(ended.exit, Some(WRONG_PICK_EXIT), "{:?}", ended.events);
    assert_eq!(ended.exit, Some(0));
    let rejected = permission_errors(&ended.events);
    assert_eq!(rejected.len(), 5, "{rejected:?}");
    assert!(rejected[0].contains(".argonauts/runs.json"), "{rejected:?}");
    assert!(rejected[2].contains("no network"), "{rejected:?}");

    // The model read-back: the agent takes the model but reports another one.
    let mut start = bench.start();
    start.model = Some("qwen3-coder".into());
    let config = AcpProfile {
        model: ModelRoute::ConfigOption,
        ..AcpProfile::default()
    };
    let prompts_before = bench.client_frames("session/prompt").len();
    let ended = bench.run("model", config, start).await;
    assert_eq!(
        ended.end.stop,
        DriverStop::Failed {
            category: ErrorCategory::InvalidRequest,
            message: "asked for qwen3-coder, the agent reports gpt-oss-20b".into(),
        }
    );
    assert_eq!(
        bench.client_frames("session/prompt").len(),
        prompts_before,
        "no prompt went out"
    );

    // Steering with `input`: a prompt during the turn, which the agent takes after it.
    let steer = AcpProfile {
        steer: SteerRoute::Input,
        ..AcpProfile::default()
    };
    let mut live = bench.launch("steer", bench.manifest("steer", steer, None), bench.start());
    live.wait_for(
        |event| matches!(event, AgentEvent::TextDelta { text, .. } if text == "Working."),
    )
    .await;
    let delivered = live.deliver("Also update the docs.", true).await;
    let ended = live.finish().await;
    assert_eq!(ended.end.stop, DriverStop::EndTurn);
    assert_eq!(delivered.await.unwrap(), Delivery::Steer);
    assert!(
        ended.events.contains(&AgentEvent::Message {
            text: " Steered.".into(),
            parent: None,
        }) || ended.events.contains(&AgentEvent::Message {
            text: "Working. Steered.".into(),
            parent: None,
        })
    );

    // Stop after the tool: the cancel waits for the running tool call to finish.
    let mut live = bench.launch(
        "stop-after-tool",
        bench.manifest("stop-after-tool", AcpProfile::default(), None),
        bench.start(),
    );
    live.wait_for(|event| matches!(event, AgentEvent::ToolStarted { id, .. } if id == "t1"))
        .await;
    live.control
        .send(DriverControl::StopAfterTool)
        .await
        .unwrap();
    let ended = live.finish().await;
    assert_eq!(ended.end.stop, DriverStop::Cancelled);
    assert!(
        matches!(ended.events.first(), Some(AgentEvent::ToolFinished { id, ok: true, .. }) if id == "t1")
    );
    assert_eq!(ended.exit, Some(0));

    // Resume after a restart: a new process picks up the CLI session with `session/resume`,
    // with the working folder and MCP servers sent again.
    let resume = AcpProfile {
        resume: ResumeRoute::Resume,
        ..AcpProfile::default()
    };
    let first = bench.run("resume", resume.clone(), bench.start()).await;
    assert_eq!(first.end.cli_session.as_deref(), Some("hermes-7"));
    let mut start = bench.start();
    start.resume = first.end.cli_session.clone();
    let second = bench.run("resume", resume, start).await;
    assert_eq!(second.end.stop, DriverStop::EndTurn);
    assert_eq!(second.end.cli_session.as_deref(), Some("hermes-7"));
    let resumed = bench.client_frames("session/resume");
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0]["params"]["sessionId"], "hermes-7");
    assert_eq!(
        resumed[0]["params"]["cwd"],
        bench.repo.root().display().to_string()
    );
    assert_eq!(resumed[0]["params"]["mcpServers"][0]["name"], "argonauts");

    // Config isolation: the agent runs with a home of its own, made from the template.
    let home = HomeTemplate {
        env: "HERMES_HOME".into(),
        template: "hermes".into(),
        keep: HomeKeep::Task,
    };
    let manifest = bench.manifest("isolation", AcpProfile::default(), Some(home));
    let ended = bench
        .launch("isolation", manifest, bench.start())
        .finish()
        .await;
    assert_eq!(ended.end.stop, DriverStop::EndTurn);
    let home_config = bench.scratch("isolation").join("home/config.yaml");
    let home_config = std::fs::read_to_string(home_config).unwrap();
    assert!(
        home_config.starts_with("memory:\n  memory_enabled: false\n"),
        "{home_config}"
    );
    // Hermes loads the worktree's reviewed skills.
    let skills = bench.repo.root().join(".agents/skills");
    assert!(
        home_config.contains(&format!("- {}\n", skills.display())),
        "{home_config}"
    );
    let invocation = bench
        .clis
        .invocations()
        .unwrap()
        .into_iter()
        .find(|invocation| invocation.args.iter().any(|arg| arg == "isolation"))
        .unwrap();
    assert!(invocation.env.iter().any(|name| name == "HERMES_HOME"));
    assert!(
        invocation
            .env
            .iter()
            .any(|name| name == "HERMES_ACP_SKIP_CONFIGURED_MCP")
    );

    // Failures: authentication, a crash named by its stderr, and a bare disconnect.
    let ended = bench
        .run("auth", AcpProfile::default(), bench.start())
        .await;
    assert_eq!(
        ended.end.stop,
        DriverStop::Failed {
            category: ErrorCategory::Auth,
            message: "Authentication required".into(),
        }
    );
    let ended = bench
        .run("crash", AcpProfile::default(), bench.start())
        .await;
    assert_eq!(
        ended.end.stop,
        DriverStop::Failed {
            category: ErrorCategory::Auth,
            message: "hermes: credentials or agent init failed".into(),
        }
    );
    assert_eq!(ended.exit, Some(1));
    let ended = bench
        .run("disconnect", AcpProfile::default(), bench.start())
        .await;
    assert_eq!(ended.end.stop, DriverStop::Disconnected);
    assert_eq!(ended.exit, Some(3));
}
