mod common;

use std::path::Path;
use std::sync::Arc;

use argonauts_adapter_manifest::{
    manifest_adapter, manifest_adapter_with, manifest_adapter_with_templates,
};
use argonauts_core::adapter::Transport;
use argonauts_core::limits::QuotaReading;
use argonauts_core::registry::HarnessManifest;
use argonauts_core::registry::harness::{
    ErrorPattern, FinalJsonPointers, HomeKeep, HomeTemplate, OutputKind, PromptChannel, SchemaFlag,
    StopWaits,
};
use argonauts_core::session::SessionRole;
use argonauts_core::settings::Span;
use argonauts_core::{
    Adapter, AgentEvent, CommandSpec, ErrorCategory, SessionRequest, StopPlan, StreamParser,
};
use common::{fixture, run, scratch};
use serde_json::json;

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| (*text).to_owned()).collect()
}

fn jsonl_plugin() -> HarnessManifest {
    HarnessManifest {
        id: "jsonl-plugin".into(),
        display_name: "JSONL plugin".into(),
        binary: fixture("jsonl-plugin.sh"),
        version_args: strings(&["--version"]),
        tested_versions: strings(&["1.0.0"]),
        read_only_args: strings(&["--model", "{model}", "--effort", "{effort}"]),
        build_args: strings(&["--model", "{model}", "--write", "out.txt"]),
        resume_args: strings(&["--resume", "{session}"]),
        family: "mine".into(),
        env: strings(&["OPENAI_API_KEY"]),
        errors: vec![ErrorPattern {
            pattern: "credentials or agent init failed".into(),
            category: ErrorCategory::Auth,
            retryable: false,
        }],
        env_set: [("PLUGIN_NO_UPDATE".to_owned(), "1".to_owned())].into(),
        ..HarnessManifest::default()
    }
}

fn request(workdir: &Path, role: SessionRole) -> SessionRequest {
    SessionRequest {
        workdir: workdir.to_path_buf(),
        prompt: "hello".into(),
        model: Some("m-1".into()),
        role,
        ..SessionRequest::default()
    }
}

fn is_result(event: &AgentEvent, text: Option<&str>, failed: bool) -> bool {
    matches!(event, AgentEvent::Result { text: got, is_error, .. }
        if got.as_deref() == text && *is_error == failed)
}

