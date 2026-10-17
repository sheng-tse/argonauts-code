use std::path::PathBuf;
use std::sync::Arc;

use argonauts_conductor::{AgentCheck, AgentStatus, DoctorReport, GitCheck};
use argonauts_core::Paths;
use argonauts_core::adapter::SetupHints;
use argonauts_core::app::{AppChoice, AppEvent};
use argonauts_core::clock::SystemClock;
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::steps::SchemaStepRunner;
use argonauts_testkit::{FakeDirectory, TempRepo};
use tokio::sync::mpsc;

use super::*;
use crate::app_settings::AppSettings;
use crate::controller::test_support::FakeConductor;
use crate::controller::{AppServices, Outbox};
use crate::env::AppEnv;
use crate::trace::Trace;

struct Fixture {
    repo: TempRepo,
    ctx: Ctx,
    events: mpsc::UnboundedReceiver<AppEvent>,
    _internal: mpsc::UnboundedReceiver<Internal>,
}

impl Fixture {
    fn new(interactive: bool) -> Self {
        Self::prepared(interactive, |_| {})
    }

    fn prepared(interactive: bool, prepare: impl FnOnce(&TempRepo)) -> Self {
        let repo = TempRepo::new().unwrap();
        prepare(&repo);
        let paths = Paths::new(repo.home());
        let mut env = AppEnv::for_repo(paths.clone(), repo.root(), interactive).unwrap();
        env.agent_session = false;
        env.user_home = Some(repo.user_home().to_owned());
        let env = Arc::new(env);
        let settings = AppSettings::load(&env, Vec::new(), interactive).unwrap();
        let (plan_usage, health) = argonauts_conductor::boards(&paths);
        let services = AppServices {
            conductor: Arc::new(FakeConductor::default()),
            approvals: None,
            workers: Arc::new(FakeDirectory::new()),
            steps: Arc::new(SchemaStepRunner),
            registry: crate::controller::test_support::quiet_registry(
                settings.settings(),
                repo.root(),
            ),
            plan_usage,
            health,
            clock: Arc::new(SystemClock),
        };
        let (events_sender, events) = mpsc::unbounded_channel();
        let out = Outbox::new(events_sender, Arc::new(Trace::off()));
        let (internal_sender, internal) = mpsc::unbounded_channel();
        let ctx = Ctx::new(env, settings, services, out, internal_sender);
        Self {
            repo,
            ctx,
            events,
            _internal: internal,
        }
    }

    fn drain(&mut self) -> Vec<AppEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    fn global(&self) -> String {
        std::fs::read_to_string(self.repo.global_config()).unwrap_or_default()
    }
}

fn agent(id: &str, name: &str, status: AgentStatus) -> FoundAgent {
    let installed = status != AgentStatus::NotInstalled;
    FoundAgent {
        id: id.to_owned(),
        name: name.to_owned(),
        version: installed.then(|| "1.0.0".to_owned()),
        path: installed.then(|| PathBuf::from(format!("/bin/{id}"))),
        status,
        enabled: true,
        hints: SetupHints {
            install: Some(format!("install {id}")),
            login: Some(format!("{id} login")),
            login_check: None,
        },
        login: LoginState::Unknown,
        newest_tested: None,
        unread: Vec::new(),
        builtins: Vec::new(),
        problem: None,
        lacks: Vec::new(),
        check: Default::default(),
    }
}

/// The doctor's report on `agents`, as the header reads it.
fn header_for(agents: &[FoundAgent]) -> Header {
    let checks = agents.iter().map(|agent| AgentCheck {
        id: agent.id.clone(),
        display_name: agent.name.clone(),
        binary: agent.id.clone(),
        path: agent.path.clone(),
        version: agent.version.clone(),
        status: agent.status,
        tested_versions: Vec::new(),
        cheapest_model: String::new(),
        cheapest_effort: None,
        problem: None,
        enabled: agent.enabled,
    });
    Header {
        report: DoctorReport {
            agents: checks.collect(),
            git: GitCheck {
                version: Some("2.50.0".to_owned()),
                problem: None,
            },
        },
        runs: RunCounts::default(),
    }
}

