//! The guided setup: on the first start, again with `argonauts setup`, with Enter or Esc, with
//! a local model server, a logged-out CLI, or no CLI at all, and what it writes.

use crate::app_support;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use app_support::*;
use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind, UiCommand};
use argonauts_core::prompts::prompt_files;
use argonauts_core::runs::events::approval::{AnswerAction, AnswerOption};
use argonauts_core::settings::workflows::WORKFLOW_FILE_KEYS;
use argonauts_scenarios::{Scenario, past_the_guard_of, scenario_file};
use argonauts_testkit::{FakeKind, FakeModelServer, FakeRule, FakeScenario, FakeStep, Key, Pty};

/// The keys an Enter-through setup writes, and nothing else.
const SETUP_KEYS: [&str; 13] = [
    "crew.main",
    "crew.worker",
    "crew.reviewer",
    "crew.judge",
    "workflow",
    "approvals.understand.mode",
    "approvals.plan.mode",
    "approvals.merge.mode",
    "approvals.stuck.mode",
    "approvals.limit.mode",
    "approvals.change_request.mode",
    "approvals.autopilot",
    // The limits section ends on its default, which keeps its commented limits inside it.
    "limits.when_reached",
];

const CLOSING_COMMANDS: [&str; 7] = [
    "/model",
    "/effort",
    "/workflow",
    "/agents",
    "/approvals",
    "/permissions",
    "/config",
];

fn is_setup_step(choice: &AppChoice) -> bool {
    matches!(choice.kind, ChoiceKind::TermsNote | ChoiceKind::Init)
}

/// Starts the app with both local-server probes pointed at closed ports.
fn launch_setup(
    scenario: &Scenario,
    args: &[&str],
    env: &[(&str, OsString)],
) -> anyhow::Result<Pty> {
    let mut all: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    all.extend(no_local_servers()?);
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    launch_with(scenario, &all, env)
}

/// Answers every setup step, with the number of the option `pick` names by its key, or Enter
/// when it names none, until the closing line shows. Each step opens with the keys, so nothing
/// gives it focus first. Returns the steps in order.
fn walk_setup(
    scenario: &Scenario,
    pty: &mut Pty,
    pick: impl Fn(&AppChoice) -> Option<char>,
) -> anyhow::Result<Vec<AppChoice>> {
    let mut seen = Vec::new();
    loop {
        let next = wait_until("the next setup step or the closing line", || {
            if let Some(step) = open_choices(scenario)?.into_iter().find(is_setup_step) {
                return Ok(Some(Some(step)));
            }
            let text = pty.screen().text();
            // A first setup closes with the commands that change its answers; one over your
            // settings with its one `/setup` row.
            let closing =
                CLOSING_COMMANDS.iter().all(|c| text.contains(c)) || text.contains(" · saved to");
            let closed = !seen.is_empty() && closing;
            Ok(closed.then_some(None))
        })
        .with_context(|| format!("the screen was:\n{}", pty.screen().text()))?;
        let Some(step) = next else {
            return Ok(seen);
        };
        check_no_history_search(pty)?;
        // A step drops the keys pressed in its first moment, as every question does.
        past_the_guard_of(pty, &step.title)?;
        match pick(&step) {
            Some(key) => pty.press(Key::Char(number_of(&step.options, key)))?,
            None => pty.press(Key::Enter)?,
        }
        wait_choice_closed(scenario, &step.id)?;
        seen.push(step);
    }
}

fn enter_through(scenario: &Scenario, pty: &mut Pty) -> anyhow::Result<Vec<AppChoice>> {
    walk_setup(scenario, pty, |_| None)
}

/// What a setup option answers with, such as `codex` for the option the question labels Codex.
fn value_of(option: &AnswerOption) -> &str {
    match &option.action {
        AnswerAction::Choose { value } => value,
        _ => "",
    }
}

fn step_text(step: &AppChoice) -> String {
    let options: Vec<String> = step
        .options
        .iter()
        .map(|o| format!("{} {}", o.label, o.detail.clone().unwrap_or_default()))
        .collect();
    format!(
        "{}\n{}\n{}",
        step.title,
        step.body.join("\n"),
        options.join("\n")
    )
}

