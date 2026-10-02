//! The replay set: isolated repositories, the checks a task passes to join, the seeded split,
//! the holdout's retirement, and hidden tests that never reach an agent's repository.

mod replay_support;

use std::sync::Arc;

use argonauts_core::limits::GateRunner;
use argonauts_core::settings::evolution::EntryAcceptance;
use argonauts_learning::LearningError;
use argonauts_learning::replay::{SplitReason, holdout_draw, isolated_repo, score};

use replay_support::{
    Fixture, IDENTITY, RUN_SPEND_USD, ShellGates, files_under, git_in, holdout_share, seed_for,
    settings,
};

#[test]
fn replay_repo_history_stops_at_base() {
    let mut fixture = Fixture::new();
    let task = fixture.task("alpha");
    fixture.repo.git(&["tag", "v1", &task.merged]).unwrap();
    fixture
        .repo
        .git(&["branch", "later", &task.merged])
        .unwrap();
    let source = fixture.repo.root();
    let source_refs = git_in(source, &["for-each-ref"]);
    let into = fixture.set.repos_dir().join("alpha");

    let repo = isolated_repo(source, &task.base, &into).unwrap();

    assert_eq!(
        git_in(&repo, &["rev-list", "--all"]),
        git_in(source, &["rev-list", &task.base])
    );
    assert!(
        git_in(&repo, &["rev-list", "--all", "--reflog"])
            .lines()
            .all(|sha| sha != task.merged)
    );
    assert!(
        argonauts_learning::git::run(
            &repo,
            &["cat-file", "-e", &format!("{}^{{commit}}", task.merged)]
        )
        .is_err(),
        "the solving commit must not be in the replay repo"
    );
    assert_eq!(
        git_in(&repo, &["for-each-ref", "--format=%(refname)"]),
        "refs/heads/main"
    );
    assert_eq!(git_in(&repo, &["remote"]), "");
    assert_eq!(git_in(&repo, &["reflog", "show", "--all"]), "");
    assert!(!repo.join(".git/FETCH_HEAD").exists());
    assert_eq!(git_in(&repo, &["rev-parse", "HEAD"]), task.base);
    assert_eq!(git_in(&repo, &["status", "--porcelain"]), "");
    assert!(!repo.join(&task.test).exists());

    assert_eq!(git_in(source, &["for-each-ref"]), source_refs);
    assert_eq!(git_in(source, &["rev-parse", "HEAD"]), task.merged);
    assert_eq!(git_in(source, &["status", "--porcelain"]), "");
    let leftovers: Vec<String> = files_under(&fixture.set.repos_dir())
        .into_iter()
        .filter(|file| !file.starts_with("alpha/"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let refused = isolated_repo(source, &task.base, &into).unwrap_err();
    assert!(matches!(refused, LearningError::Refused(_)), "{refused}");
}

#[tokio::test]
async fn replay_entry_needs_fail_then_pass() {
    let mut fixture = Fixture::new();
    let good = fixture.task("alpha");
    let passes_at_base = fixture.task_with(
        "Add the beta feature",
        &[("src/beta.txt", "beta ready\n")],
        "tests/beta.sh",
        "test -f README.md\n",
    );
    let fails_when_merged = fixture.task_with(
        "Add the gamma feature",
        &[("src/gamma.txt", "gamma ready\n")],
        "tests/gamma.sh",
        "test -f src/missing.txt\n",
    );
    let settings = settings();
    let gates = Arc::new(ShellGates::default());

    let accepted = fixture.consider(&good, &settings, &gates).await.unwrap();
    assert!(accepted.valid(), "{:?}", accepted.checks);
    let entries = fixture.set.entries().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.id, good.id);
    assert_eq!(entry.task, good.request);
    assert_eq!(entry.task_type, "feature");
    assert_eq!(
        (entry.base.as_str(), entry.merged.as_str()),
        (good.base.as_str(), good.merged.as_str())
    );
    assert_eq!(entry.test_command, ["sh", "tests/alpha.sh"]);
    assert_eq!(entry.budget_usd, RUN_SPEND_USD);
    assert_eq!(
        entry.tests,
        [fixture
            .replay_dir()
            .join("tests")
            .join(&good.id)
            .join("tests/alpha.sh")]
    );
    assert!(fixture.set.proposals().unwrap().is_empty());

    for (task, failing) in [
        (&passes_at_base, "fails_at_base"),
        (&fails_when_merged, "passes_when_merged"),
    ] {
        let proposal = fixture.consider(task, &settings, &gates).await.unwrap();
        assert!(!proposal.valid());
        let check = proposal
            .checks
            .iter()
            .find(|check| check.name == failing)
            .unwrap();
        assert!(!check.passed, "{check:?}");
        let refused = fixture.set.accept(&task.id, 7).unwrap_err();
        assert!(refused.to_string().contains(failing), "{refused}");
    }
    assert_eq!(fixture.set.entries().unwrap().len(), 1);
    assert_eq!(fixture.set.proposals().unwrap().len(), 2);

    let mut asking = settings.clone();
    asking.evolution.replay.accept_entries = EntryAcceptance::Ask;
    let delta = fixture.task("delta");
    let waiting = fixture.consider(&delta, &asking, &gates).await.unwrap();
    assert!(waiting.valid(), "{:?}", waiting.checks);
    assert_eq!(fixture.set.entries().unwrap().len(), 1);
    assert!(
        fixture
            .set
            .proposals()
            .unwrap()
            .iter()
            .any(|proposal| proposal.entry.id == delta.id)
    );
    fixture.set.accept(&delta.id, 7).unwrap();
    assert_eq!(fixture.set.entries().unwrap().len(), 2);

    assert!(files_under(&fixture.replay_dir().join("work")).is_empty());
    assert!(fixture.consider(&good, &settings, &gates).await.is_none());
    for (request, _) in gates.requests() {
        assert!(
            request
                .workdir
                .starts_with(fixture.replay_dir().join("work"))
        );
        assert_eq!(request.sandbox.deny_read, [fixture.replay_dir()]);
        assert!(!request.sandbox.network);
    }
}

#[tokio::test]
async fn replay_entry_refuses_unseen_names() {
    let mut fixture = Fixture::new();
    fixture.repo.write("src/lib.txt", "numbers\n").unwrap();
    fixture.repo.commit_all("a library").unwrap();
    let named_by_solution = fixture.task_with(
        "Make the library double numbers",
        &[("src/lib.txt", "numbers\ntwice\n")],
        "tests/double.sh",
        "grep -q twice src/lib.txt\n",
    );
    let file_by_solution = fixture.task_with(
        "Add a config loader",
        &[("src/loader_conf.json", "{}\n")],
        "tests/loader.sh",
        "test -f src/loader_conf.json\n",
    );
    let named_by_request = fixture.task_with(
        "Add thrice to the library",
        &[("src/lib.txt", "numbers\ntwice\nthrice\n")],
        "tests/triple.sh",
        "grep -q thrice src/lib.txt\n",
    );
    let settings = settings();
    let gates = Arc::new(ShellGates::default());

    for (task, unseen) in [
        (&named_by_solution, "twice"),
        (&file_by_solution, "src/loader_conf.json"),
    ] {
        let proposal = fixture.consider(task, &settings, &gates).await.unwrap();
        let check = proposal
            .checks
            .iter()
            .find(|check| check.name == "needs_only_given_names")
            .unwrap();
        assert!(!check.passed, "{check:?}");
        assert!(check.detail.contains(unseen), "{check:?}");
        assert!(
            proposal
                .checks
                .iter()
                .filter(|check| check.name != "needs_only_given_names")
                .all(|check| check.passed)
        );
    }

    let proposal = fixture
        .consider(&named_by_request, &settings, &gates)
        .await
        .unwrap();
    assert!(proposal.valid(), "{:?}", proposal.checks);
    let ids: Vec<String> = fixture
        .set
        .entries()
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids, std::slice::from_ref(&named_by_request.id));
}

