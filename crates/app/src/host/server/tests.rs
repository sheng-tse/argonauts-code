use std::io::{BufRead as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use argonauts_core::api::{CheckpointAnswer, StatusSummary};
use argonauts_core::control::ClientKind;
use argonauts_core::engine::EngineCommand;
use argonauts_core::engine::EventSink as _;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::journal::{Journal, RunLock};
use argonauts_core::layout::run_files;
use argonauts_core::names::{CheckpointId, MessageId};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::{Note, NoteLevel};
use tokio::net::unix::OwnedReadHalf;

use super::*;
use crate::ancestry::tests::{ScriptedProcesses, register_group, test_markers};
use crate::ancestry::{agent_ancestor, agent_ancestor_in};
use crate::host::relay::{Queue, forward};
use crate::host::sink::JournalSink;

pub(crate) const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PEER_SOCKET: &str = "ARGONAUTS_TEST_PEER_SOCKET";
const PEER_TOKEN: &str = "ARGONAUTS_TEST_PEER_TOKEN";

pub(crate) type Log = Arc<Mutex<Vec<&'static str>>>;

pub(crate) fn command_name(command: &EngineCommand) -> &'static str {
    match command {
        EngineCommand::Answer { .. } => "answer",
        EngineCommand::Steer { .. } => "steer",
        EngineCommand::Mention { .. } => "mention",
        EngineCommand::Guidance { .. } => "guidance",
        EngineCommand::TakeBack { .. } => "take_back",
        EngineCommand::Pause { .. } => "pause",
        EngineCommand::StopSession { .. } => "stop_session",
        EngineCommand::ApprovalMode { .. } => "approval_mode",
        EngineCommand::CancelPause => "cancel_pause",
        EngineCommand::Stop => "stop",
        EngineCommand::Reassign { .. } => "reassign",
        EngineCommand::TakeOver { .. } => "take_over",
        EngineCommand::TakeOverEnded { .. } => "take_over_ended",
        EngineCommand::Status { .. } => "status",
        EngineCommand::Diff { .. } => "diff",
        EngineCommand::Raw { .. } => "raw",
        EngineCommand::Revert { .. } => "revert",
        EngineCommand::SpotCheck { .. } => "spot_check",
        EngineCommand::ProposeWorkflowChange { .. } => "propose_workflow_change",
        EngineCommand::Goal { .. } => "goal",
    }
}

/// Answers a command the way a willing engine would, and logs its name.
pub(crate) fn answer_command(command: EngineCommand, log: &Log) {
    log.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(command_name(&command));
    match command {
        EngineCommand::Answer { reply, .. }
        | EngineCommand::Reassign { reply, .. }
        | EngineCommand::Revert { reply, .. }
        | EngineCommand::SpotCheck { reply, .. }
        | EngineCommand::Goal { reply, .. } => {
            let _ = reply.send(Ok(()));
        }
        EngineCommand::Mention { reply, .. } | EngineCommand::Guidance { reply, .. } => {
            let _ = reply.send(Ok(MessageId::numbered(1)));
        }
        EngineCommand::Steer { reply, .. } => {
            let _ = reply.send(Err("no such session".to_owned()));
        }
        EngineCommand::TakeBack { reply, .. } => {
            let _ = reply.send(Vec::new());
        }
        EngineCommand::TakeOver { reply, .. } => {
            let _ = reply.send(Err("no such card".to_owned()));
        }
        EngineCommand::Status { reply } => {
            let _ = reply.send(StatusSummary::default());
        }
        _ => {}
    }
}

fn respond(mut inbox: mpsc::Receiver<EngineCommand>, log: Log) {
    tokio::spawn(async move {
        while let Some(command) = inbox.recv().await {
            answer_command(command, &log);
        }
    });
}

/// A willing engine that never returns, fed from the host's queue.
pub(crate) fn engine_for(mut queue: Queue, log: Log) {
    let (inbox, commands) = mpsc::channel(1);
    respond(commands, log);
    tokio::spawn(async move { forward(&mut queue, inbox).await });
}

struct Fixture {
    dir: tempfile::TempDir,
    state: Arc<ServerState>,
    log: Log,
}

impl Fixture {
    /// A willing engine behind the door.
    fn new(check_peers: bool, markers: AgentMarkers) -> Self {
        Self::build(check_peers, markers, Arc::new(agent_ancestor))
    }

