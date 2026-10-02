use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use argonauts_testkit::scenario::{
    FakeRule, FakeScenario, FakeStep, LogLine, NO_RULE_EXIT, StdinPrompt,
};
use argonauts_testkit::{FakeVendors, TempRepo};

const FAKE: &str = env!("CARGO_BIN_EXE_argonauts-fake-vendor");

fn install(repo: &TempRepo, scenario: FakeScenario) -> FakeVendors {
    let path = repo.scratch().join("scenario.toml");
    scenario.save(&path).unwrap();
    FakeVendors::install_with(Path::new(FAKE), repo.scratch(), &path).unwrap()
}

fn run(
    vendors: &FakeVendors,
    repo: &TempRepo,
    program: &str,
    args: &[&str],
    stdin: &str,
) -> (i32, String, String) {
    let mut child = Command::new(vendors.bin_dir().join(program))
        .args(args)
        .current_dir(repo.root())
        .envs(vendors.envs())
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
    let vendors = install(&repo, scenario);

    let (code, stdout, stderr) = run(
        &vendors,
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

    let (code, stdout, _) = run(&vendors, &repo, "claude", &["--version"], "");
    assert_eq!((code, stdout.trim()), (0, "2.1.284 (Claude Code)"));

    let (code, _, stderr) = run(&vendors, &repo, "agy", &["-p", "x"], "");
    assert_eq!(code, NO_RULE_EXIT);
    assert!(stderr.contains("no rule matched agy"));

    let invocations = vendors.invocations().unwrap();
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
    let vendors = install(&repo, scenario);
    let (code, stdout, _) = run(
        &vendors,
        &repo,
        "claude",
        &["--input-format", "stream-json"],
        "first\nsecond\nplease wrap up now\n",
    );
    assert_eq!((code, stdout.as_str()), (0, "done\n"));
    let stdin_lines: Vec<String> = vendors
        .log()
        .unwrap()
        .into_iter()
        .filter_map(|line| match line {
            LogLine::StdinLine { line, .. } => Some(line),
            _ => None,
        })
        .collect();
    assert_eq!(stdin_lines, ["first", "second", "please wrap up now"]);

    let (code, _, stderr) = run(&vendors, &repo, "agy", &[], "");
    assert_ne!(code, 0);
    assert!(stderr.contains("outside the working directory"), "{stderr}");
    assert!(!repo.scratch().join("escape.txt").exists());
}

#[test]
fn answers_help_with_the_flags_the_doctor_looks_for() {
    let repo = TempRepo::new().unwrap();
    let vendors = install(&repo, FakeScenario::default());
    let (code, help, _) = run(&vendors, &repo, "codex", &["exec", "--help"], "");
    assert_eq!(code, 0);
    for flag in ["--json", "--output-schema", "--skip-git-repo-check"] {
        assert!(help.contains(flag), "{flag} in {help}");
    }
    let (code, help, _) = run(&vendors, &repo, "claude", &["--help"], "");
    assert_eq!(code, 0);
    assert!(help.contains("--output-format"), "{help}");
    let (code, _, _) = run(&vendors, &repo, "claude", &["-p", "--help"], "");
    assert_eq!(code, NO_RULE_EXIT);
}