#[tokio::test]
async fn replay_split_is_seeded_and_recorded() {
    let mut fixture = Fixture::new();
    let tasks: Vec<_> = ["alpha", "beta", "gamma", "delta"]
        .map(|name| fixture.task(name))
        .to_vec();
    let mut settings = settings();
    settings.evolution.replay.accept_entries = EntryAcceptance::Ask;
    settings.evolution.replay.holdout_tasks = 2;
    let share = holdout_share(&settings);
    let gates = Arc::new(ShellGates::default());
    for task in &tasks {
        fixture.consider(task, &settings, &gates).await.unwrap();
    }

    let holdout_seed = seed_for(true, share);
    let tuning_seed = seed_for(false, share);
    let later_holdout_seed = (holdout_seed + 1..)
        .find(|&seed| holdout_draw(seed, share))
        .unwrap();
    let seeds = [holdout_seed, tuning_seed, later_holdout_seed, holdout_seed];
    let entries: Vec<_> = tasks
        .iter()
        .zip(seeds)
        .map(|(task, seed)| fixture.set.accept(&task.id, seed).unwrap())
        .collect();

    let sides: Vec<bool> = entries.iter().map(|entry| entry.holdout).collect();
    assert_eq!(sides, [true, false, true, false], "the holdout keeps 2");
    let records = fixture.set.splits().unwrap();
    assert_eq!(records.len(), 4);
    for ((record, entry), seed) in records.iter().zip(&entries).zip(seeds) {
        assert_eq!(record.entry, entry.id);
        assert_eq!(record.seed, Some(seed));
        assert_eq!(record.holdout, entry.holdout);
        assert_eq!(record.generation, 0);
    }
    assert_eq!(records[3].reason, SplitReason::HoldoutFull);
    for record in &records[..3] {
        assert_eq!(record.reason, SplitReason::Drawn);
        assert_eq!(holdout_draw(record.seed.unwrap(), share), record.holdout);
    }

    let lists = |name: &str| -> Vec<String> {
        serde_json::from_slice(&std::fs::read(fixture.replay_dir().join(name)).unwrap()).unwrap()
    };
    assert_eq!(
        lists("holdout.json"),
        [tasks[0].id.clone(), tasks[2].id.clone()]
    );
    assert_eq!(
        lists("tuning.json"),
        [tasks[1].id.clone(), tasks[3].id.clone()]
    );

    let mut again = Fixture::new();
    let replayed: Vec<_> = ["alpha", "beta", "gamma", "delta"]
        .map(|name| again.task(name))
        .to_vec();
    for task in &replayed {
        again.consider(task, &settings, &gates).await.unwrap();
    }
    let redrawn: Vec<bool> = replayed
        .iter()
        .zip(seeds)
        .map(|(task, seed)| again.set.accept(&task.id, seed).unwrap().holdout)
        .collect();
    assert_eq!(redrawn, sides, "the same seeds draw the same split");
}