fn two_ready() -> Vec<FoundAgent> {
    vec![
        agent("claude-code", "Claude Code", AgentStatus::Tested),
        agent("codex", "Codex", AgentStatus::Tested),
        agent(
            "antigravity-cli",
            "Antigravity CLI",
            AgentStatus::NotInstalled,
        ),
        agent("opencode", "OpenCode", AgentStatus::NotInstalled),
    ]
}

/// Starts the setup with its probes answered: these logins, and these servers.
async fn start(
    fixture: &mut Fixture,
    agents: Vec<FoundAgent>,
    logins: Vec<(String, LoginState)>,
    servers: Vec<LocalServer>,
) -> (Setup, Vec<Effect>) {
    let header = header_for(&agents);
    let (mut setup, mut effects) =
        Setup::begin_with(&mut fixture.ctx, agents, header, Probing::Fed)
            .await
            .unwrap();
    if !setup.done {
        effects.extend(
            setup
                .on_update(&mut fixture.ctx, SetupUpdate::Logins(logins))
                .await
                .unwrap(),
        );
        effects.extend(
            setup
                .on_update(&mut fixture.ctx, SetupUpdate::Servers(servers))
                .await
                .unwrap(),
        );
    }
    (setup, effects)
}

fn open_choice(events: &[AppEvent]) -> Option<AppChoice> {
    events.iter().rev().find_map(|event| match event {
        AppEvent::Choice { choice } => Some(choice.clone()),
        _ => None,
    })
}

fn finished(effects: &[Effect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, Effect::SetupDone))
}

/// Answers each question with `pick` until the setup finishes. Returns every choice asked.
async fn answer_all(
    fixture: &mut Fixture,
    setup: &mut Setup,
    mut effects: Vec<Effect>,
    pick: impl Fn(&AppChoice) -> AnswerAction,
) -> (Vec<AppChoice>, Vec<AppEvent>) {
    let mut asked = Vec::new();
    let mut seen = Vec::new();
    for _ in 0..20 {
        let events = fixture.drain();
        seen.extend(events.clone());
        if finished(&effects) {
            return (asked, seen);
        }
        let choice = open_choice(&events).expect("a question while the setup is open");
        assert!(setup.owns(&choice.id));
        let action = pick(&choice);
        asked.push(choice.clone());
        effects = setup
            .answer(&mut fixture.ctx, &choice.id, action, None)
            .await
            .unwrap();
    }
    panic!("the setup never finished");
}

fn enter(choice: &AppChoice) -> AnswerAction {
    assert!(choice.options[0].recommended, "{choice:?}");
    choice.options[0].action.clone()
}

/// Each `key = value` line's key, with its table, in file order.
fn written_keys(text: &str) -> Vec<String> {
    let mut table = String::new();
    let mut keys = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = format!("{name}.");
        } else if let Some((key, _)) = line.split_once(" = ")
            && !line.starts_with('#')
        {
            keys.push(format!("{table}{}", key.trim()));
        }
    }
    keys
}

/// Every key line has a comment line right above it, or above its table header.
fn each_key_commented(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    lines.iter().enumerate().all(|(index, line)| {
        let is_key = line.contains(" = ") && !line.starts_with('#');
        if !is_key {
            return true;
        }
        let above = &lines[..index];
        let before = above.iter().rev().find(|line| !line.is_empty());
        before.is_some_and(|line| line.starts_with('#'))
    })
}