    /// Checks peers with `ancestry` instead of the real process table.
    fn with_ancestry(markers: AgentMarkers, ancestry: AncestryCheck) -> Self {
        Self::build(true, markers, ancestry)
    }

    fn build(check_peers: bool, markers: AgentMarkers, ancestry: AncestryCheck) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ah")
            .tempdir_in("/tmp")
            .unwrap();
        let (events, _) = broadcast::channel(64);
        let (door, queue) = Door::new();
        let state = Arc::new(ServerState {
            run: RunId::new("run-20260930-010000-0001").unwrap(),
            pid: std::process::id(),
            token: TOKEN.to_owned(),
            home: Paths::new(dir.path().join("home")),
            markers,
            run_dir: dir.path().to_path_buf(),
            journal: dir.path().join(run_files::EVENTS),
            door,
            events,
            ancestry,
            check_peers,
        });
        let log = Log::default();
        engine_for(queue, log.clone());
        Self { dir, state, log }
    }

    fn serve(&self) -> PathBuf {
        let listener = bind(self.dir.path()).unwrap();
        tokio::spawn(serve(
            listener,
            self.state.clone(),
            CancellationToken::new(),
        ));
        socket_path(self.dir.path())
    }

    fn log(&self) -> Vec<&'static str> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

pub(crate) struct Client {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

impl Client {
    pub(crate) async fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).await.unwrap();
        let (read, write) = stream.into_split();
        let mut client = Self {
            read: BufReader::new(read),
            write,
        };
        let hello = ControlRequest::Hello {
            client: ClientKind::App,
            version: "test".to_owned(),
        };
        let welcome = client.call(1, hello).await;
        assert!(
            matches!(welcome, ControlReply::Welcome { .. }),
            "{welcome:?}"
        );
        client
    }

    pub(crate) async fn send(&mut self, id: u64, request: ControlRequest) {
        write_frame(&mut self.write, &Frame { id, body: request })
            .await
            .unwrap();
    }

    pub(crate) async fn next(&mut self) -> Frame<ControlReply> {
        read_frame(&mut self.read).await.unwrap().unwrap()
    }

    /// The reply to `request`, skipping events for earlier subscriptions.
    pub(crate) async fn call(&mut self, id: u64, request: ControlRequest) -> ControlReply {
        self.send(id, request).await;
        loop {
            let frame = self.next().await;
            if frame.id == id && !matches!(frame.body, ControlReply::Event { .. }) {
                return frame.body;
            }
        }
    }
}

pub(crate) fn answer(token: &str) -> ControlRequest {
    ControlRequest::Answer {
        answer: CheckpointAnswer {
            id: CheckpointId::numbered(1),
            action: AnswerAction::Approve,
            text: None,
            evidence_sha256: "e".repeat(64),
            cards: Vec::new(),
        },
        token: token.to_owned(),
    }
}

fn guidance() -> ControlRequest {
    ControlRequest::Guidance {
        text: "keep the tests fast".to_owned(),
        front_end: None,
    }
}