#[tokio::test]
async fn holdout_retires_after_its_candidates() {
    let mut fixture = Fixture::new();
    let tasks: Vec<_> = ["alpha", "beta", "gamma", "delta"]
        .map(|name| fixture.task(name))
        .to_vec();
    let mut settings = settings();
    settings.evolution.replay.holdout_retire_after = 2;
    fixture
        .accept(
            &[(&tasks[0], true), (&tasks[1], true), (&tasks[2], false)],
            &settings,
        )
        .await;
    let holdout_ids = |fixture: &Fixture| -> Vec<String> {
        fixture
            .set
            .holdout()
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect()
    };
    assert_eq!(
        holdout_ids(&fixture),
        [tasks[0].id.clone(), tasks[1].id.clone()]
    );

    fixture.set.holdout_used(&settings).unwrap();
    assert_eq!(fixture.set.holdout_candidates().unwrap(), 1);
    assert_eq!(holdout_ids(&fixture).len(), 2);

    fixture.set.holdout_used(&settings).unwrap();
    assert!(holdout_ids(&fixture).is_empty());
    assert_eq!(fixture.set.holdout_candidates().unwrap(), 0);
    assert_eq!(fixture.set.tuning().unwrap().len(), 3);
    let retired: Vec<_> = fixture
        .set
        .splits()
        .unwrap()
        .into_iter()
        .filter(|record| record.reason == SplitReason::Retired)
        .collect();
    assert_eq!(retired.len(), 2);
    assert!(
        retired
            .iter()
            .all(|record| !record.holdout && record.generation == 0)
    );

    let fresh = fixture.accept(&[(&tasks[3], true)], &settings).await;
    assert!(fresh[0].holdout);
    assert_eq!(holdout_ids(&fixture), [tasks[3].id.clone()]);
    assert_eq!(fixture.set.splits().unwrap().last().unwrap().generation, 1);
}