#[test]
fn manifest_adapter_runs_jsonl_plugin() {
    let adapter = manifest_adapter(jsonl_plugin());
    assert_eq!(adapter.id(), "jsonl-plugin");
    assert_eq!(adapter.display_name(), "JSONL plugin");
    assert_eq!(adapter.version_args(), ["--version"]);
    assert_eq!(adapter.tested_versions(), ["1.0.0"]);
    assert_eq!(adapter.default_family(), "mine");
    assert_eq!(adapter.transport(), Transport::Lines);
    let work = tempfile::tempdir().unwrap();

    // A build session: the prompt on stdin, the model filled in, a file written.
    let spec = adapter
        .command(&request(work.path(), SessionRole::Build))
        .unwrap();
    assert_eq!(spec.args, ["--model", "m-1", "--write", "out.txt"]);
    assert_eq!(spec.stdin.as_deref(), Some("hello"));
    assert_eq!(spec.cwd, work.path());
    assert!(
        spec.env_set
            .contains(&("PLUGIN_NO_UPDATE".into(), "1".into()))
    );
    assert!(spec.env_remove.contains(&"ANTHROPIC_API_KEY".to_owned()));
    assert!(!spec.env_remove.contains(&"OPENAI_API_KEY".to_owned()));
    let run = run(adapter.as_ref(), &spec);
    assert_eq!(run.exit_code, Some(0));
    assert_eq!(
        run.events[..2],
        [
            AgentEvent::Init {
                session_id: Some("plugin-session-1".into()),
                model: Some("m-1".into()),
                tools: strings(&["read", "write"]),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            },
            AgentEvent::Message {
                text: "Read 5 bytes of prompt.".into(),
                parent: None
            },
        ]
    );
    assert!(run.events.contains(&AgentEvent::FileChanged {
        path: "out.txt".into(),
        kind: "add".into()
    }));
    assert!(run.events.iter().any(|event| matches!(
        event,
        AgentEvent::Usage {
            input_tokens: 5,
            output_tokens: 7,
            ..
        }
    )));
    assert!(is_result(run.events.last().unwrap(), Some("done"), false));
    assert_eq!(run.events.len(), 7, "{:?}", run.events);
    assert!(work.path().join("out.txt").exists());

    // Read-only without an effort drops `--effort` with its value; a resume adds its args.
    let mut req = request(work.path(), SessionRole::ReadOnly);
    req.resume_session = Some("s-9".into());
    req.extra_args = strings(&["--noise"]);
    let spec = adapter.command(&req).unwrap();
    assert_eq!(spec.args, ["--model", "m-1", "--resume", "s-9", "--noise"]);
    let run = common::run(adapter.as_ref(), &spec);
    assert!(matches!(&run.events[0],
        AgentEvent::Init { session_id: Some(id), .. } if id == "s-9"));
    assert_eq!(
        run.events[1],
        AgentEvent::Unknown {
            raw: json!("plugin: warming up")
        }
    );

    // A failure on stderr becomes the manifest's category, then the session closes.
    let mut req = request(work.path(), SessionRole::ReadOnly);
    req.extra_args = strings(&["--fail"]);
    let run = common::run(adapter.as_ref(), &adapter.command(&req).unwrap());
    assert_eq!(run.exit_code, Some(3));
    assert_eq!(
        run.events,
        [
            AgentEvent::Error {
                category: ErrorCategory::Auth,
                message: "error: credentials or agent init failed".into(),
                retryable: false,
            },
            AgentEvent::Result {
                text: None,
                structured: None,
                is_error: true
            },
        ]
    );
}

#[test]
fn a_plugin_scripts_its_own_events() {
    let work = tempfile::tempdir().unwrap();
    let script = work.path().join("events.jsonl");
    let line = json!({"type": "quota", "reading": reading("claude-code")}).to_string();
    std::fs::write(&script, format!("{line}\n")).unwrap();
    let adapter = manifest_adapter(jsonl_plugin());
    let mut req = request(work.path(), SessionRole::ReadOnly);
    req.extra_args = vec!["--events".into(), script.to_string_lossy().into_owned()];
    let run = run(adapter.as_ref(), &adapter.command(&req).unwrap());
    // A plugin can't file readings under another vendor.
    assert!(matches!(&run.events[1],
        AgentEvent::Quota { reading } if reading.vendor.0 == "jsonl-plugin"));
    assert!(is_result(run.events.last().unwrap(), None, false));
}

fn final_json_cli() -> HarnessManifest {
    HarnessManifest {
        id: "one-shot".into(),
        display_name: "One-shot CLI".into(),
        binary: fixture("final-json-plugin.sh"),
        read_only_args: strings(&["--model", "{model}", "--cwd", "{workdir}"]),
        build_args: strings(&[
            "--model",
            "{model}",
            "--cwd",
            "{workdir}",
            "--write",
            "x.txt",
        ]),
        family: "from-model".into(),
        output: OutputKind::FinalJson,
        schema: Some(SchemaFlag {
            flag: "--json-schema".into(),
            field: "structured_output".into(),
        }),
        exit_codes: [("2".to_owned(), ErrorCategory::Timeout)].into(),
        final_json: Some(FinalJsonPointers {
            text: Some("/final".into()),
            status: Some("/status".into()),
            ok_status: strings(&["ok"]),
            session_id: Some("/sessionId".into()),
            model: Some("/model".into()),
            input_tokens: Some("/usage/input".into()),
            output_tokens: Some("/usage/output".into()),
            cached_tokens: Some("/usage/cached".into()),
            cost_usd: Some("/costUsd".into()),
            error_kind: Some("/error/kind".into()),
            error_message: Some("/error/message".into()),
        }),
        ..HarnessManifest::default()
    }
}

