use std::path::PathBuf;

use argonauts_adapter_codex::CodexAdapter;
use argonauts_conductor::doctor::standing::oldest_supported;
use argonauts_conductor::{DoctorReport, GitCheck};
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::app::{AnswerTarget, AppChoice, UiCommand};
use argonauts_core::input::SlashCommand;
use argonauts_core::names::ModelRef;
use argonauts_testkit::TempRepo;

use super::super::test_support::{Call, CliOptions, TestCtx, TestDriver};
use super::*;

/// A version older than any Codex was recorded with.
const OLD: &str = "0.1.0";

/// A fake Codex outside any install Homebrew or npm made, so its own `update` is its updater.
/// Its version lives in a file that `update` runs `on_update` against.
struct FakeCodex {
    dir: tempfile::TempDir,
}

impl FakeCodex {
    fn new(on_update: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fake = Self { dir };
        std::fs::write(fake.version_file(), format!("{OLD}\n")).unwrap();
        let version = fake.version_file().display().to_string();
        let on_update = on_update.replace("$VERSION", &version);
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo \"codex-cli $(cat '{version}')\" ;;\n  \
             update) {on_update} ;;\nesac\n"
        );
        argonauts_testkit::write_script(&fake.binary(), &script).unwrap();
        fake
    }

    /// One whose update installs the oldest version supported.
    fn updating() -> Self {
        Self::new(&format!("echo {} > '$VERSION'", supported()))
    }

    fn binary(&self) -> PathBuf {
        self.dir.path().join("codex")
    }

    fn version_file(&self) -> PathBuf {
        self.dir.path().join("version")
    }

    /// Your settings with this Codex as a worker beside the main agent.
    fn options(&self) -> CliOptions {
        self.options_with("")
    }

    /// The same, with `crew` as more of the `[crew]` table.
    fn options_with(&self, crew: &str) -> CliOptions {
        let config = format!(
            "[crew]\nworker = [\"codex/gpt-5.6\"]\n{crew}\n[agents.codex]\nbinary = {:?}\n",
            self.binary().display().to_string()
        );
        CliOptions {
            prepare: Some(Box::new(move |repo: &TempRepo| {
                std::fs::write(repo.global_config(), config).unwrap();
            })),
            ..CliOptions::default()
        }
    }

    /// What Doctor finds of the CLIs as the app starts: this Codex alone.
    async fn report(&self) -> DoctorReport {
        let codex = check_agent_at(&CodexAdapter, Some(&self.binary()), VERSION_TIMEOUT).await;
        DoctorReport {
            agents: vec![codex],
            git: GitCheck {
                version: None,
                problem: None,
            },
        }
    }
}

fn supported() -> &'static str {
    oldest_supported(CodexAdapter.tested_versions()).expect("Codex was recorded with a version")
}

fn crew_run(task: &str) -> UiCommand {
    UiCommand::Slash {
        command: SlashCommand::Crew { task: task.into() },
    }
}

fn request(mode: RunMode) -> StartRun {
    StartRun {
        request: "tidy the date helpers".to_owned(),
        mode,
        forced: true,
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        overrides: Vec::new(),
        include_uncommitted: false,
        main_agent: None,
        approval_mode: None,
        outside_agent: None,
        instructions: Vec::new(),
        request_settings: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    }
}

fn answer(id: &ChoiceId, value: &str) -> UiCommand {
    UiCommand::Answer {
        to: AnswerTarget::Choice { id: id.clone() },
        action: choices::choose(value),
        text: None,
        evidence_sha256: None,
        tasks: Vec::new(),
        remember: false,
    }
}

fn is_question(event: &AppEvent) -> bool {
    matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::NeededUpdate)
}

fn is_started(event: &AppEvent) -> bool {
    matches!(event, AppEvent::Attached { .. })
}

async fn question(driver: &mut TestDriver) -> AppChoice {
    match driver.until(is_question).await {
        AppEvent::Choice { choice } => choice,
        other => unreachable!("{other:?}"),
    }
}