/// The global config holds exactly the recommended answers, each with its description.
fn assert_recommended_global(scenario: &Scenario) -> anyhow::Result<()> {
    let text = read(&scenario.repo.global_config())?;
    let entries = toml_entries(&text);
    let mut keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    keys.sort_unstable();
    let mut want = SETUP_KEYS.to_vec();
    want.sort_unstable();
    assert_eq!(keys, want, "{text}");
    for entry in &entries {
        assert!(entry.commented, "{} has no comment:\n{text}", entry.key);
    }
    let workers = toml_value(&text, "crew.worker").unwrap_or_default();
    for agent in ["claude-code", "codex", "antigravity-cli"] {
        assert!(
            workers.contains(agent),
            "{agent} isn't in the crew: {workers}"
        );
    }
    assert!(toml_value(&text, "crew.main").is_some_and(|v| v.contains("claude-code")));
    assert!(same_toml(
        &toml_value(&text, "workflow").unwrap_or_default(),
        "\"council\""
    ));
    for key in SETUP_KEYS.iter().filter(|k| k.ends_with(".mode")) {
        assert!(
            same_toml(&toml_value(&text, key).unwrap_or_default(), "\"auto\""),
            "{key}:\n{text}"
        );
    }
    assert!(
        same_toml(
            &toml_value(&text, "approvals.autopilot").unwrap_or_default(),
            "false"
        ),
        "{text}"
    );
    Ok(())
}

/// A workflow file as the setup writes it: the default named, the built-in's hash recorded, each
/// key it sets commented, and the protocol keys it leaves alone commented out at their defaults.
fn assert_commented_workflow(path: &Path) -> anyhow::Result<String> {
    let text = read(path)?;
    let comments: Vec<&str> = text
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .collect();
    assert!(
        comments.iter().any(|line| line.contains("default")),
        "{} doesn't name the default:\n{text}",
        path.display()
    );
    assert!(
        comments
            .iter()
            .any(|line| line.starts_with("# built-in sha256: ")),
        "{} doesn't record the built-in it came from:\n{text}",
        path.display()
    );
    // The keys that say what the workflow is, and its steps, are laid out as they read best.
    // Every setting the file carries has a comment.
    for entry in toml_entries(&text) {
        assert!(
            entry.commented || describes_the_workflow(&entry.key),
            "{} sets {} without a comment:\n{text}",
            path.display(),
            entry.key
        );
    }
    assert!(
        comments.iter().any(|line| line.starts_with("# rounds = ")),
        "{} doesn't show critique.rounds at its default:\n{text}",
        path.display()
    );
    Ok(text)
}