#[test]
fn manifest_final_json_pointers() {
    let root = tempfile::tempdir().unwrap();
    let work = scratch(root.path(), "work");
    let session = scratch(root.path(), "session");
    let adapter = manifest_adapter(final_json_cli());
    assert!(!adapter.capabilities().live_usage);
    assert!(adapter.capabilities().reports_cost);

    let mut req = request(&work, SessionRole::Build);
    req.json_schema = Some(json!({"type": "object"}));
    req.scratch_dir = Some(session.clone());
    let spec = adapter.command(&req).unwrap();
    let schema_file = session.join("schema.json").to_string_lossy().into_owned();
    let workdir = work.to_string_lossy().into_owned();
    assert_eq!(
        spec.args,
        [
            "--model",
            "m-1",
            "--cwd",
            &workdir,
            "--write",
            "x.txt",
            "--json-schema",
            &schema_file
        ]
    );
    let run = run(adapter.as_ref(), &spec);
    assert_eq!(
        run.events,
        [
            AgentEvent::Init {
                session_id: Some("exec-1".into()),
                model: Some("m-1".into()),
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            },
            AgentEvent::Usage {
                input_tokens: 5,
                output_tokens: 9,
                cached_tokens: 4,
                cost_usd: Some(0.0125),
                cumulative: true,
            },
            AgentEvent::Result {
                text: Some(format!("Read 5 bytes in {workdir}.")),
                structured: Some(json!({"summary": "done"})),
                is_error: false,
            },
        ]
    );
    assert!(work.join("x.txt").exists());

    // Without a schema, neither the file nor the flag. The error's kind names a category,
    // and the mapped exit code adds its own.
    let mut req = request(&work, SessionRole::ReadOnly);
    req.extra_args = strings(&["--fail"]);
    let spec = adapter.command(&req).unwrap();
    assert!(spec.files.is_empty());
    assert!(!spec.args.contains(&"--json-schema".to_owned()));
    let run = common::run(adapter.as_ref(), &spec);
    assert_eq!(run.exit_code, Some(2));
    assert_eq!(
        run.events[1],
        AgentEvent::Error {
            category: ErrorCategory::Timeout,
            message: "the run took too long".into(),
            retryable: false,
        }
    );
    assert!(matches!(&run.events[2],
        AgentEvent::Error { category: ErrorCategory::Timeout, message, .. }
            if message == "one-shot exited with code 2"));
    assert!(is_result(&run.events[3], None, true));
}

#[test]
fn final_json_without_a_document_fails_plainly() {
    let adapter = manifest_adapter(final_json_cli());
    let mut parser = adapter.parser();
    parser.parse_stdout_line("not json at all");
    let events = parser.finish(Some(0));
    assert!(matches!(&events[0],
        AgentEvent::Error { category: ErrorCategory::Cli, message, .. }
            if message == "one-shot didn't print a JSON document"));
    assert!(is_result(&events[1], None, true));

    // A status outside `ok_status` is a failure even with a clean exit.
    let mut parser = adapter.parser();
    parser.parse_stdout_line(r#"{"status":"partial","final":"half"}"#);
    let events = parser.finish(Some(0));
    assert!(matches!(&events[0],
        AgentEvent::Error { message, .. } if message == "one-shot finished with status partial"));
    assert!(is_result(&events[1], Some("half"), true));
}

fn reading(vendor: &str) -> QuotaReading {
    serde_json::from_value(json!({
        "vendor": vendor,
        "status": "warning",
        "used": 0.9,
        "resets_at": null,
        "window": "five_hour",
        "source": "rate_limit_event",
        "observed_at": "2026-09-29T12:00:00Z",
        "detail": null,
    }))
    .unwrap()
}

/// Stands in for a built-in adapter: `quota` lines become readings under its own vendor.
struct Builtin;

struct BuiltinParser;

impl StreamParser for BuiltinParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        match line {
            "quota" => vec![AgentEvent::Quota {
                reading: reading("claude-code"),
            }],
            text => vec![AgentEvent::Message {
                text: text.into(),
                parent: None,
            }],
        }
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::Error {
            category: ErrorCategory::Cli,
            message: line.into(),
            retryable: false,
        }]
    }

    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        vec![AgentEvent::Result {
            text: Some("built-in".into()),
            structured: None,
            is_error: false,
        }]
    }
}