/// The notices among `events`, as text.
fn notices(events: &[AppEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// What each question among `events` closed with.
fn closed(events: &[AppEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            AppEvent::ChoiceClosed { outcome, .. } => Some(outcome.as_str()),
            _ => None,
        })
        .collect()
}

/// The question as the versions picture has it: Codex 0.158.0 from Homebrew, where 0.160.0 is
/// the oldest version supported and another CLI holds the main role.
#[test]
fn the_question_reads_as_its_picture() {
    let cli = Outdated {
        id: "codex".to_owned(),
        name: "Codex".to_owned(),
        version: "0.158.0".to_owned(),
        check: CliVersionCheck {
            standing: VersionStanding::Older,
            oldest_supported: Some("0.160.0".to_owned()),
            update_command: Some("brew upgrade codex".to_owned()),
            ..CliVersionCheck::default()
        },
        can_leave_out: true,
        run_going: false,
    };
    let options = cli.options();
    let label_width = options
        .iter()
        .map(|option| option.label.chars().count())
        .max()
        .unwrap();
    let mut drawn = vec![
        format!("◆ {}", cli.title()),
        format!("  {WHAT_IT_MEANS}"),
        String::new(),
    ];
    for (index, option) in options.iter().enumerate() {
        let pointer = if index == 0 { "❯" } else { " " };
        let detail = option.detail.as_deref().unwrap();
        // Four cells after the longest answer, as the screen draws them.
        let label = format!("{:<width$}", option.label, width = label_width + 4);
        drawn.push(format!("  {pointer} {}. {label}{detail}", index + 1));
    }
    let picture = include_str!("../../../../tui/tests/golden/overview/versions.txt");
    let pictured: Vec<&str> = picture.lines().skip(13).take(6).collect();
    assert_eq!(drawn, pictured);
    let keys: Vec<(char, AnswerAction)> = options
        .into_iter()
        .map(|option| (option.key, option.action))
        .collect();
    assert_eq!(
        keys,
        [
            ('u', choices::choose(UPDATE)),
            ('l', choices::choose(LEAVE_OUT)),
            ('r', choices::choose(RUN_ANYWAY)),
        ]
    );
}

/// The main agent is the one you talk to, so its CLI can't be left out; a CLI nobody knows how
/// to update is never offered an update; and a version known not to work says that.
#[test]
fn only_the_answers_that_can_be_carried_out_are_offered() {
    let mut cli = Outdated {
        id: "codex".to_owned(),
        name: "Codex".to_owned(),
        version: "0.161.0".to_owned(),
        check: CliVersionCheck {
            standing: VersionStanding::Broken,
            ..CliVersionCheck::default()
        },
        can_leave_out: false,
        run_going: false,
    };
    assert_eq!(
        cli.title(),
        "Codex 0.161.0 is known not to work with Argonauts Code"
    );
    let labels = |cli: &Outdated| -> Vec<String> {
        cli.options()
            .into_iter()
            .map(|option| option.label)
            .collect()
    };
    assert_eq!(labels(&cli), ["Run with it anyway"]);
    cli.check.update_command = Some("codex update".to_owned());
    assert_eq!(labels(&cli), ["Update it now", "Run with it anyway"]);
    assert_eq!(
        cli.unasked_line(),
        "Codex 0.161.0 is known not to work with Argonauts Code · running with it anyway · \
         codex update updates it"
    );
}

/// A crew run that would use an outdated Codex waits on the question. Update it now runs
/// Codex's updater, says the version it is at, and the run starts. Nothing is asked twice.
#[tokio::test]
async fn update_it_now_updates_the_cli_then_the_run_starts() {
    let codex = FakeCodex::updating();
    let mut driver = TestDriver::start_with(codex.options()).await;
    driver.cli_facts.checked(codex.report().await);

    driver.send(crew_run("tidy the date helpers")).await;
    let asked = question(&mut driver).await;
    assert_eq!(
        asked.title,
        format!(
            "Codex {OLD} is older than Argonauts Code supports ({})",
            supported()
        )
    );
    assert_eq!(asked.body, [WHAT_IT_MEANS]);
    assert_eq!(
        asked.options[0].detail.as_deref(),
        Some("codex update, then the run starts")
    );
    assert!(driver.conductor.starts().is_empty(), "nothing has started");

    driver.send(answer(&asked.id, UPDATE)).await;
    driver.until(is_started).await;
    let started = driver.conductor.starts();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].request, "tidy the date helpers");
    assert!(started[0].request_settings.is_empty());
    let now = format!("Codex is now {}.", supported());
    assert!(notices(driver.seen()).contains(&now.as_str()));
    assert_eq!(closed(driver.seen()), ["Updating Codex · codex update"]);
    let installed = std::fs::read_to_string(codex.version_file()).unwrap();
    assert_eq!(installed.trim(), supported());

    let events = driver.exit().await;
    let questions = events.iter().filter(|event| is_question(event)).count();
    assert_eq!(questions, 1);
}

