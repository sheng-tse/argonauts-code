use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use argonauts_testkit::scenario::{
    FakeRule, FakeScenario, FakeStep, LogLine, NO_RULE_EXIT, StdinPrompt,
};
use argonauts_testkit::{FakeClis, TempRepo};

const FAKE: &str = env!("CARGO_BIN_EXE_argonauts-fake-cli");

fn install(repo: &TempRepo, scenario: FakeScenario) -> FakeClis {
    let path = repo.scratch().join("scenario.toml");
    scenario.save(&path).unwrap();
    FakeClis::install_with(Path::new(FAKE), repo.scratch(), &path).unwrap()
}

fn run(
    clis: &FakeClis,
    repo: &TempRepo,
    program: &str,
    args: &[&str],
    stdin: &str,
) -> (i32, String, String) {
    let mut child = Command::new(clis.bin_dir().join(program))
        .args(args)
        .current_dir(repo.root())
        .envs(clis.envs())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn plays_the_first_matching_rule_and_logs_the_invocation() {
    let repo = TempRepo::new().unwrap();
    std::fs::write(
        repo.scratch().join("recorded.jsonl"),
        "{\"a\":1}\n{\"b\":2}\n",
    )
    .unwrap();
    let scenario = FakeScenario::default()
        .version("claude", "2.1.284 (Claude Code)")
        .rule(
            FakeRule::for_program("codex")
                .prompt("never")
                .step(FakeStep::Exit(9)),
        )
        .rule(
            FakeRule::for_program("codex")
                .args(&["exec"])
                .prompt("hello")
                .stdin(StdinPrompt::All)
                .step(FakeStep::Emit("{\"type\":\"thread.started\"}".into()))
                .step(FakeStep::EmitFile("recorded.jsonl".into()))
                .step(FakeStep::Write {
                    path: "hello.txt".into(),
                    content: "hi\n".into(),
                })
                .step(FakeStep::Stderr("warming up".into()))
                .step(FakeStep::Exit(0)),
        );
    let clis = install(&repo, scenario);

    let (code, stdout, stderr) = run(
        &clis,
        &repo,
        "codex",
        &["exec", "--json", "-"],
        "say hello\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout,
        "{\"type\":\"thread.started\"}\n{\"a\":1}\n{\"b\":2}\n"
    );
    assert_eq!(stderr.trim(), "warming up");
    assert_eq!(
        std::fs::read_to_string(repo.root().join("hello.txt")).unwrap(),
        "hi\n"
    );

    let (code, stdout, _) = run(&clis, &repo, "claude", &["--version"], "");
    assert_eq!((code, stdout.trim()), (0, "2.1.284 (Claude Code)"));

    let (code, _, stderr) = run(&clis, &repo, "agy", &["-p", "x"], "");
    assert_eq!(code, NO_RULE_EXIT);
    assert!(stderr.contains("no rule matched agy"));

    let invocations = clis.invocations().unwrap();
    assert_eq!(invocations.len(), 3);
    assert_eq!(invocations[0].argv0, "codex");
    assert_eq!(invocations[0].args, ["exec", "--json", "-"]);
    assert_eq!(invocations[0].rule, Some(1));
    assert_eq!(invocations[0].stdin.as_deref(), Some("say hello\n"));
    assert!(invocations[0].env.iter().any(|name| name == "PATH"));
    assert_eq!(invocations[0].cwd, repo.root());
    assert_eq!(invocations[2].rule, None);
}

#[test]
fn reads_stdin_during_the_script_and_refuses_to_write_outside() {
    let repo = TempRepo::new().unwrap();
    let scenario = FakeScenario::default()
        .rule(
            FakeRule::for_program("claude")
                .args(&["--input-format"])
                .step(FakeStep::ReadStdinLine(true))
                .step(FakeStep::ExpectStdinContains("wrap up".into()))
                .step(FakeStep::Emit("done".into())),
        )
        .rule(FakeRule::for_program("agy").step(FakeStep::Write {
            path: "../escape.txt".into(),
            content: "no".into(),
        }));
    let clis = install(&repo, scenario);
    let (code, stdout, _) = run(
        &clis,
        &repo,
        "claude",
        &["--input-format", "stream-json"],
        "first\nsecond\nplease wrap up now\n",
    );
    assert_eq!((code, stdout.as_str()), (0, "done\n"));
    let stdin_lines: Vec<String> = clis
        .log()
        .unwrap()
        .into_iter()
        .filter_map(|line| match line {
            LogLine::StdinLine { line, .. } => Some(line),
            _ => None,
        })
        .collect();
    assert_eq!(stdin_lines, ["first", "second", "please wrap up now"]);

    let (code, _, stderr) = run(&clis, &repo, "agy", &[], "");
    assert_ne!(code, 0);
    assert!(stderr.contains("outside the working directory"), "{stderr}");
    assert!(!repo.scratch().join("escape.txt").exists());
}

#[test]
fn answers_help_with_the_flags_the_doctor_looks_for() {
    let repo = TempRepo::new().unwrap();
    let clis = install(&repo, FakeScenario::default());
    let (code, help, _) = run(&clis, &repo, "codex", &["exec", "--help"], "");
    assert_eq!(code, 0);
    for flag in ["--json", "--output-schema", "--skip-git-repo-check"] {
        assert!(help.contains(flag), "{flag} in {help}");
    }
    let (code, help, _) = run(&clis, &repo, "claude", &["--help"], "");
    assert_eq!(code, 0);
    assert!(help.contains("--output-format"), "{help}");
    let (code, _, _) = run(&clis, &repo, "claude", &["-p", "--help"], "");
    assert_eq!(code, NO_RULE_EXIT);
}

/// A fake waiting for a signal goes on once its test gives it, and stops once that test has
/// ended and taken its folder with it, so no fake waits on after its test.
#[test]
fn a_signal_lets_a_fake_go_on_and_an_ended_test_stops_it() {
    let repo = TempRepo::new().unwrap();
    let scenario = FakeScenario::default().rule(
        FakeRule::for_program("codex")
            .step(FakeStep::AwaitSignal("go".into()))
            .step(FakeStep::Emit("{\"went\":true}".into()))
            .step(FakeStep::Exit(0)),
    );
    let path = repo.scratch().join("scenario.toml");
    scenario.save(&path).unwrap();
    let fakes = repo.scratch().join("fakes");
    let clis = FakeClis::install_with(Path::new(FAKE), &fakes, &path).unwrap();
    let start = || {
        Command::new(clis.bin_dir().join("codex"))
            .current_dir(repo.root())
            .envs(clis.envs())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let waiting = start();
    clis.signal("go").unwrap();
    let went = waiting.wait_with_output().unwrap();
    assert_eq!(went.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&went.stdout).contains("went"));

    std::fs::remove_file(argonauts_testkit::fake_clis::signal_path(clis.log_path(), "go").unwrap())
        .unwrap();
    let mut waiting = start();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        waiting.try_wait().unwrap().is_none(),
        "it waits for the signal"
    );
    std::fs::remove_dir_all(&fakes).unwrap();
    let until = argonauts_testkit::deadline::after(std::time::Duration::from_secs(5));
    let status = loop {
        if let Some(status) = waiting.try_wait().unwrap() {
            break status;
        }
        assert!(std::time::Instant::now() < until, "the fake waited on");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(!status.success());
    let mut said = String::new();
    std::io::Read::read_to_string(&mut waiting.stderr.take().unwrap(), &mut said).unwrap();
    assert!(
        said.contains("the test that would signal go has ended"),
        "{said}"
    );
}