impl Adapter for Builtin {
    fn id(&self) -> &'static str {
        "claude-code"
    }
    fn display_name(&self) -> &'static str {
        "Built-in"
    }
    fn binary(&self) -> &'static str {
        "builtin"
    }
    fn command(&self, _req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        anyhow::bail!("not launched in this test")
    }
    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(BuiltinParser)
    }
    fn version_args(&self) -> &'static [&'static str] {
        &[]
    }
    fn parse_version(&self, _output: &str) -> Option<String> {
        None
    }
    fn tested_versions(&self) -> &'static [&'static str] {
        &[]
    }
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("haiku", None)
    }
    fn stop_plan(&self) -> StopPlan {
        StopPlan::default()
    }
}

fn borrowed_stream() -> HarnessManifest {
    HarnessManifest {
        id: "other-cli".into(),
        display_name: "Other CLI".into(),
        binary: "/opt/other/bin/other".into(),
        read_only_args: strings(&["-p", "{prompt}", "--model", "{model}"]),
        build_args: strings(&["-p", "{prompt}"]),
        family: "other".into(),
        prompt: PromptChannel::Argument,
        output: OutputKind::Parser,
        parser: Some("claude-code".into()),
        errors: vec![ErrorPattern {
            pattern: "overloaded".into(),
            category: ErrorCategory::Overloaded,
            retryable: true,
        }],
        ..HarnessManifest::default()
    }
}

#[test]
fn manifest_reuses_builtin_parser() {
    let builtins: Vec<Arc<dyn Adapter>> = vec![Arc::new(Builtin)];
    let adapter = manifest_adapter_with(borrowed_stream(), &builtins);
    let work = tempfile::tempdir().unwrap();
    let spec = adapter
        .command(&request(work.path(), SessionRole::ReadOnly))
        .unwrap();
    assert_eq!(spec.args, ["-p", "hello", "--model", "m-1"]);
    assert_eq!(spec.stdin, None);

    let mut parser = adapter.parser();
    assert_eq!(
        parser.parse_stdout_line("hi"),
        [AgentEvent::Message {
            text: "hi".into(),
            parent: None
        }]
    );
    // The reading lands on this harness's board, not the built-in's.
    assert_eq!(
        parser.parse_stdout_line("quota"),
        [AgentEvent::Quota {
            reading: reading("other-cli")
        }]
    );
    // The built-in's own error is recategorized by the manifest's pattern, and not doubled.
    assert_eq!(
        parser.parse_stderr_line("server overloaded"),
        [AgentEvent::Error {
            category: ErrorCategory::Overloaded,
            message: "server overloaded".into(),
            retryable: true,
        }]
    );
    assert!(is_result(
        &parser.finish(Some(0))[0],
        Some("built-in"),
        false
    ));

    // Without the built-in, the adapter won't launch.
    let alone = manifest_adapter(borrowed_stream());
    let err = alone
        .command(&request(work.path(), SessionRole::ReadOnly))
        .unwrap_err();
    assert!(err.to_string().contains("claude-code parser"), "{err}");
}