fn assert_error(reply: &ControlReply, needle: &str) {
    match reply {
        ControlReply::Error { message } => {
            assert!(message.contains(needle), "{message:?} lacks {needle:?}");
        }
        other => panic!("expected an error with {needle:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn socket_binds_and_connects_with_a_long_run_dir() {
    use std::os::unix::fs::FileTypeExt as _;

    let fixture = Fixture::new(false, test_markers());
    let home = tempfile::tempdir().unwrap();
    let run_dir = home
        .path()
        .join("state")
        .join("a-repository-folder-with-a-long-name-0123-5f0c2a9e4b71")
        .join("runs")
        .join("run-20260930-010000-0001");
    std::fs::create_dir_all(&run_dir).unwrap();
    let socket = socket_path(&run_dir);
    assert!(socket.as_os_str().len() >= 120, "{}", socket.display());

    let listener = bind(&run_dir).unwrap();
    tokio::spawn(serve(
        listener,
        fixture.state.clone(),
        CancellationToken::new(),
    ));
    let found = std::fs::symlink_metadata(&socket).unwrap();
    assert!(
        found.file_type().is_socket(),
        "the socket is in the run folder"
    );
    assert_eq!(found.permissions().mode() & 0o777, 0o600);

    let connection = crate::client::socket::Connection::connect(&run_dir, ClientKind::App)
        .await
        .unwrap()
        .expect("the supervisor answers");
    assert_eq!(connection.pid, std::process::id());

    let link = socket_address(&run_dir)
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    crate::run_files::remove_socket(&run_dir).unwrap();
    assert!(!socket.exists());
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the link is gone"
    );
}

#[tokio::test]
async fn socket_refuses_answer_without_token() {
    let fixture = Fixture::new(false, test_markers());
    let socket = fixture.serve();
    let mut client = Client::connect(&socket).await;

    assert_error(&client.call(2, answer("")).await, "token");
    assert_error(
        &client
            .call(3, answer("0123456789abcdef0123456789abcdeX"))
            .await,
        "token",
    );
    let mode = ControlRequest::ApprovalMode {
        mode: ApprovalMode::Auto,
        token: "short".to_owned(),
    };
    assert_error(&client.call(4, mode).await, "token");
    let reassign = ControlRequest::Reassign {
        card: argonauts_core::CardId::new("card-001").unwrap(),
        to: argonauts_core::names::WorkerRef::new("codex", None),
        rule_override: Some("review.other_family".to_owned()),
        token: None,
    };
    assert_error(&client.call(5, reassign).await, "token");
    assert!(fixture.log().is_empty(), "nothing reached the engine");

    assert_eq!(client.call(6, answer(TOKEN)).await, ControlReply::Ok);
    assert_eq!(
        client.call(7, guidance()).await,
        ControlReply::Queued {
            id: MessageId::numbered(1)
        },
        "guidance needs no token"
    );
    assert_eq!(fixture.log(), ["answer", "guidance"]);
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn the_first_request_must_be_hello() {
    let fixture = Fixture::new(false, test_markers());
    let socket = fixture.serve();
    let stream = UnixStream::connect(&socket).await.unwrap();
    let (read, write) = stream.into_split();
    let mut client = Client {
        read: BufReader::new(read),
        write,
    };
    assert_error(&client.call(1, ControlRequest::Status).await, "hello");
    assert!(
        read_frame::<_, ControlReply>(&mut client.read)
            .await
            .unwrap()
            .is_none()
    );
    assert!(fixture.log().is_empty());
}

#[tokio::test]
#[ignore = "run as a child process by socket_refuses_peer_descended_from_registered_agent"]
async fn peer_helper() {
    let (Some(socket), Ok(token)) = (std::env::var_os(PEER_SOCKET), std::env::var(PEER_TOKEN))
    else {
        return;
    };
    let mut go = String::new();
    std::io::stdin().lock().read_line(&mut go).unwrap();
    let mut client = Client::connect(Path::new(&socket)).await;
    let reply = client.call(2, answer(&token)).await;
    println!("REPLY:{}", serde_json::to_string(&reply).unwrap());
}

/// Runs `peer_helper` as its own process group, registered as an agent when `home` is given.
fn run_helper(socket: &Path, home: Option<&Paths>) -> ControlReply {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "host::server::tests::peer_helper",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PEER_SOCKET, socket)
        .env(PEER_TOKEN, TOKEN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    if let Some(home) = home {
        register_group(home, child.id());
    }
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"go\n").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find_map(|line| line.split_once("REPLY:").map(|(_, reply)| reply))
        .unwrap_or_else(|| panic!("the helper printed no reply: {stdout}"));
    serde_json::from_str(line).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_refuses_peer_descended_from_registered_agent() {
    let fixture = Fixture::new(true, test_markers());
    let socket = fixture.serve();

    let path = socket.clone();
    let unregistered = tokio::task::spawn_blocking(move || run_helper(&path, None))
        .await
        .unwrap();
    assert_eq!(
        unregistered,
        ControlReply::Ok,
        "the same peer, not an agent"
    );

    let home = fixture.state.home.clone();
    let registered = tokio::task::spawn_blocking(move || run_helper(&socket, Some(&home)))
        .await
        .unwrap();
    assert_error(&registered, "agent process group");
    assert_eq!(fixture.log(), ["answer"]);
}

/// The process table is scripted, so the walk up the parent chain to a registered group runs
/// the same on every system. No other process's environment is read.
#[tokio::test]
async fn approval_refused_under_a_registered_agent() {
    let table = ScriptedProcesses::agent_shells();
    let agents = table.clone();
    let fixture = Fixture::with_ancestry(
        AgentMarkers::builtin(),
        Arc::new(move |pid, home, markers| {
            agent_ancestor_in(&mut table.clone(), pid, home, markers)
        }),
    );
    agents.register_agents(&fixture.state.home);
    for (pid, marker) in [
        (30, "agent process group 20"),
        (31, "agent process group 21"),
    ] {
        let peer = Origin::Peer { pid: Some(pid) };
        assert_error(&apply(&fixture.state, answer(TOKEN), peer).await, marker);
        let mode = ControlRequest::ApprovalMode {
            mode: ApprovalMode::Auto,
            token: TOKEN.to_owned(),
        };
        assert_error(&apply(&fixture.state, mode, peer).await, marker);
        // A spot check has no token, but a right verdict can raise autonomy.
        assert_error(&apply(&fixture.state, spot_check(), peer).await, marker);
        assert!(
            matches!(
                apply(&fixture.state, guidance(), peer).await,
                ControlReply::Queued { .. }
            ),
            "guidance isn't an approval"
        );
    }
    let clean = Origin::Peer { pid: Some(32) };
    assert_eq!(
        apply(&fixture.state, answer(TOKEN), clean).await,
        ControlReply::Ok
    );
    assert_eq!(
        apply(&fixture.state, spot_check(), clean).await,
        ControlReply::Ok
    );
    assert_error(
        &apply(&fixture.state, answer(TOKEN), Origin::Peer { pid: None }).await,
        "can't be identified",
    );
    assert_error(
        &apply(
            &fixture.state,
            answer(TOKEN),
            Origin::Peer { pid: Some(99) },
        )
        .await,
        "gone",
    );
    assert_eq!(
        fixture.log(),
        ["guidance", "guidance", "answer", "spot_check"]
    );
}

fn spot_check() -> ControlRequest {
    ControlRequest::SpotCheck {
        id: CheckpointId::numbered(3),
        correct: true,
    }
}

#[tokio::test]
async fn pending_requests_skip_the_peer_check_but_not_the_token() {
    let fixture = Fixture::new(true, test_markers());
    assert!(
        submit(&fixture.state, answer(TOKEN), Origin::Pending)
            .await
            .is_ok()
    );
    assert!(
        submit(&fixture.state, answer("nope"), Origin::Pending)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn subscribe_replays_the_journal_then_streams_live_events() {
    let fixture = Fixture::new(false, test_markers());
    let lock = RunLock::try_acquire(fixture.dir.path()).unwrap().unwrap();
    let journal = Journal::open(&fixture.state.journal, &lock).unwrap();
    let sink = JournalSink::new(
        journal,
        fixture.state.run.clone(),
        fixture.state.events.clone(),
    );
    let note = |text: &str| {
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: text.to_owned(),
        })
    };
    for text in ["zero", "one", "two"] {
        sink.record(None, None, "conductor", note(text)).unwrap();
    }
    let socket = fixture.serve();
    let mut client = Client::connect(&socket).await;
    client
        .send(
            9,
            ControlRequest::Subscribe {
                from_seq: 1,
                from_snapshot: false,
            },
        )
        .await;
    let mut seen = Vec::new();
    for _ in 0..2 {
        let frame = client.next().await;
        assert_eq!(frame.id, 9);
        let ControlReply::Event { event } = frame.body else {
            panic!("{:?}", frame.body);
        };
        seen.push(event.seq);
    }
    assert_eq!(seen, [1, 2]);
    sink.record(None, None, "conductor", note("three")).unwrap();
    let ControlReply::Event { event } = client.next().await.body else {
        panic!("expected a live event");
    };
    assert_eq!(event.seq, 3);
    assert_eq!(
        client.call(10, ControlRequest::Status).await,
        ControlReply::Status {
            summary: Box::default()
        },
        "requests still work while subscribed"
    );
}

/// An event recorded just as the host closes: both are ready when the subscription next runs.
/// Without the bias its select picks either at random, so the rounds make a lost event certain
/// to show.
#[tokio::test]
async fn a_subscriber_gets_the_event_recorded_as_the_host_closes() {
    let fixture = Fixture::new(false, test_markers());
    let lock = RunLock::try_acquire(fixture.dir.path()).unwrap().unwrap();
    let journal = Journal::open(&fixture.state.journal, &lock).unwrap();
    let sink = JournalSink::new(
        journal,
        fixture.state.run.clone(),
        fixture.state.events.clone(),
    );
    for round in 0..32 {
        let (out, mut frames) = mpsc::channel(8);
        let closing = CancellationToken::new();
        let task = tokio::spawn(subscription(
            fixture.state.clone(),
            1,
            round,
            false,
            out,
            closing.clone(),
        ));
        // This test's runtime has one thread, so the subscription has read the log and waits
        // in its select once this task yields.
        tokio::task::yield_now().await;
        let note = RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: format!("round {round}"),
        });
        sink.record(None, None, "conductor", note).unwrap();
        closing.cancel();
        task.await.unwrap();
        let frame = frames.recv().await;
        let Some(Frame {
            body: ControlReply::Event { event },
            ..
        }) = frame
        else {
            panic!("round {round}: the subscription closed without the last event: {frame:?}");
        };
        assert_eq!(event.seq, round);
    }
}