fn notices(events: &[AppEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn guided_setup_enter_through_writes_exactly_the_recommended_keys_each_commented() {
    let mut fixture = Fixture::new(true);
    let (mut setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let (asked, seen) = answer_all(&mut fixture, &mut setup, effects, enter).await;

    let titles: Vec<&str> = asked.iter().map(|choice| choice.title.as_str()).collect();
    assert_eq!(
        titles,
        vec![
            "Welcome to Argonauts Code",
            "Your agents",
            "Who you talk to",
            "How the crew works",
            "Who orchestrates",
            "Who does what",
            "Approvals",
            "Usage counts",
            "This folder",
        ]
    );
    let text = fixture.global();
    let mut expected = vec![
        "crew.main",
        "crew.worker",
        "crew.reviewer",
        "crew.judge",
        "workflow",
        "roles.orchestrator.models",
        "telemetry.enabled",
    ];
    expected.extend([
        "approvals.understand.mode",
        "approvals.plan.mode",
        "approvals.merge.mode",
        "approvals.stuck.mode",
        "approvals.limit.mode",
        "approvals.change_request.mode",
        "approvals.autopilot",
        // The limits section ends on its default, which keeps its commented limits inside it.
        "limits.when_reached",
    ]);
    let mut keys = written_keys(&text);
    keys.sort();
    expected.sort();
    assert_eq!(keys, expected, "{text}");
    assert!(each_key_commented(&text), "{text}");
    assert!(
        text.contains(r#"worker = ["claude-code", "codex"]"#),
        "{text}"
    );
    assert!(text.contains(r#"main = "claude-code""#), "{text}");
    assert!(text.contains(r#"workflow = "orchestrator""#), "{text}");

    let closing = notices(&seen)
        .into_iter()
        .find(|text| text.starts_with("Saved to"))
        .expect("the closing line");
    for command in [
        "/model",
        "/effort",
        "/workflow",
        "/agents",
        "/approvals",
        "/permissions",
        "/config",
    ] {
        assert!(closing.contains(command), "{closing}");
    }
    assert!(!fixture.repo.root().join(".argonauts").exists());
    assert!(fixture.repo.home().join("terms-note-shown").exists());
    assert!(!crate::needs_guided_setup(&Paths::new(fixture.repo.home())));
}

#[tokio::test]
async fn guided_setup_without_a_terminal_writes_no_safety_key_and_finishes_at_once() {
    let mut fixture = Fixture::new(false);
    let (setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    assert!(finished(&effects));
    assert!(setup.done);
    assert!(open_choice(&fixture.drain()).is_none());
    let text = fixture.global();
    assert_eq!(
        written_keys(&text),
        vec![
            "workflow",
            "crew.main",
            "crew.worker",
            "crew.reviewer",
            "crew.judge",
            "roles.orchestrator.models",
            "limits.when_reached"
        ],
        "{text}"
    );
    assert!(!text.contains("approvals"), "{text}");
}

#[tokio::test]
async fn guided_setup_writes_the_starter_mods_and_a_readme_in_every_place_you_can_change() {
    let mut fixture = Fixture::new(false);
    let (_setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    assert!(finished(&effects));
    let home = fixture.repo.home();
    for file in [
        "README.md",
        "workflows/README.md",
        "roles/README.md",
        "mods/README.md",
        "mods/stats.toml",
        "mods/bottom-line.toml",
        "mods/branch.toml",
        "mods/checks.toml",
    ] {
        assert!(home.join(file).is_file(), "{file}");
    }
    // A built-in card draws from its own settings, so no file of its name is written.
    for task in ["mods/context.toml", "mods/plan-usage.toml"] {
        assert!(!home.join(task).exists(), "{task}");
    }
    // What it wrote loads: no starter is a file with a problem.
    let set = argonauts_core::settings::mods::ModSet::load(Some(&home.join("mods")), None);
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    assert!(set.get("checks").is_some());
}

#[tokio::test]
async fn guided_setup_rerun_preselects_your_values() {
    let mut fixture = Fixture::prepared(true, |repo| {
        std::fs::create_dir_all(repo.home()).unwrap();
        std::fs::write(repo.home().join("terms-note-shown"), "").unwrap();
        std::fs::write(
            repo.global_config(),
            "crew.main = \"codex/gpt-5.6-luna\"\nworkflow = \"council\"\n\n\
             [approvals.understand]\nmode = \"auto\"\n\n[approvals.merge]\nmode = \"supervised\"\n",
        )
        .unwrap();
    });
    // Your file says council, but the workflow step recommends Orchestrator every time.
    let logins = vec![("claude-code".to_owned(), LoginState::LoggedOut)];
    let (mut setup, effects) = start(&mut fixture, two_ready(), logins, Vec::new()).await;
    let (asked, _) = answer_all(&mut fixture, &mut setup, effects, enter).await;
    assert_eq!(asked[0].title, "Your agents", "the terms note shows once");
    let approvals = asked
        .iter()
        .find(|choice| choice.title == "Approvals")
        .expect("the approvals question");
    assert_eq!(approvals.options[0].label, "Keep them as they are");

    let text = fixture.global();
    let settings: argonauts_core::settings::Settings = toml::from_str(&text).unwrap();
    assert_eq!(settings.crew.main.to_string(), "codex/gpt-5.6-luna");
    assert_eq!(settings.workflow, "orchestrator");
    assert_eq!(
        settings.approvals.understand.mode,
        ApprovalMode::Auto,
        "{text}"
    );
    assert_eq!(
        settings.approvals.merge.mode,
        ApprovalMode::Supervised,
        "{text}"
    );
    assert!(!text.contains("[approvals.plan]"), "{text}");
}

#[tokio::test]
async fn guided_setup_esc_takes_the_recommendations_for_the_rest() {
    let mut fixture = Fixture::new(true);
    let (mut setup, _) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let terms = open_choice(&fixture.drain()).unwrap();
    setup
        .answer(&mut fixture.ctx, &terms.id, enter(&terms), None)
        .await
        .unwrap();
    let agents = open_choice(&fixture.drain()).unwrap();
    let effects = setup.set_aside(&mut fixture.ctx, &agents.id).await.unwrap();
    assert!(finished(&effects));
    let text = fixture.global();
    assert!(text.contains(r#"workflow = "orchestrator""#), "{text}");
    assert!(text.contains(r#"mode = "auto""#), "{text}");
    let outcome = fixture.drain().into_iter().find_map(|event| match event {
        AppEvent::ChoiceClosed { id, outcome } if id == agents.id => Some(outcome),
        _ => None,
    });
    let outcome = outcome.expect("the task closes");
    assert!(outcome.contains("recommended answers"), "{outcome}");
    assert!(
        outcome.contains("/setup") && outcome.contains("argonauts setup"),
        "{outcome}"
    );
}

/// The header printed before the setup, so it comes again with what you picked, ahead of the
/// lines that close the setup.
#[tokio::test]
async fn guided_setup_prints_the_header_again_with_the_new_agent_and_workflow() {
    let mut fixture = Fixture::new(true);
    let before = fixture.ctx.settings().crew.main.agent.clone();
    let agents = vec![
        agent("claude-code", "Claude Code", AgentStatus::NotInstalled),
        agent("codex", "Codex", AgentStatus::Tested),
        agent("opencode", "OpenCode", AgentStatus::NotInstalled),
    ];
    let (mut setup, _) = start(&mut fixture, agents, Vec::new(), Vec::new()).await;
    let terms = open_choice(&fixture.drain()).unwrap();
    setup
        .answer(&mut fixture.ctx, &terms.id, enter(&terms), None)
        .await
        .unwrap();
    let task = open_choice(&fixture.drain()).unwrap();
    setup.set_aside(&mut fixture.ctx, &task.id).await.unwrap();

    let events = fixture.drain();
    let at = |found: &dyn Fn(&AppEvent) -> bool| events.iter().position(found).unwrap();
    let closed = at(&|event| matches!(event, AppEvent::ChoiceClosed { .. }));
    let header = at(&|event| matches!(event, AppEvent::Welcome { .. }));
    let saved =
        at(&|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("Saved to")));
    assert!(closed < header && header < saved, "{events:#?}");
    let AppEvent::Welcome {
        main_agent,
        workflow,
        agents,
        ..
    } = &events[header]
    else {
        unreachable!()
    };
    assert_ne!(before, "codex");
    assert_eq!(main_agent.as_ref().unwrap().agent, "codex");
    assert_eq!(workflow, &fixture.ctx.settings().workflow);
    let codex = agents.iter().find(|agent| agent.id == "codex").unwrap();
    assert!(codex.ready, "{agents:?}");
}

#[tokio::test]
async fn guided_setup_leaves_a_logged_out_agent_out_of_the_crew() {
    let mut fixture = Fixture::new(true);
    let logins = vec![("codex".to_owned(), LoginState::LoggedOut)];
    let (mut setup, effects) = start(&mut fixture, two_ready(), logins, Vec::new()).await;
    let (asked, _) = answer_all(&mut fixture, &mut setup, effects, enter).await;
    let agents = asked
        .iter()
        .find(|choice| choice.title == "Your agents")
        .unwrap();
    assert!(
        agents
            .body
            .contains(&"Codex 1.0.0 · logged out · codex login".to_owned()),
        "{:?}",
        agents.body
    );
    let text = fixture.global();
    assert!(text.contains(r#"worker = ["claude-code"]"#), "{text}");
    assert!(text.contains(r#"workflow = "orchestrator""#), "{text}");
}

#[tokio::test]
async fn guided_setup_offers_a_local_servers_models_as_reviewers() {
    let mut fixture = Fixture::new(true);
    let server = LocalServer {
        id: "ollama".to_owned(),
        base_url: "http://127.0.0.1:9/v1".to_owned(),
        models: vec!["llama3.2".to_owned(), "qwen3".to_owned()],
    };
    let (mut setup, effects) = start(&mut fixture, two_ready(), Vec::new(), vec![server]).await;
    let (asked, _) = answer_all(&mut fixture, &mut setup, effects, |choice| {
        if choice.title.starts_with("A local model server") {
            let keys: Vec<char> = choice.options.iter().map(|option| option.key).collect();
            assert_eq!(keys, vec!['n', 'a', 'b']);
            choice.options[1].action.clone()
        } else {
            enter(choice)
        }
    })
    .await;
    assert!(
        asked
            .iter()
            .any(|choice| choice.title == "A local model server: Ollama")
    );
    let settings: argonauts_core::settings::Settings = toml::from_str(&fixture.global()).unwrap();
    let ollama = &settings.endpoints["ollama"];
    assert_eq!(ollama.base_url.as_deref(), Some("http://127.0.0.1:9/v1"));
    assert!(ollama.local);
    let reviewers: Vec<String> = settings.roles[&argonauts_core::names::RoleName::Reviewer]
        .models
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(reviewers, vec!["ollama/llama3.2"]);
}

#[tokio::test]
async fn guided_setup_asks_who_orchestrates_a_orchestrator() {
    let mut fixture = Fixture::new(true);
    let (mut setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let pick = |choice: &AppChoice| {
        if choice.title == "How the crew works" {
            let value = |option: &argonauts_core::events::approval::AnswerOption| {
                questions::value_of(&option.action)
            };
            let orchestrated = choice
                .options
                .iter()
                .find(|option| value(option).as_deref() == Some(steps::ORCHESTRATOR))
                .unwrap();
            orchestrated.action.clone()
        } else if choice.title == "Who orchestrates" {
            choice.options[1].action.clone()
        } else {
            enter(choice)
        }
    };
    let (asked, _) = answer_all(&mut fixture, &mut setup, effects, pick).await;
    assert!(
        asked
            .iter()
            .any(|choice| choice.title == "Who orchestrates")
    );
    let text = fixture.global();
    assert!(text.contains(r#"workflow = "orchestrator""#), "{text}");
    assert!(text.contains(r#"models = ["codex"]"#), "{text}");
}

#[tokio::test]
async fn who_does_what_sets_a_role_and_comes_back_until_you_keep_them_all() {
    let mut fixture = Fixture::new(true);
    let (mut setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let changed = std::cell::Cell::new(false);
    let value = |option: &argonauts_core::events::approval::AnswerOption| {
        questions::value_of(&option.action)
    };
    let pick = |choice: &AppChoice| {
        let chosen = |wanted: &str| {
            let option = choice
                .options
                .iter()
                .find(|option| value(option).as_deref() == Some(wanted))
                .unwrap_or_else(|| panic!("{wanted} isn't offered: {choice:?}"));
            option.action.clone()
        };
        match choice.title.as_str() {
            "Who does what" if !changed.replace(true) => chosen("roles.reviewer.models"),
            "Who is the reviewer" => chosen("codex"),
            _ => enter(choice),
        }
    };
    let (asked, _) = answer_all(&mut fixture, &mut setup, effects, pick).await;

    let titles: Vec<&str> = asked.iter().map(|choice| choice.title.as_str()).collect();
    let list = titles
        .iter()
        .filter(|title| **title == "Who does what")
        .count();
    assert_eq!(list, 2, "the list comes back after the change: {titles:?}");
    let first = asked
        .iter()
        .find(|choice| choice.title == "Who does what")
        .unwrap();
    let labels: Vec<&str> = first
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels[0], "Keep them all");
    for role in [
        "Main agent",
        "orchestrator",
        "reviewer",
        "narrator",
        "judge",
    ] {
        assert!(labels.contains(&role), "{labels:?}");
    }
    let reviewer = first
        .options
        .iter()
        .find(|option| option.label == "reviewer")
        .unwrap();
    assert_eq!(reviewer.detail.as_deref(), Some("anyone in your crew"));
    let again = asked
        .iter()
        .rfind(|choice| choice.title == "Who does what")
        .unwrap();
    let reviewer = again
        .options
        .iter()
        .find(|option| option.label == "reviewer")
        .unwrap();
    assert!(
        reviewer
            .detail
            .as_deref()
            .is_some_and(|now| now.contains("Codex")),
        "{reviewer:?}"
    );
    let text = fixture.global();
    assert!(text.contains("[roles.reviewer]"), "{text}");
    assert!(text.contains(r#"models = ["codex"#), "{text}");
}

#[tokio::test]
async fn guided_setup_with_no_agent_ready_still_finishes_and_says_what_to_install() {
    let mut fixture = Fixture::new(true);
    let agents = vec![
        agent("claude-code", "Claude Code", AgentStatus::NotInstalled),
        agent("codex", "Codex", AgentStatus::NotInstalled),
    ];
    let (mut setup, effects) = start(&mut fixture, agents, Vec::new(), Vec::new()).await;
    let (asked, seen) = answer_all(&mut fixture, &mut setup, effects, enter).await;
    assert!(!asked.iter().any(|choice| choice.title == "Who you talk to"));
    let text = fixture.global();
    assert!(!text.contains("crew ="), "{text}");
    assert!(!text.contains("crew.main"), "{text}");
    assert!(text.contains(r#"workflow = "orchestrator""#), "{text}");
    let warned = notices(&seen)
        .into_iter()
        .any(|line| line.contains("install claude-code"));
    assert!(warned, "{seen:?}");
}

#[tokio::test]
async fn guided_setup_refuses_an_answer_it_did_not_offer() {
    let mut fixture = Fixture::new(true);
    let (mut setup, _) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let terms = open_choice(&fixture.drain()).unwrap();
    let odd = AnswerAction::Choose {
        value: "everything".to_owned(),
    };
    assert!(
        setup
            .answer(&mut fixture.ctx, &terms.id, odd, None)
            .await
            .is_err()
    );
    assert!(setup.owns(&terms.id));
}

#[tokio::test]
async fn guided_setup_waits_for_the_login_checks_before_listing_agents() {
    let mut fixture = Fixture::prepared(true, |repo| {
        std::fs::create_dir_all(repo.home()).unwrap();
        std::fs::write(repo.home().join("terms-note-shown"), "").unwrap();
    });
    let agents = two_ready();
    let header = header_for(&agents);
    let (mut setup, effects) = Setup::begin_with(&mut fixture.ctx, agents, header, Probing::Fed)
        .await
        .unwrap();
    assert!(effects.is_empty());
    assert!(open_choice(&fixture.drain()).is_none());
    setup
        .on_update(&mut fixture.ctx, SetupUpdate::Logins(Vec::new()))
        .await
        .unwrap();
    let agents = open_choice(&fixture.drain()).unwrap();
    assert_eq!(agents.title, "Your agents");
    // The servers aren't back, so answering waits for them rather than skipping them.
    setup
        .answer(&mut fixture.ctx, &agents.id, enter(&agents), None)
        .await
        .unwrap();
    assert!(open_choice(&fixture.drain()).is_none());
    setup
        .on_update(&mut fixture.ctx, SetupUpdate::Servers(Vec::new()))
        .await
        .unwrap();
    assert_eq!(
        open_choice(&fixture.drain()).unwrap().title,
        "Who you talk to"
    );
}

#[tokio::test]
async fn guided_setup_trusts_the_folder_only_when_asked() {
    let mut fixture = Fixture::prepared(true, |repo| {
        repo.write(".argonauts/config.toml", "[ui]\nbell = false\n")
            .unwrap();
    });
    let (mut setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let (asked, _) = answer_all(&mut fixture, &mut setup, effects, |choice| {
        if choice
            .body
            .iter()
            .any(|line| line.contains("settings files"))
        {
            choice.options[1].action.clone()
        } else {
            enter(choice)
        }
    })
    .await;
    assert!(
        !asked.iter().any(|choice| choice
            .body
            .iter()
            .any(|line| line.contains("live in .argonauts"))),
        "the folder exists, so it isn't offered"
    );
    let project = fixture.ctx.env.project_config();
    assert!(fixture.ctx.settings.trust().was_trusted(&project));
}

#[tokio::test]
async fn guided_setup_leaves_a_file_that_needs_a_fix_untrusted_and_says_what_to_fix() {
    let mut fixture = Fixture::prepared(true, |repo| {
        repo.write(".argonauts/config.toml", "[ui]\nbell = false\n")
            .unwrap();
        repo.write(
            ".argonauts/config.local.toml",
            "[checks]\nrun = [\"cargo test\"]\n",
        )
        .unwrap();
    });
    let (mut setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let (_, seen) = answer_all(&mut fixture, &mut setup, effects, |choice| {
        if choice
            .body
            .iter()
            .any(|line| line.contains("settings files"))
        {
            choice.options[1].action.clone()
        } else {
            enter(choice)
        }
    })
    .await;
    let project = fixture.ctx.env.project_config();
    let local = fixture.ctx.env.project_local_config();
    assert!(fixture.ctx.settings.trust().was_trusted(&project));
    assert!(!fixture.ctx.settings.trust().was_trusted(&local));
    let notices = notices(&seen);
    assert!(
        notices.iter().any(|notice| notice.starts_with(
            ".argonauts/config.local.toml needs a fix before you can trust it: checks.run can't \
             be a list of strings"
        )),
        "{notices:#?}"
    );
    assert!(
        notices
            .iter()
            .all(|notice| !notice.contains("invalid type")),
        "{notices:#?}"
    );
}

/// The bottom line shows the mode the approvals step picked, before the setup writes it.
#[tokio::test]
async fn the_approvals_answer_shows_on_the_bottom_line_at_once() {
    let mut fixture = Fixture::new(true);
    let (mut setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let pick = |choice: &AppChoice| match choice.title.as_str() {
        "Approvals" => crate::choices::choose("autopilot"),
        _ => enter(choice),
    };
    let mut effects = effects;
    for _ in 0..20 {
        let choice = open_choice(&fixture.drain()).expect("a question");
        let approvals = choice.title == "Approvals";
        effects = setup
            .answer(&mut fixture.ctx, &choice.id, pick(&choice), None)
            .await
            .unwrap();
        if approvals {
            assert!(
                matches!(effects.first(), Some(Effect::Status)),
                "{effects:?}"
            );
            assert_eq!(setup.approval_mode(), Some(ApprovalMode::Autopilot));
            return;
        }
    }
    panic!("no approvals step: {effects:?}");
}

/// A repo with its own `.argonauts/` is asked once whether the guide pages go there too, and
/// the closing line names each page written.
#[tokio::test]
async fn the_guide_pages_go_into_a_repo_only_after_a_yes_asked_once() {
    let mut fixture = Fixture::prepared(true, |repo| {
        std::fs::create_dir_all(repo.root().join(".argonauts")).unwrap();
    });
    let (mut setup, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let yes = |choice: &AppChoice| match choice.title.as_str() {
        "Guide pages in this repo" => crate::choices::choose("yes"),
        _ => enter(choice),
    };
    let (asked, seen) = answer_all(&mut fixture, &mut setup, effects, yes).await;
    let guides = asked
        .iter()
        .find(|choice| choice.title == "Guide pages in this repo")
        .expect("the guide pages question");
    assert_eq!(guides.options[0].label, "Not here");
    let dot = fixture.repo.root().join(".argonauts");
    assert!(dot.join("README.md").is_file());
    assert!(dot.join("workflows/README.md").is_file());
    let closing = notices(&seen)
        .into_iter()
        .find(|text| text.starts_with("Saved to"))
        .expect("the closing line");
    assert!(
        closing.contains("Wrote .argonauts/README.md, "),
        "{closing}"
    );

    let (mut again, effects) = start(&mut fixture, two_ready(), Vec::new(), Vec::new()).await;
    let (asked, _) = answer_all(&mut fixture, &mut again, effects, enter).await;
    assert!(
        !asked
            .iter()
            .any(|choice| choice.title == "Guide pages in this repo"),
        "asked once"
    );
}