#[test]
fn manifest_home_template_per_session() {
    let root = tempfile::tempdir().unwrap();
    let templates = scratch(root.path(), "harnesses");
    let template = scratch(&templates, "agent");
    std::fs::create_dir_all(template.join("skills")).unwrap();
    std::fs::write(template.join("config.yaml"), "memory: false\n").unwrap();
    std::fs::write(template.join("skills/README.md"), "none\n").unwrap();
    let manifest = HarnessManifest {
        read_only_args: strings(&["--config", "{home}/config.yaml"]),
        home: Some(HomeTemplate {
            env: "AGENT_HOME".into(),
            template: "agent".into(),
            keep: HomeKeep::Session,
        }),
        ..jsonl_plugin()
    };
    let adapter = manifest_adapter_with_templates(manifest.clone(), &[], templates.clone());
    let work = scratch(root.path(), "work");

    let mut homes = Vec::new();
    for name in ["s1", "s2"] {
        let session = scratch(root.path(), name);
        let mut req = request(&work, SessionRole::ReadOnly);
        req.scratch_dir = Some(session.clone());
        let spec = adapter.command(&req).unwrap();
        let home = session.join("home");
        let paths: Vec<_> = spec.files.iter().map(|file| file.path.clone()).collect();
        assert_eq!(
            paths,
            [home.join("config.yaml"), home.join("skills/README.md")]
        );
        let home_text = home.to_string_lossy().into_owned();
        assert!(
            spec.env_set
                .contains(&("AGENT_HOME".into(), home_text.clone()))
        );
        assert_eq!(spec.args[1], format!("{home_text}/config.yaml"));
        common::write_files(&spec);
        homes.push(home);
    }
    assert_ne!(homes[0], homes[1]);
    // Each session's home is its own copy: a change in one reaches neither the other nor the
    // template.
    std::fs::write(homes[0].join("config.yaml"), "memory: true\n").unwrap();
    let read = |path: &Path| std::fs::read_to_string(path).unwrap();
    assert_eq!(read(&homes[1].join("config.yaml")), "memory: false\n");
    assert_eq!(read(&template.join("config.yaml")), "memory: false\n");

    // No session folder, no home.
    let err = adapter
        .command(&request(&work, SessionRole::ReadOnly))
        .unwrap_err();
    assert!(err.to_string().contains("session folder"), "{err}");

    // A template name can't leave the templates folder.
    for name in ["../agent", "a/b", ".."] {
        let mut escaping = manifest.clone();
        if let Some(home) = &mut escaping.home {
            home.template = name.into();
        }
        let adapter = manifest_adapter_with_templates(escaping, &[], templates.clone());
        let mut req = request(&work, SessionRole::ReadOnly);
        req.scratch_dir = Some(root.path().join("s3"));
        let err = adapter.command(&req).unwrap_err();
        assert!(err.to_string().contains("one folder name"), "{name}: {err}");
    }
}

#[test]
fn the_prompt_channel_is_honored() {
    let root = tempfile::tempdir().unwrap();
    let session = scratch(root.path(), "session");
    let by_file = HarnessManifest {
        prompt: PromptChannel::File,
        read_only_args: strings(&["--message-file", "{prompt_file}"]),
        ..jsonl_plugin()
    };
    let mut req = request(root.path(), SessionRole::ReadOnly);
    req.scratch_dir = Some(session.clone());
    let spec = manifest_adapter(by_file).command(&req).unwrap();
    let prompt = session.join("prompt.md");
    assert_eq!(spec.args, ["--message-file", &*prompt.to_string_lossy()]);
    assert_eq!(spec.files[0].path, prompt);
    assert_eq!(spec.files[0].contents, "hello");
    assert_eq!(spec.stdin, None);

    // `{prompt}` is refused unless the manifest chose the argument channel.
    let sneaky = HarnessManifest {
        read_only_args: strings(&["-p", "{prompt}"]),
        ..jsonl_plugin()
    };
    let err = manifest_adapter(sneaky).command(&req).unwrap_err();
    assert!(err.to_string().contains("prompt = \"argument\""), "{err}");

    let unknown = HarnessManifest {
        read_only_args: strings(&["--dir", "{worktree}"]),
        ..jsonl_plugin()
    };
    let err = manifest_adapter(unknown).command(&req).unwrap_err();
    assert!(err.to_string().contains("{worktree}"), "{err}");
}