/// A key of a workflow file that says what the workflow is: its name, what it is for, how its
/// steps are followed, or one of its steps. Every other key in the file is a setting it carries.
fn describes_the_workflow(key: &str) -> bool {
    WORKFLOW_FILE_KEYS.iter().any(|own| {
        key == *own
            || key
                .strip_prefix(own)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

fn workflows_dir(scenario: &Scenario) -> std::path::PathBuf {
    scenario.repo.home().join("workflows")
}

#[test]
fn app_first_run_enter_through_writes_commented_global() -> anyhow::Result<()> {
    let scenario = first_run_scenario()?;
    let mut pty = launch_setup(&scenario, &[], &[])?;
    let steps = enter_through(&scenario, &mut pty)?;

    assert_eq!(steps[0].kind, ChoiceKind::TermsNote, "{steps:?}");
    for step in &steps[1..] {
        assert_eq!(step.kind, ChoiceKind::Init);
        assert!(
            step.options.first().is_some_and(|o| o.recommended),
            "the recommended answer isn't first: {step:?}"
        );
    }
    assert!(
        !events(&scenario)?.iter().any(|event| matches!(event,
            AppEvent::Choice { choice } if choice.kind == ChoiceKind::ConfirmSafetySetting)),
        "a setup answer asked for a safety confirm"
    );
    assert_recommended_global(&scenario)?;
    let text = pty.screen().text();
    for command in CLOSING_COMMANDS {
        assert!(
            text.contains(command),
            "the closing line doesn't name {command}:\n{text}"
        );
    }
    assert_commented_workflow(&workflows_dir(&scenario).join("council.toml"))?;
    let orchestrated =
        assert_commented_workflow(&workflows_dir(&scenario).join("orchestrator.toml"))?;
    assert!(
        same_toml(
            &toml_value(&orchestrated, "drafting.by").unwrap_or_default(),
            "\"orchestrator\""
        ),
        "{orchestrated}"
    );
    assert!(
        !scenario.repo.root().join(".argonauts").exists(),
        "the setup made .argonauts/ without being asked"
    );
    // Every prompt's template is left in yours beside their README, there to read and edit.
    let prompts = scenario.repo.home().join(prompt_files::PROMPTS_DIR);
    assert!(
        prompts.join(prompt_files::README_FILE).is_file(),
        "no prompt templates in {}",
        prompts.display()
    );
    scenario.shot(&pty, "app_first_run_enter_through_writes_commented_global")?;
    Ok(())
}

#[test]
fn app_first_run_esc_takes_defaults() -> anyhow::Result<()> {
    let scenario = first_run_scenario()?;
    let mut pty = launch_setup(&scenario, &[], &[])?;
    let terms = wait_shown_choice(&scenario, &pty, ChoiceKind::TermsNote)?;
    pty.press(Key::Enter)?;
    wait_choice_closed(&scenario, &terms.id)?;

    let agents = wait_shown_choice(&scenario, &pty, ChoiceKind::Init)?;
    pty.press(Key::Esc)?;
    wait_choice_closed(&scenario, &agents.id)?;
    assert!(
        commands(&scenario)?
            .iter()
            .any(|c| matches!(c, UiCommand::SetAside { .. })),
        "Esc didn't set the step aside"
    );
    wait_until("the global config", || {
        Ok(scenario.repo.global_config().exists().then_some(()))
    })?;
    assert!(
        open_choices(&scenario)?.iter().all(|c| !is_setup_step(c)),
        "a step still waits after Esc"
    );
    assert_recommended_global(&scenario)?;
    check_bottom_row(&pty)?;
    scenario.shot(&pty, "app_first_run_esc_takes_defaults")?;
    Ok(())
}

#[test]
fn app_first_run_offers_local_server() -> anyhow::Result<()> {
    // The setup takes a server for Ollama only when it answers in Ollama's own API.
    let server = FakeModelServer::start(FakeKind::Ollama, &["llama3.2"])?;
    let scenario = first_run_scenario()?;
    let args = [
        "--set".to_owned(),
        format!("endpoints.ollama.base_url={}", server.base_url()),
        "--set".to_owned(),
        format!("endpoints.lmstudio.base_url={}", closed_port_url()?),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut pty = launch_with(&scenario, &args, &[])?;

    let steps = walk_setup(&scenario, &mut pty, |step| {
        step.options
            .iter()
            .find(|o| o.label == "Llama3.2")
            .map(|o| o.key)
    })?;
    let offer = steps
        .iter()
        .find(|s| s.options.iter().any(|o| o.label == "Llama3.2"))
        .expect("the local server wasn't offered by the model's name");
    assert!(
        offer
            .options
            .first()
            .is_some_and(|o| o.key == 'n' && o.recommended),
        "Not now isn't the recommended first answer: {offer:?}"
    );

    let text = read(&scenario.repo.global_config())?;
    let base_url = toml_value(&text, "endpoints.ollama.base_url").unwrap_or_default();
    assert!(
        same_toml(&base_url, &format!("\"{}\"", server.base_url())),
        "{text}"
    );
    assert!(same_toml(
        &toml_value(&text, "endpoints.ollama.local").unwrap_or_default(),
        "true"
    ));
    assert!(same_toml(
        &toml_value(&text, "roles.reviewer.models").unwrap_or_default(),
        "[\"ollama/llama3.2\"]"
    ));
    let requests = server.requests();
    assert!(!requests.is_empty(), "the server was never probed");
    // Only reads: Ollama's tags, what it holds and each tag's details, or a model list.
    let reads_only = |method: &str, path: &str| match (method, path) {
        ("GET", "/api/tags" | "/api/ps") | ("POST", "/api/show") => true,
        ("GET", path) => path.ends_with("/models"),
        _ => false,
    };
    assert!(
        requests.iter().all(|r| reads_only(&r.method, &r.path)),
        "the setup did more than list models: {:?}",
        requests.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    scenario.shot(&pty, "app_first_run_offers_local_server")?;
    Ok(())
}

#[test]
fn app_first_run_shows_logged_out_codex() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut fakes = FakeScenario::load(&scenario_file(AGENTS))?;
    fakes.rules.insert(
        0,
        FakeRule::for_program("codex")
            .args(&["login", "status"])
            .step(FakeStep::Stderr("Not logged in".to_owned()))
            .step(FakeStep::Exit(1)),
    );
    let logged_out = dir.path().join("logged-out-codex.toml");
    fakes.save(&logged_out)?;
    let scenario = scenario_playing(&logged_out)?;

    let mut pty = launch_setup(&scenario, &[], &[])?;
    let steps = enter_through(&scenario, &mut pty)?;
    let agents = steps
        .iter()
        .find(|s| s.kind == ChoiceKind::Init)
        .expect("no agents step");
    let said = step_text(agents);
    assert!(said.contains("codex login"), "{said}");
    assert!(said.to_lowercase().contains("logged out"), "{said}");
    let screen = pty.screen().text();
    assert!(screen.contains("codex login"), "{screen}");
    let checked = invocations_of(&scenario, "codex")?
        .into_iter()
        .any(|i| has_words(&i.args, &["login", "status"]));
    assert!(
        checked,
        "the setup guessed Codex's login instead of asking it"
    );
    scenario.shot(&pty, "app_first_run_shows_logged_out_codex")?;
    Ok(())
}

#[test]
fn app_first_run_without_clis_still_finishes() -> anyhow::Result<()> {
    let scenario = first_run_scenario()?;
    // Only git, so no agent CLI can be found anywhere.
    let tools = scenario.repo.scratch().join("only-git");
    std::fs::create_dir_all(&tools)?;
    let git = Command::new("/usr/bin/which").arg("git").output()?;
    let git = String::from_utf8(git.stdout)?.trim().to_owned();
    anyhow::ensure!(!git.is_empty(), "no git on PATH");
    std::os::unix::fs::symlink(&git, tools.join("git")).context("linking git")?;
    let mut path = OsString::from(tools.as_os_str());
    path.push(":/bin");

    let mut pty = launch_setup(&scenario, &[], &[("PATH", path)])?;
    enter_through(&scenario, &mut pty)?;
    let text = read(&scenario.repo.global_config())?;
    assert!(
        toml_value(&text, "crew.worker").is_none(),
        "a crew without agents:\n{text}"
    );
    let screen = pty.screen().text();
    assert!(screen.to_lowercase().contains("install"), "{screen}");
    assert!(
        scenario.clis.invocations()?.is_empty(),
        "an agent CLI was found through the fakes"
    );
    check_bottom_row(&pty)?;
    scenario.shot(&pty, "app_first_run_without_clis_still_finishes")?;
    Ok(())
}

#[test]
fn app_second_start_skips_setup() -> anyhow::Result<()> {
    let scenario = first_run_scenario()?;
    let mut pty = launch_setup(&scenario, &[], &[])?;
    enter_through(&scenario, &mut pty)?;
    quit(&mut pty)?;

    rotate_trace(&scenario, "first")?;
    let config = read(&scenario.repo.global_config())?;
    let pty = launch_setup(&scenario, &[], &[])?;
    wait_ready(&scenario, &pty)?;
    let steps = events(&scenario)?
        .into_iter()
        .filter(|event| matches!(event, AppEvent::Choice { choice } if is_setup_step(choice)))
        .count();
    assert_eq!(steps, 0, "the setup opened again");
    assert_eq!(read(&scenario.repo.global_config())?, config);
    scenario.shot(&pty, "app_second_start_skips_setup")?;
    Ok(())
}

#[test]
fn app_setup_again_keeps_your_choices_and_workflows() -> anyhow::Result<()> {
    let scenario = first_run_scenario()?;
    let mut pty = launch_setup(&scenario, &[], &[])?;
    // Not the recommendations: Codex to talk to, Captain, and auto preview approvals.
    walk_setup(&scenario, &mut pty, |step| {
        let has = |value: &str| step.options.iter().any(|o| value_of(o) == value);
        let key_of = |value: &str| {
            step.options
                .iter()
                .find(|o| value_of(o) == value)
                .map(|o| o.key)
        };
        if has("claude-code") && has("codex") {
            key_of("codex")
        } else if has("council") {
            key_of("captain")
        } else if has("auto_preview") {
            key_of("auto_preview")
        } else {
            None
        }
    })?;
    quit(&mut pty)?;

    let council = workflows_dir(&scenario).join("council.toml");
    let orchestrated = workflows_dir(&scenario).join("orchestrator.toml");
    let edited = format!("{}# my own change\n", read(&council)?);
    std::fs::write(&council, &edited)?;
    std::fs::remove_file(&orchestrated)?;

    rotate_trace(&scenario, "first")?;
    let mut pty = launch_setup(&scenario, &["setup"], &[])?;
    let steps = enter_through(&scenario, &mut pty)?;
    let main_agent_step = steps
        .iter()
        .find(|s| s.options.iter().any(|o| value_of(o) == "claude-code"))
        .expect("no main agent step");
    assert_eq!(
        value_of(&main_agent_step.options[0]),
        "codex",
        "your choice isn't preselected: {main_agent_step:?}"
    );

    let text = read(&scenario.repo.global_config())?;
    assert!(
        toml_value(&text, "crew.main").is_some_and(|v| v.contains("codex")),
        "{text}"
    );
    // The workflow step recommends Council every time, so Enter through it picks that.
    assert!(
        same_toml(
            &toml_value(&text, "workflow").unwrap_or_default(),
            "\"council\""
        ),
        "{text}"
    );
    assert!(
        same_toml(
            &toml_value(&text, "approvals.merge.mode").unwrap_or_default(),
            "\"auto_preview\""
        ),
        "{text}"
    );
    assert_eq!(
        read(&council)?,
        edited,
        "setup replaced your edited workflow"
    );
    assert_commented_workflow(&orchestrated)?;
    scenario.shot(&pty, "app_setup_again_keeps_your_choices_and_workflows")?;
    Ok(())
}

#[test]
fn app_setup_shows_terms_note_once() -> anyhow::Result<()> {
    let scenario = first_run_scenario()?;
    let mut pty = launch_setup(&scenario, &[], &[])?;
    let first = enter_through(&scenario, &mut pty)?;
    assert_eq!(first[0].kind, ChoiceKind::TermsNote);
    assert!(scenario.repo.home().join("terms-note-shown").exists());
    quit(&mut pty)?;

    rotate_trace(&scenario, "first")?;
    let mut pty = launch_setup(&scenario, &["setup"], &[])?;
    let again = enter_through(&scenario, &mut pty)?;
    assert!(!again.is_empty(), "init showed no steps");
    assert!(
        again.iter().all(|step| step.kind == ChoiceKind::Init),
        "the terms note showed twice: {again:?}"
    );
    scenario.shot(&pty, "app_setup_shows_terms_note_once")?;
    Ok(())
}

#[test]
fn app_setup_writes_council_and_its_file_sets_no_key() -> anyhow::Result<()> {
    let scenario = first_run_scenario()?;
    let mut pty = launch_setup(&scenario, &["setup"], &[])?;
    enter_through(&scenario, &mut pty)?;

    let text = read(&scenario.repo.global_config())?;
    assert!(
        same_toml(
            &toml_value(&text, "workflow").unwrap_or_default(),
            "\"council\""
        ),
        "{text}"
    );
    // The built-in settings are council's, so beside its own steps its file sets no key that
    // would hide your own.
    let workflow = assert_commented_workflow(&workflows_dir(&scenario).join("council.toml"))?;
    let settings: Vec<String> = toml_entries(&workflow)
        .into_iter()
        .map(|entry| entry.key)
        .filter(|key| !describes_the_workflow(key))
        .collect();
    assert!(
        settings.is_empty(),
        "the council workflow sets {settings:?} over your settings:\n{workflow}"
    );
    // Council talks in its phases, so it has no steps for an orchestrator to adapt.
    assert!(
        toml_value(&workflow, "adapt").is_none(),
        "the council workflow has no steps to adapt:\n{workflow}"
    );
    assert!(workflow.contains("kind  = \"talk\""), "{workflow}");
    scenario.shot(&pty, "app_setup_writes_council")?;
    Ok(())
}