/// Esc starts nothing, and the next run is asked about again.
#[tokio::test]
async fn esc_leaves_the_run_unstarted() {
    let codex = FakeCodex::updating();
    let mut driver = TestDriver::start_with(codex.options()).await;
    driver.cli_facts.checked(codex.report().await);

    driver.send(crew_run("tidy the date helpers")).await;
    let asked = question(&mut driver).await;
    let to = AnswerTarget::Choice {
        id: asked.id.clone(),
    };
    driver.send(UiCommand::SetAside { to }).await;
    driver
        .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
        .await;
    assert_eq!(
        closed(driver.seen()),
        [format!("Not started · Codex {OLD} still needs an update")]
    );

    driver.send(crew_run("tidy them after all")).await;
    let again = question(&mut driver).await;
    assert_ne!(again.id, asked.id);
    let started = driver.conductor.starts();
    driver.exit().await;
    assert!(started.is_empty(), "{started:?}");
    let installed = std::fs::read_to_string(codex.version_file()).unwrap();
    assert_eq!(installed.trim(), OLD, "nothing updated it");
}

/// A follow-up is a run like any other: it waits on the question, then continues the merged
/// run it builds on.
#[tokio::test]
async fn a_follow_up_run_waits_on_the_question_too() {
    let codex = FakeCodex::updating();
    let merged = RunSummary {
        run: argonauts_core::RunId::new("run-20261010-091503-3fa2").unwrap(),
        title: "Export the report".to_owned(),
        name: None,
        mode: RunMode::Captain,
        workflow: "captain".to_owned(),
        state: RunState::Merged,
        stopped_at: String::new(),
        updated: time::OffsetDateTime::UNIX_EPOCH,
        spent: argonauts_core::Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: false,
        remind: false,
        shape_known: true,
    };
    let options = CliOptions {
        runs: vec![merged.clone()],
        ..codex.options()
    };
    let mut driver = TestDriver::start_with(options).await;
    driver.cli_facts.checked(codex.report().await);
    let conductor = driver.conductor.clone();
    let continued = || {
        conductor.calls().into_iter().find_map(|call| match call {
            Call::Continue { merged, request } => Some((merged, request)),
            _ => None,
        })
    };

    let command = SlashCommand::FollowUp {
        run: None,
        request: "Add a CSV export too".to_owned(),
    };
    driver.send(UiCommand::Slash { command }).await;
    let asked = question(&mut driver).await;
    // Captain's reviewer may run on Codex, so the run waits, and nothing has continued.
    assert!(continued().is_none());

    driver.send(answer(&asked.id, RUN_ANYWAY)).await;
    driver.exit().await;
    let (from, request) = continued().expect("the follow-up started");
    assert_eq!(from, merged.run);
    assert_eq!(request.request, "Add a CSV export too");
}

/// The state and the screen's events, for tests that call the handlers themselves.
struct Fixture {
    parts: TestCtx,
    state: NeededUpdateState,
}

impl Fixture {
    async fn new(codex: &FakeCodex) -> Self {
        Self::with(codex, codex.options()).await
    }

    async fn with(codex: &FakeCodex, options: CliOptions) -> Self {
        let (parts, _) = TestCtx::new(options);
        parts.ctx.cli_facts.checked(codex.report().await);
        Self {
            parts,
            state: NeededUpdateState::default(),
        }
    }

    async fn start(&mut self, request: StartRun, mode: ApprovalMode) -> Start {
        before_run(&mut self.parts.ctx, &mut self.state, request, mode).await
    }

    fn events(&mut self) -> Vec<AppEvent> {
        std::iter::from_fn(|| self.parts.events.try_recv().ok()).collect()
    }

    /// The question now on screen.
    fn asked(&mut self) -> AppChoice {
        let mut questions: Vec<AppChoice> = self
            .events()
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::Choice { choice } => Some(choice),
                _ => None,
            })
            .collect();
        assert_eq!(questions.len(), 1, "{questions:?}");
        questions.remove(0)
    }

    /// The run an answer lets start.
    fn answered(&mut self, id: &ChoiceId, value: &str) -> StartRun {
        let ctx = &mut self.parts.ctx;
        let effects =
            answer_choice(ctx, &mut self.state, id.clone(), choices::choose(value)).unwrap();
        match effects.into_iter().next() {
            Some(Effect::StartRun(request)) => *request,
            other => panic!("{other:?}"),
        }
    }
}

fn starts_now(start: Start) -> StartRun {
    match start {
        Start::Now(request) => *request,
        Start::Waits => panic!("the run waits on a question"),
        Start::Refused(why) => panic!("refused: {why}"),
    }
}

fn waits(start: &Start) -> bool {
    matches!(start, Start::Waits)
}