#[test]
fn launches_the_manifest_refuses() {
    let work = tempfile::tempdir().unwrap();
    let mut req = request(work.path(), SessionRole::ReadOnly);
    req.resume_session = Some("s-1".into());
    let no_resume = HarnessManifest {
        resume_args: Vec::new(),
        ..jsonl_plugin()
    };
    let err = manifest_adapter(no_resume).command(&req).unwrap_err();
    assert!(err.to_string().contains("can't resume"), "{err}");

    let acp = HarnessManifest {
        transport: Transport::Acp,
        ..jsonl_plugin()
    };
    let err = manifest_adapter(acp)
        .command(&request(work.path(), SessionRole::ReadOnly))
        .unwrap_err();
    assert!(err.to_string().contains("ACP adapter"), "{err}");

    let bad_regex = HarnessManifest {
        errors: vec![ErrorPattern {
            pattern: "(unclosed".into(),
            category: ErrorCategory::Auth,
            retryable: false,
        }],
        ..jsonl_plugin()
    };
    let err = manifest_adapter(bad_regex)
        .command(&request(work.path(), SessionRole::ReadOnly))
        .unwrap_err();
    assert!(format!("{err:#}").contains("(unclosed"), "{err:#}");
}

#[test]
fn take_over_and_stop_come_from_the_manifest() {
    let manifest = HarnessManifest {
        interactive_resume: strings(&["--resume", "{session}", "--cwd", "{workdir}"]),
        stop: Some(StopWaits {
            after_interrupt: Span::new("2s"),
            after_terminate: Span::new("none"),
        }),
        ..jsonl_plugin()
    };
    let adapter = manifest_adapter(manifest);
    let spec = adapter
        .interactive_resume("s-7", Path::new("/work"))
        .unwrap();
    assert_eq!(spec.args, ["--resume", "s-7", "--cwd", "/work"]);
    assert_eq!(spec.cwd, Path::new("/work"));
    let plan = adapter.stop_plan();
    assert_eq!(plan.steps[0].wait, std::time::Duration::from_secs(2));
    assert_eq!(plan.steps[1].wait, StopPlan::default().steps[1].wait);

    assert!(
        manifest_adapter(jsonl_plugin())
            .interactive_resume("s-7", Path::new("/work"))
            .is_none()
    );
}

#[test]
fn a_harness_without_a_schema_flag_gets_the_schema_in_its_prompt() {
    let root = tempfile::tempdir().unwrap();
    let work = scratch(root.path(), "work");
    let session = scratch(root.path(), "session");
    let mut req = request(&work, SessionRole::Build);
    req.json_schema = Some(json!({"type": "object", "required": ["status"]}));
    req.scratch_dir = Some(session.clone());

    let on_stdin = manifest_adapter(jsonl_plugin()).command(&req).unwrap();
    let stdin = on_stdin.stdin.unwrap();
    assert!(
        stdin.starts_with("hello\n\nEnd your final message with your report"),
        "{stdin}"
    );
    assert!(stdin.contains("\"status\""), "{stdin}");
    assert!(
        on_stdin.files.is_empty(),
        "no schema file for a CLI that can't take one"
    );

    let in_a_file = HarnessManifest {
        prompt: PromptChannel::File,
        ..jsonl_plugin()
    };
    let spec = manifest_adapter(in_a_file).command(&req).unwrap();
    let prompt = spec
        .files
        .iter()
        .find(|file| file.path == session.join("prompt.md"))
        .unwrap();
    assert!(
        prompt.contents.contains("\"status\""),
        "{}",
        prompt.contents
    );
    assert_eq!(spec.stdin, None);

    let as_an_argument = HarnessManifest {
        prompt: PromptChannel::Argument,
        read_only_args: strings(&["--ask", "{prompt}"]),
        ..jsonl_plugin()
    };
    req.role = SessionRole::ReadOnly;
    let spec = manifest_adapter(as_an_argument).command(&req).unwrap();
    assert!(spec.args[1].contains("\"status\""), "{:?}", spec.args);

    // One that holds the run to the schema itself gets the prompt alone.
    req.role = SessionRole::Build;
    let spec = manifest_adapter(final_json_cli()).command(&req).unwrap();
    assert_eq!(spec.stdin.as_deref(), Some("hello"));
}
