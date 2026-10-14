//! A replay with the real binary and fake agents: `argonauts learned replay` runs a candidate
//! and its parent as supervised runs in isolated repos, which finish on their own with only the
//! replay's workers, scores each with the hidden tests, and takes the winner to the promotion
//! approval.

mod app_support;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use app_support::*;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{ApprovalEvent, ApprovalKind};
use argonauts_core::events::run::{Party, RunLifecycle, RunOutcome};
use argonauts_core::events::task::TaskEvent;
use argonauts_core::settings::Settings;
use argonauts_core::steps::task::ProcessChange;
use argonauts_core::{Paths, RepoId, RunId};
use argonauts_learning::evolution::{CandidateState, Evolution};
use argonauts_learning::replay::{Check, Entry, Proposal, ReplaySet, holdout_draw};
use argonauts_learning::{LearnedWorkflowRef, RunRecord, Store};
use argonauts_scenarios::scenario_file;

/// What both versions are asked. No fake rule but the candidate's build template answers it.
const TASK: &str = "Correct the package name people are told to install";
/// The candidate's build template; the fake worker fixes README.md only when it reads this.
const HOUSE_RULE: &str = "House rule: name the scoped package @acme/api in README.md.";

/// Replays run on the crew's one agent, once per task, and your rules promote a build prompt.
const CONFIG: &str = "\
crew = [\"claude-code\"]

[ui]
keys = \"default\"

[run_host]
wait_grace = \"1s\"

[evolution.replay]
runs_per_task = 1
sequential = false
use_subscriptions = true