/// Leave it out turns Codex off for this run alone, and Run with it anyway changes nothing.
/// Either way the run starts at once, and the next run is asked again.
#[tokio::test]
async fn leaving_it_out_or_running_anyway_holds_for_one_run() {
    let codex = FakeCodex::updating();
    let mut fixture = Fixture::new(&codex).await;

    assert!(waits(
        &fixture
            .start(request(RunMode::Crew), ApprovalMode::Supervised)
            .await
    ));
    let asked = fixture.asked();
    let labels: Vec<&str> = asked
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "Update it now",
            "Leave Codex out of this run",
            "Run with it anyway"
        ]
    );
    let without = fixture.answered(&asked.id, LEAVE_OUT);
    assert_eq!(without.request_settings, ["agents.codex.enabled=false"]);
    assert_eq!(
        closed(&fixture.events()),
        ["Leaving Codex out of this run."]
    );
    // The run host reads that line as the request's own layer of settings, over every other.
    let sources = argonauts_registry::SettingsSources {
        interactive: true,
        request: without.request_settings.clone(),
        ..argonauts_registry::SettingsSources::default()
    };
    let resolved = argonauts_registry::resolve(&sources).unwrap();
    assert!(!resolved.settings.agents["codex"].enabled);
    // Started again with your answer, it goes ahead unasked, with Codex off in its settings.
    let going = starts_now(fixture.start(without, ApprovalMode::Supervised).await);
    let settings = run_settings(fixture.parts.ctx.settings(), &going);
    assert!(!settings.agents["codex"].enabled);
    assert!(fixture.parts.ctx.settings().agents["codex"].enabled);
    assert!(fixture.events().is_empty());

    assert!(waits(
        &fixture
            .start(request(RunMode::Crew), ApprovalMode::Supervised)
            .await
    ));
    let asked = fixture.asked();
    let anyway = fixture.answered(&asked.id, RUN_ANYWAY);
    assert!(anyway.request_settings.is_empty());
    assert_eq!(
        closed(&fixture.events()),
        [format!("Running with Codex {OLD}.")]
    );
    starts_now(fixture.start(anyway, ApprovalMode::Supervised).await);
    assert!(fixture.events().is_empty());

    // The answer was for that run. The next one is asked about again.
    assert!(waits(
        &fixture
            .start(request(RunMode::Crew), ApprovalMode::Supervised)
            .await
    ));
    // And a run that comes while one waits is turned away, saying what it waits on.
    let Start::Refused(why) = fixture
        .start(request(RunMode::Crew), ApprovalMode::Supervised)
        .await
    else {
        panic!("a second run started while the first waits");
    };
    assert_eq!(
        why,
        "another run waits on the question about Codex's update, so answer that first"
    );
}

/// An update that fails says why and asks again, with the run still waiting.
#[tokio::test]
async fn an_update_that_fails_says_why_and_asks_again() {
    let codex = FakeCodex::new("echo checking; echo 'no network' >&2; exit 1");
    let mut fixture = Fixture::new(&codex).await;
    assert!(waits(
        &fixture
            .start(request(RunMode::Crew), ApprovalMode::Supervised)
            .await
    ));
    let asked = fixture.asked();
    let ctx = &mut fixture.parts.ctx;
    let effects =
        answer_choice(ctx, &mut fixture.state, asked.id, choices::choose(UPDATE)).unwrap();
    assert!(effects.is_empty(), "the run waits for the update");
    let Some(Internal::CliUpdated(done)) = fixture.parts.internal.recv().await else {
        panic!("the update never reported back");
    };
    let effects = updated(&mut fixture.parts.ctx, &mut fixture.state, *done);
    assert!(effects.is_empty(), "the run still waits");
    let events = fixture.events();
    assert_eq!(notices(&events), ["Codex couldn't be updated: no network"]);
    assert_eq!(events.iter().filter(|event| is_question(event)).count(), 1);
}

/// While a run is going, in this folder or another, a CLI isn't updated: the question says
/// why and offers the rest, and an answer given as one starts is put off the same way.
#[tokio::test]
async fn a_cli_is_never_updated_while_a_run_is_going() {
    let codex = FakeCodex::updating();
    let mut fixture = Fixture::new(&codex).await;
    assert!(waits(
        &fixture
            .start(request(RunMode::Crew), ApprovalMode::Supervised)
            .await
    ));
    let asked = fixture.asked();
    assert_eq!(asked.options[0].label, "Update it now");

    // A run starts somewhere before you answer.
    let (pid, start_time) = crate::ancestry::own_process();
    let going = crate::live_registry::LiveRunHost {
        pid,
        start_time,
        repo_root: PathBuf::from("/another/folder"),
        run: argonauts_core::RunId::new("run-20261010-091503-3fa2").unwrap(),
        sessions: 1,
    };
    crate::live_registry::register(&fixture.parts.env.paths, &going).unwrap();
    let ctx = &mut fixture.parts.ctx;
    let effects =
        answer_choice(ctx, &mut fixture.state, asked.id, choices::choose(UPDATE)).unwrap();
    assert!(effects.is_empty());
    let events = fixture.events();
    assert_eq!(closed(&events), ["Codex wasn't updated."]);
    let again: Vec<&AppChoice> = events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Choice { choice } => Some(choice),
            _ => None,
        })
        .collect();
    assert_eq!(
        again[0].body,
        [
            WHAT_IT_MEANS,
            "A run is going now, so it can't be updated until that ends: codex update"
        ]
    );
    let labels: Vec<&str> = again[0]
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["Leave Codex out of this run", "Run with it anyway"]
    );
    let installed = std::fs::read_to_string(codex.version_file()).unwrap();
    assert_eq!(installed.trim(), OLD);
}