#[tokio::test]
async fn hidden_tests_never_reach_a_worktree() {
    let mut fixture = Fixture::new();
    let task = fixture.task("alpha");
    let mut settings = settings();
    settings.evolution.replay.holdout_tasks = 0;
    let entry = fixture.accept(&[(&task, false)], &settings).await.remove(0);
    let gates = Arc::new(ShellGates::default());
    let runner_gates: Arc<dyn GateRunner> = gates.clone();

    let agent_repo = isolated_repo(
        fixture.repo.root(),
        &task.base,
        &fixture.set.repos_dir().join("agent"),
    )
    .unwrap();
    std::fs::create_dir_all(agent_repo.join("src")).unwrap();
    std::fs::write(agent_repo.join("src/alpha.txt"), "alpha ready\n").unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), agent_repo.join("tests")).unwrap();
    git_in(&agent_repo, &["add", "-A"]);
    argonauts_learning::git::run_with(
        &agent_repo,
        &["commit", "-q", "-m", "solve it"],
        &IDENTITY,
        None,
    )
    .unwrap();
    let head = git_in(&agent_repo, &["rev-parse", "HEAD"]);

    assert!(
        score(&entry, &agent_repo, &head, runner_gates.clone())
            .await
            .unwrap()
    );

    assert!(
        files_under(outside.path()).is_empty(),
        "a planted link carried a test out"
    );
    assert!(!agent_repo.join("tests/alpha.sh").exists());
    assert_eq!(git_in(&agent_repo, &["status", "--porcelain"]), "");
    assert_eq!(git_in(&agent_repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git_in(&agent_repo, &["for-each-ref", "--format=%(refname)"]),
        "refs/heads/main"
    );
    let ever_committed = git_in(&agent_repo, &["log", "--all", "--name-only", "--format="]);
    assert!(!ever_committed.contains("alpha.sh"), "{ever_committed}");

    let (request, files) = gates.requests().pop().unwrap();
    assert!(
        request
            .workdir
            .starts_with(fixture.replay_dir().join("work"))
    );
    assert!(!request.workdir.starts_with(&agent_repo));
    assert!(files.contains(&"tests/alpha.sh".to_owned()), "{files:?}");
    assert!(!request.workdir.exists(), "the scoring checkout is deleted");
    assert!(files_under(&fixture.replay_dir().join("work")).is_empty());
    assert_eq!(request.sandbox.deny_read, [fixture.replay_dir()]);
    assert_eq!(
        request.sandbox.readable,
        std::slice::from_ref(&request.workdir)
    );

    let mut unsolved = agent_repo.clone();
    unsolved.set_file_name("unsolved");
    let unsolved = isolated_repo(fixture.repo.root(), &task.base, &unsolved).unwrap();
    assert!(
        !score(&entry, &unsolved, &task.base, runner_gates)
            .await
            .unwrap()
    );
    assert!(!unsolved.join("tests/alpha.sh").exists());
    assert!(files_under(&fixture.replay_dir().join("work")).is_empty());
}