[approvals.rules.workflow_promotion]
change_classes = [\"build_prompt\"]
";

const V1: &str = "name: docs-fix\nversion: 1\ndescription: Small docs fixes\nmode: captain\n\
                  match: { task_types: [docs] }\ncaptain: { understand_first: true }\n";

/// The app scenario's fakes, with one worker first that fixes README.md when its prompt
/// carries the candidate's build template.
fn fakes_file(scratch: &Path) -> anyhow::Result<PathBuf> {
    let base = std::fs::read_to_string(scenario_file(AGENTS))?;
    let result = "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\
                  \"session_id\":\"sess-worker-house\",\"result\":\"Fixed README.md.\",\
                  \"total_cost_usd\":0.001,\"usage\":{\"input_tokens\":12,\"output_tokens\":20},\
                  \"structured_output\":{\"status\":\"done\",\"summary\":\"the readme names the \
                  scoped package\",\"files_changed\":[\"README.md\"],\"tests_run\":[],\
                  \"concerns\":[],\"questions\":[],\"change_requests\":[],\"memory_proposals\":[]}}";
    // First, a rule that never matches but reads one line of stdin, which every later rule
    // then searches: stream-json input stays open, so reading it all would wait forever.
    let primer = "[[rule]]\nargv0 = \"claude\"\nprompt_contains = 'never in any prompt'\n\
                  stdin_prompt = \"line\"\n\n";
    let rule = format!(
        "{primer}[[rule]]\nargv0 = \"claude\"\nargs_contain = [\"acceptEdits\"]\n\
         prompt_contains = '{HOUSE_RULE}'\nstdin_prompt = \"line\"\n\
         [[rule.step]]\nemit = '{{\"type\":\"system\",\"subtype\":\"init\",\
         \"session_id\":\"sess-worker-house\",\"cwd\":\".\",\"tools\":[\"Read\"],\
         \"model\":\"claude-haiku-4-5\"}}'\n\
         [[rule.step]]\nwrite = {{ path = \"README.md\", content = \"# Test repo\\n\\nnpm install @acme/api\\n\" }}\n\
         [[rule.step]]\nemit = '{result}'\n\
         [[rule.step]]\nexit = 0\n\n"
    );
    let at = base
        .find("[[rule]]")
        .context("no rule in the app's fakes")?;
    let path = scratch.join("replay-agents.toml");
    std::fs::write(&path, format!("{}{rule}{}", &base[..at], &base[at..]))?;
    Ok(path)
}

/// `argonauts <args>` in the repo, killed and failed after `limit`: a replay that never
/// finishes would otherwise wait out its six hours.
fn run_cli_within(
    scenario: &argonauts_scenarios::Scenario,
    args: &[&str],
    limit: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(argonauts_testkit::argonauts_bin()?)
        .args(args)
        .current_dir(scenario.repo.root())
        .env_remove("ARGONAUTS_SESSION")
        .envs(scenario.envs())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let began = std::time::Instant::now();
    while child.try_wait()?.is_none() {
        if began.elapsed() > limit {
            child.kill()?;
            let output = child.wait_with_output()?;
            anyhow::bail!(
                "argonauts {} ran past {limit:?}: {}",
                args.join(" "),
                output_text(&output)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(child.wait_with_output()?)
}

/// Puts a task in the replay set: its hidden test kept in Argonauts Code's state, on the side
/// `holdout` asks for.
fn add_task(
    store: &Store,
    id: &str,
    base: &str,
    merged: &str,
    holdout: bool,
) -> anyhow::Result<()> {
    let replay = store.replay_dir();
    let test = replay.join("tests").join(id).join("tests/readme.sh");
    std::fs::create_dir_all(test.parent().context("a test folder")?)?;
    std::fs::write(&test, "grep -q '@acme/api' README.md\n")?;
    let proposal = Proposal {
        entry: Entry {
            id: id.to_owned(),
            run: RunId::new(id)?,
            task: TASK.to_owned(),
            task_type: "docs".to_owned(),
            base: base.to_owned(),
            merged: merged.to_owned(),
            tests: vec![test],
            test_command: vec!["sh".to_owned(), "tests/readme.sh".to_owned()],
            limit_usd: 1.0,
            holdout: false,
            accepted: time::OffsetDateTime::now_utc(),
        },
        checks: vec![Check {
            name: "fails_at_base".to_owned(),
            passed: true,
            detail: String::new(),
        }],
    };
    let path = replay.join("proposals").join(format!("{id}.json"));
    std::fs::create_dir_all(path.parent().context("a proposals folder")?)?;
    std::fs::write(&path, serde_json::to_string(&proposal)?)?;
    // New tasks split a quarter to the holdout under the default set sizes.
    let seed = (0u64..)
        .find(|&seed| holdout_draw(seed, 0.25) == holdout)
        .context("a seed")?;
    let entry = ReplaySet::open(store)?.accept(id, seed)?;
    anyhow::ensure!(entry.holdout == holdout, "{id} merged into the wrong side");
    Ok(())
}

#[test]
fn a_replay_finishes_on_its_own_and_reaches_promotion() -> anyhow::Result<()> {
    let fakes = tempfile::tempdir()?;
    let scenario = scenario_playing(&fakes_file(fakes.path())?)?;
    std::fs::write(scenario.repo.global_config(), CONFIG)?;
    let repo = &scenario.repo;
    repo.write("README.md", "# Test repo\n\nnpm install api\n")?;
    repo.write(".argonauts/learned/docs-fix/v1.yaml", V1)?;
    let base = repo.commit_all("a docs learned workflow")?;
    repo.write("README.md", "# Test repo\n\nnpm install @acme/api\n")?;
    repo.write("tests/readme.sh", "grep -q '@acme/api' README.md\n")?;
    let merged = repo.commit_all(TASK)?;

    let id = RepoId::for_repo(&repo.root().canonicalize()?)?;
    let store = Store::new(Paths::new(repo.home()).state_dir(&id), repo.root());
    add_task(&store, "run-001", &base, &merged, false)?;
    add_task(&store, "run-002", &base, &merged, true)?;
    let mut settings = Settings::default();
    settings.evolution.evolvable = vec!["prompts.build".to_owned()];
    let ran = RunRecord {
        run: RunId::new("run-003")?,
        at: time::OffsetDateTime::now_utc(),
        task_type: "docs".to_owned(),
        learned_workflow: Some(LearnedWorkflowRef {
            name: "docs-fix".to_owned(),
            version: 1,
        }),
        workflow: None,
        outcome: RunOutcome::Merged,
        stuck_tasks: 0,
        final_checks_passed: true,
        cost_usd: 0.01,
        minutes: 1.0,
        canary: false,
    };
    let proposal = ProcessChange {
        setting: "prompts.build".to_owned(),
        value: format!("\"{HOUSE_RULE}\""),
        why: "the worker never looked at the package name".to_owned(),
    };
    let evolution = Evolution::open(&store)?;
    evolution.after_run(&settings, &ran, Some(&proposal))?;
    let state = |id: &str| -> anyhow::Result<CandidateState> {
        Ok(evolution.candidate(id)?.context("the candidate")?.state)
    };
    assert!(matches!(
        state("docs-fix-v2")?,
        CandidateState::Replaying { .. }
    ));

    let output = run_cli_within(
        &scenario,
        &["learned", "replay"],
        std::time::Duration::from_secs(300),
    )?;
    let said = output_text(&output);
    assert!(output.status.success(), "{said}");
    assert!(said.contains("Replayed docs-fix-v2"), "{said}");

    // Four runs, each version on each task, each finished with nobody answering: the goal
    // approval went to your rules, and the branch was left, never merged.
    let replays: Vec<PathBuf> = run_dirs(&scenario)
        .into_iter()
        .filter(|dir| {
            std::fs::read_to_string(dir.join("start.json"))
                .ok()
                .and_then(|start| serde_json::from_str::<serde_json::Value>(&start).ok())
                .is_some_and(|start| start["replay"] == true)
        })
        .collect();
    assert_eq!(replays.len(), 4, "{said}");
    let replay_dir = store.replay_dir().to_string_lossy().into_owned();
    for dir in &replays {
        let events = argonauts_testkit::journal::events(dir)?;
        let goal = events.iter().any(|event| {
            matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Understand)
        });
        let by_rules = events.iter().any(|event| {
            matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Decided(decided))
                if decided.by == Party::Rules)
        });
        let left = events.iter().any(|event| {
            matches!(
                event.kind,
                RunEventKind::Run(RunLifecycle::Finished {
                    outcome: RunOutcome::BranchLeft,
                    ..
                })
            )
        });
        assert!(goal && by_rules && left, "{}", dir.display());
        // Its tasks keep the hidden test and the replay folder out of reach.
        let questions: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Task(TaskEvent::Created(spec)) => Some(spec),
                _ => None,
            })
            .collect();
        assert!(!questions.is_empty(), "{}", dir.display());
        for spec in questions {
            assert!(
                spec.hidden_paths
                    .iter()
                    .any(|path| path == "tests/readme.sh")
                    && spec.hidden_paths.contains(&replay_dir),
                "{:?}",
                spec.hidden_paths
            );
        }
    }
    assert_eq!(repo.head()?, merged, "a replay touched your branch");

    // Only the candidate's template fixed the readme, on the tuning task and the holdout, so
    // it went on to your rules, which promote a build prompt: its canary runs now.
    assert_eq!(
        state("docs-fix-v2")?,
        CandidateState::Canary { runs: 0 },
        "{said}"
    );
    assert!(evolution.spent_this_month(time::OffsetDateTime::now_utc())? > 0.0);
    Ok(())
}