/// Autopilot is never asked: one line says the run goes on with the CLI as it is and what
/// updates it, and nothing updates it.
#[tokio::test]
async fn autopilot_is_told_in_one_line_and_runs_anyway() {
    let codex = FakeCodex::updating();
    let mut fixture = Fixture::new(&codex).await;
    let going = starts_now(
        fixture
            .start(request(RunMode::Crew), ApprovalMode::Autopilot)
            .await,
    );
    assert!(going.request_settings.is_empty());
    let events = fixture.events();
    assert_eq!(
        notices(&events),
        [format!(
            "Codex {OLD} is older than Argonauts Code supports ({}) · running with it anyway · \
             codex update updates it",
            supported()
        )]
    );
    assert!(!events.iter().any(is_question));
    let installed = std::fs::read_to_string(codex.version_file()).unwrap();
    assert_eq!(installed.trim(), OLD);
}

/// Only a run that would use the CLI is held: Captain works alone and is reviewed, so a CLI
/// that only workers run on isn't asked about, and one the run has off isn't either.
#[tokio::test]
async fn a_run_that_would_not_use_the_cli_is_not_asked() {
    let codex = FakeCodex::updating();
    let others_review = "reviewer = [\"antigravity-cli\"]\njudge = [\"antigravity-cli\"]\n";
    let mut fixture = Fixture::with(&codex, codex.options_with(others_review)).await;
    starts_now(
        fixture
            .start(request(RunMode::Captain), ApprovalMode::Supervised)
            .await,
    );
    // The crew's workers would run on it.
    assert!(waits(
        &fixture
            .start(request(RunMode::Crew), ApprovalMode::Supervised)
            .await
    ));
    let asked = fixture.asked();
    set_aside(&mut fixture.parts.ctx, &mut fixture.state, asked.id);
    fixture.events();
    let mut off = request(RunMode::Crew);
    off.request_settings
        .push("agents.codex.enabled=false".to_owned());
    starts_now(fixture.start(off, ApprovalMode::Supervised).await);
    assert!(fixture.events().is_empty());

    // With Codex as the main agent it can't be left out, in a Captain run or any other.
    let mut captain = request(RunMode::Captain);
    captain.main_agent = Some(ModelRef::new("codex", Some("gpt-5.6".to_owned())));
    assert!(waits(
        &fixture.start(captain, ApprovalMode::Supervised).await
    ));
    let labels: Vec<String> = fixture
        .asked()
        .options
        .into_iter()
        .map(|option| option.label)
        .collect();
    assert_eq!(labels, ["Update it now", "Run with it anyway"]);
}

/// A CLI you updated in another terminal since the app started is read again before anything
/// is said of it, so nothing is asked.
#[tokio::test]
async fn a_cli_updated_since_the_app_started_is_not_asked_about() {
    let codex = FakeCodex::updating();
    let mut fixture = Fixture::new(&codex).await;
    std::fs::write(codex.version_file(), supported()).unwrap();
    starts_now(
        fixture
            .start(request(RunMode::Crew), ApprovalMode::Supervised)
            .await,
    );
    assert!(fixture.events().is_empty());
    let report = fixture.parts.ctx.cli_facts.report().unwrap();
    assert_eq!(report.agents[0].version.as_deref(), Some(supported()));
}

/// A version that needs no update never holds a run.
#[tokio::test]
async fn a_supported_version_is_never_asked_about() {
    let codex = FakeCodex::updating();
    std::fs::write(codex.version_file(), supported()).unwrap();
    let mut fixture = Fixture::new(&codex).await;
    starts_now(
        fixture
            .start(request(RunMode::Crew), ApprovalMode::Supervised)
            .await,
    );
    assert!(fixture.events().is_empty());
}