#[tokio::test]
async fn a_parked_run_wakes_only_for_requests_that_need_the_engine() {
    let fixture = Fixture::new(false, test_markers());
    fixture.state.door.park();
    assert_error(
        &apply(
            &fixture.state,
            ControlRequest::StopSession {
                session: argonauts_core::names::SessionKey::numbered(1),
            },
            Origin::Pending,
        )
        .await,
        "no agent is running",
    );
    assert!(fixture.log().is_empty(), "nothing woke the engine");
    let state = fixture.state.clone();
    let waiting = tokio::spawn(async move { apply(&state, guidance(), Origin::Pending).await });
    tokio::time::timeout(Duration::from_secs(5), fixture.state.door.woken())
        .await
        .unwrap();
    assert!(matches!(
        waiting.await.unwrap(),
        ControlReply::Queued { .. }
    ));
    assert_eq!(fixture.log(), ["guidance"]);
    fixture.state.door.close();
    assert_error(
        &apply(&fixture.state, guidance(), Origin::Pending).await,
        "exiting",
    );
}

#[tokio::test]
async fn a_parked_run_answers_status_diff_and_raw_from_its_files() {
    let fixture = Fixture::new(false, test_markers());
    let lock = RunLock::try_acquire(fixture.dir.path()).unwrap().unwrap();
    let journal = Journal::open(&fixture.state.journal, &lock).unwrap();
    let sink = JournalSink::new(
        journal,
        fixture.state.run.clone(),
        fixture.state.events.clone(),
    );
    let card = argonauts_core::CardId::new("card-001").unwrap();
    std::fs::write(fixture.dir.path().join("card-001.diff"), "+hello\n").unwrap();
    let diff = RunEventKind::Card(argonauts_core::events::card::CardEvent::Diff {
        stat: argonauts_core::DiffStat::default(),
        path: PathBuf::from("card-001.diff"),
    });
    sink.record(Some(&card), None, "conductor", diff).unwrap();
    let session = argonauts_core::names::SessionKey::numbered(1);
    let session_dir = argonauts_core::layout::session_dir(fixture.dir.path(), &session);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join(argonauts_core::layout::session_files::RAW_STDOUT),
        "{\"line\":1}\n",
    )
    .unwrap();
    fixture.state.door.park();

    let status = apply(&fixture.state, ControlRequest::Status, Origin::Pending).await;
    assert!(matches!(status, ControlReply::Status { .. }), "{status:?}");
    let request = ControlRequest::Diff {
        card: card.clone(),
        scope: argonauts_core::api::DiffScope::Card,
    };
    match apply(&fixture.state, request, Origin::Pending).await {
        ControlReply::Diff { diff } => assert_eq!(diff.text, "+hello\n"),
        other => panic!("{other:?}"),
    }
    match apply(
        &fixture.state,
        ControlRequest::Raw { session },
        Origin::Pending,
    )
    .await
    {
        ControlReply::Raw { raw } => assert_eq!(raw.lines, ["{\"line\":1}"]),
        other => panic!("{other:?}"),
    }
    assert!(fixture.log().is_empty(), "no engine was needed");
    assert_eq!(
        fixture.state.door.state(),
        super::super::door::EngineState::Parked
    );
}

#[tokio::test]
async fn a_parked_run_wakes_for_the_last_turns_diff() {
    let fixture = Fixture::new(false, test_markers());
    fixture.state.door.park();
    let state = fixture.state.clone();
    let request = ControlRequest::Diff {
        card: argonauts_core::CardId::new("card-001").unwrap(),
        scope: DiffScope::LastTurn,
    };
    let waiting = tokio::spawn(async move { apply(&state, request, Origin::Pending).await });
    tokio::time::timeout(Duration::from_secs(5), fixture.state.door.woken())
        .await
        .unwrap();
    waiting.await.unwrap();
    assert_eq!(
        fixture.log(),
        ["diff"],
        "only an engine diffs the last turn"
    );
}
